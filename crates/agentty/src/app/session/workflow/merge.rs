//! Merge, rebase, and cleanup workflows for session branches.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use askama::Template;
use tokio::sync::mpsc;
use tracing::warn;

use super::published_branch::{self, PublishedBranchAutoPushInput};
use super::worker::SessionCommand;
use super::{SessionTaskService, session_branch};
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError};
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{PublishedBranchSyncStatus, SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::agent;
use crate::infra::agent::protocol::AgentResponseSummary;
use crate::infra::db::AppRepositories;
use crate::infra::fs::{self as fs, FsClient};
use crate::infra::git::{self as git, GitClient};

const REBASE_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 3,
    // Allow up to 3 consecutive identical-content observations before
    // giving up, so the agent gets a genuine second chance when partial
    // progress is made inside a file without fully clearing all markers.
    max_identical_failure_streak: 3,
};

/// Coordinates merge/rebase session workflows behind a dedicated service
/// boundary.
pub(crate) struct SessionMergeService;

/// Askama view model for rendering rebase conflict-assistance prompts.
#[derive(Template)]
#[template(path = "rebase_assist_prompt.md", escape = "none")]
struct RebaseAssistPromptTemplate<'a> {
    base_branch: &'a str,
    conflicted_files: &'a str,
}

/// Boxed async result used by sync conflict assistance boundary methods.
type SyncAssistFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Boxed async result used by existing-session rebase assistance.
pub(super) type RebaseAssistFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Executes one rebase-conflict prompt inside the already-active session
/// channel.
pub(super) trait ExistingSessionRebaseAssistClient: Send + Sync {
    /// Resolves the current rebase conflict files with the provided prompt.
    fn resolve_rebase_conflicts(
        &self,
        prompt: String,
    ) -> RebaseAssistFuture<Result<(), SessionError>>;
}

/// Selects how session rebase conflicts are submitted to an agent.
#[derive(Clone)]
pub(super) enum RebaseAssistMode {
    /// Submit through the session worker's existing provider conversation.
    ExistingSession(Arc<dyn ExistingSessionRebaseAssistClient>),
    /// Submit through the historical isolated one-shot utility prompt.
    OneShot,
}

/// Shared context needed to restore a failed merge-start attempt back to
/// `Review`.
struct MergeStartRestoreContext<'a> {
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    clock: &'a dyn Clock,
    db: &'a AppRepositories,
    session_id: &'a str,
    session_update_versions: &'a SessionUpdateVersionMap,
    status: &'a Arc<Mutex<Status>>,
}

struct MergeTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    child_pid: Arc<Mutex<Option<u32>>>,
    clock: Arc<dyn Clock>,
    db: AppRepositories,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    id: SessionId,
    output: Arc<Mutex<String>>,
    repo_root: PathBuf,
    session_model: AgentModel,
    session_update_versions: SessionUpdateVersionMap,
    source_branch: String,
    status: Arc<Mutex<Status>>,
}

#[derive(Clone)]
struct RebaseAssistInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    assist_mode: RebaseAssistMode,
    child_pid: Arc<Mutex<Option<u32>>>,
    db: AppRepositories,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    id: SessionId,
    output: Arc<Mutex<String>>,
    rebase_target: String,
    session_model: AgentModel,
    session_update_versions: SessionUpdateVersionMap,
}

/// Input required to run a session rebase command to completion.
pub(super) struct RebaseCommandInput {
    /// Reducer event sender used for status/output updates.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Agent submission mode used when rebase conflicts need edits.
    pub(super) assist_mode: RebaseAssistMode,
    /// Stored base branch for the session worktree.
    pub(super) base_branch: String,
    /// Shared child-process PID slot used by cancellation.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    /// Clock used for persisted status timing.
    pub(super) clock: Arc<dyn Clock>,
    /// Repository bundle used by workflow persistence.
    pub(super) db: AppRepositories,
    /// Session worktree folder where git commands run.
    pub(super) folder: PathBuf,
    /// Filesystem boundary used for conflict fingerprinting.
    pub(super) fs_client: Arc<dyn FsClient>,
    /// Git boundary used for rebase commands.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Session identifier receiving output/status updates.
    pub(super) id: SessionId,
    /// Shared transcript buffer mirrored to persistence and UI.
    pub(super) output: Arc<Mutex<String>>,
    /// Model used by agent-assisted conflict prompts.
    pub(super) session_model: AgentModel,
    /// Per-app update versions for targeted session refresh events.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Shared session lifecycle status.
    pub(super) status: Arc<Mutex<Status>>,
}

/// Bundled context for finalizing one rebase task.
struct FinalizeRebaseInput<'a> {
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    clock: &'a dyn Clock,
    db: &'a AppRepositories,
    folder: &'a Path,
    git_client: &'a Arc<dyn GitClient>,
    id: &'a str,
    output: &'a Arc<Mutex<String>>,
    rebase_result: Result<String, SessionError>,
    session_update_versions: &'a SessionUpdateVersionMap,
    status: &'a Arc<Mutex<Status>>,
}

/// Bundled context for finalizing one merge task.
struct FinalizeMergeInput<'a> {
    clock: &'a dyn Clock,
    db: &'a AppRepositories,
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    id: &'a str,
    output: &'a Arc<Mutex<String>>,
    result: Result<String, SessionError>,
    session_update_versions: &'a SessionUpdateVersionMap,
    status: &'a Arc<Mutex<Status>>,
}

/// Input context for assisted conflict resolution during `sync main`.
struct SyncRebaseAssistInput {
    base_branch: String,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    session_model: AgentModel,
    sync_assist_client: Arc<dyn SyncAssistClient>,
}

/// Polymorphic input for shared assisted rebase loop orchestration.
enum RebaseAssistLoopInput {
    Session(RebaseAssistInput),
    Project(SyncRebaseAssistInput),
}

impl RebaseAssistLoopInput {
    /// Returns worktree path used for conflict fingerprinting.
    fn folder(&self) -> &Path {
        match self {
            Self::Session(input) => &input.folder,
            Self::Project(input) => &input.folder,
        }
    }

    /// Returns the filesystem boundary used by assist fingerprinting.
    fn fs_client(&self) -> &dyn FsClient {
        match self {
            Self::Session(input) => input.fs_client.as_ref(),
            Self::Project(input) => input.fs_client.as_ref(),
        }
    }

    /// Builds conflict-state no-progress error text for the active workflow.
    fn repeated_conflict_state_error(&self, detail: &str) -> String {
        match self {
            Self::Session(_) => format!(
                "Rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {detail}"
            ),
            Self::Project(_) => format!(
                "Sync rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {detail}"
            ),
        }
    }

    /// Builds unchanged-conflict-files no-progress error text for the active
    /// workflow.
    fn unchanged_conflict_files_error(&self) -> String {
        match self {
            Self::Session(_) => {
                "Rebase assistance made no progress: conflicted files did not change".to_string()
            }
            Self::Project(_) => "Sync rebase assistance made no progress: conflicted files did \
                                 not change"
                .to_string(),
        }
    }

    /// Builds unresolved-conflict-after-assistance error text for the active
    /// workflow.
    fn still_conflicted_error(&self, detail: &str) -> String {
        match self {
            Self::Session(_) => format!("Rebase still has conflicts after assistance: {detail}"),
            Self::Project(_) => {
                format!("Sync rebase still has conflicts after assistance: {detail}")
            }
        }
    }

    /// Returns final exhausted-attempts error text for the active workflow.
    fn exhausted_error(&self) -> String {
        match self {
            Self::Session(_) => "Failed to complete assisted rebase".to_string(),
            Self::Project(_) => "Failed to complete assisted sync rebase".to_string(),
        }
    }

    /// Loads conflicted files for the active workflow.
    ///
    /// # Errors
    /// Returns an error if conflicted-file inspection fails.
    async fn load_conflicted_files(
        &self,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, SessionError> {
        match self {
            Self::Session(input) => {
                SessionManager::load_conflicted_files(input, previous_conflict_files).await
            }
            Self::Project(input) => {
                SessionManager::load_sync_conflicted_files(input, previous_conflict_files).await
            }
        }
    }

    /// Executes one assistance attempt for the active workflow.
    ///
    /// # Errors
    /// Returns an error when assistance command execution fails.
    async fn run_assist_attempt(
        &self,
        assist_attempt: usize,
        conflicted_files: &[String],
    ) -> Result<(), SessionError> {
        match self {
            Self::Session(input) => {
                SessionManager::append_rebase_assist_header(
                    input,
                    assist_attempt,
                    conflicted_files,
                )
                .await;
                SessionManager::run_rebase_assist_agent(input, conflicted_files).await
            }
            Self::Project(input) => {
                SessionManager::run_sync_rebase_assist_agent(input, conflicted_files).await
            }
        }
    }

    /// Stages edits and checks whether conflicts remain for the active
    /// workflow.
    ///
    /// # Errors
    /// Returns an error when staging or conflict checks fail.
    async fn stage_and_check_for_conflicts(
        &self,
        conflict_files: &[String],
    ) -> Result<bool, SessionError> {
        match self {
            Self::Session(input) => {
                SessionManager::stage_and_check_for_conflicts(input, conflict_files).await
            }
            Self::Project(input) => {
                SessionManager::stage_and_check_for_sync_conflicts(input, conflict_files).await
            }
        }
    }

    /// Continues in-progress rebase for the active workflow.
    ///
    /// # Errors
    /// Returns an error when `git rebase --continue` fails with non-conflict
    /// errors.
    async fn run_rebase_continue(&self) -> Result<git::RebaseStepResult, SessionError> {
        match self {
            Self::Session(input) => SessionManager::run_rebase_continue(input).await,
            Self::Project(input) => SessionManager::run_sync_rebase_continue(input).await,
        }
    }

    /// Aborts rebase for the active workflow after assistance failure.
    async fn abort_rebase_after_assist_failure(&self) {
        match self {
            Self::Session(input) => {
                SessionManager::abort_rebase_after_assist_failure(input).await;
            }
            Self::Project(input) => {
                SessionManager::abort_sync_rebase_after_assist_failure(input).await;
            }
        }
    }
}

/// Async boundary for one sync rebase assistance attempt.
#[cfg_attr(test, mockall::automock)]
trait SyncAssistClient: Send + Sync {
    /// Executes one agent-assisted edit attempt for the provided rebase prompt.
    fn resolve_rebase_conflicts(
        &self,
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> SyncAssistFuture<Result<(), SessionError>>;
}

/// Production sync-assistance executor backed by real agent commands.
struct RealSyncAssistClient;

impl RealSyncAssistClient {
    /// Runs one sync conflict assistance command through the shared one-shot
    /// agent submission path.
    ///
    /// # Errors
    /// Returns an error when the one-shot agent command fails.
    async fn run_assist_command(
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> Result<(), SessionError> {
        // Success payload unused; run for side effects only.
        let _ = agent::submit_one_shot(agent::OneShotRequest {
            child_pid: None,
            folder: &folder,
            model: session_model,
            prompt: &prompt,
            request_kind: crate::infra::channel::AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
        })
        .await
        .map_err(SessionError::Workflow)?;

        Ok(())
    }
}

impl SyncAssistClient for RealSyncAssistClient {
    fn resolve_rebase_conflicts(
        &self,
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> SyncAssistFuture<Result<(), SessionError>> {
        Box::pin(async move { Self::run_assist_command(folder, prompt, session_model).await })
    }
}

/// User-facing reasons why repository branch sync cannot be started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SyncSessionStartError {
    /// Sync cannot run while the selected project branch has local
    /// modifications.
    MainHasUncommittedChanges { default_branch: String },
    /// Generic start failure outside sync-specific policy constraints.
    Other(String),
}

/// Summary of one completed main-branch sync run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncMainOutcome {
    /// Commit titles discovered upstream and pulled during sync.
    pub(crate) pulled_commit_titles: Vec<String>,
    /// Number of commits rebased from upstream into the local branch.
    pub(crate) pulled_commits: Option<u32>,
    /// Commit titles discovered locally and pushed during sync.
    pub(crate) pushed_commit_titles: Vec<String>,
    /// Number of local commits pushed to upstream during sync.
    pub(crate) pushed_commits: Option<u32>,
    /// Paths that were conflicted and resolved during assisted sync rebase.
    pub(crate) resolved_conflict_files: Vec<String>,
}

/// Successful assisted rebase completion details.
#[derive(Debug)]
struct RebaseAssistOutcome {
    resolved_conflict_files: Vec<String>,
}

impl RebaseAssistOutcome {
    /// Creates an empty assisted-rebase completion payload.
    fn empty() -> Self {
        Self {
            resolved_conflict_files: Vec::new(),
        }
    }

    /// Adds conflicted file names and keeps the list unique and sorted.
    fn extend_resolved_conflict_files(&mut self, conflict_files: &[String]) {
        for conflict_file in conflict_files {
            if !self.resolved_conflict_files.contains(conflict_file) {
                self.resolved_conflict_files.push(conflict_file.clone());
            }
        }

        self.resolved_conflict_files.sort_unstable();
    }
}

/// Control result for one bounded rebase-assistance attempt.
enum RebaseAssistAttemptOutcome {
    /// The rebase assistance loop should continue with another attempt.
    Continue,
    /// The rebase completed successfully.
    Completed,
}

impl SyncSessionStartError {
    /// Returns user-facing detail for non-popup sync start errors.
    pub(crate) fn detail_message(&self) -> String {
        match self {
            Self::MainHasUncommittedChanges { default_branch } => format!(
                "Sync cannot run while `{default_branch}` has uncommitted changes.\nCommit or \
                 stash changes in `{default_branch}`, then try again."
            ),
            Self::Other(detail) => detail.clone(),
        }
    }
}

impl SessionMergeService {
    /// Starts a squash merge for a review-ready or queued session branch in
    /// the background.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the status transition to `Merging` fails.
    async fn merge_session(
        &self,
        manager: &SessionManager,
        session_id: &str,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<(), SessionError> {
        let session = manager
            .session_or_err(session_id)
            .map_err(|_| SessionError::NotFound)?;
        if !(session.status.allows_review_actions() || session.status == Status::Queued) {
            return Err(SessionError::Workflow(
                "Session must be in review or queued status".to_string(),
            ));
        }
        if !manager.can_mutate_session_branch_in_stack(session_id) {
            return Err(SessionError::Workflow(
                "Stacked branch work can only run when no other stack session is active and \
                 parent branch edits are not blocked by materialized children"
                    .to_string(),
            ));
        }

        let (db, folder, id, session_model) = (
            services.db().clone(),
            session.folder.clone(),
            session.id.clone(),
            session.model,
        );
        let (app_event_tx, clock, fs_client, git_client, session_update_versions) = (
            services.event_sender(),
            services.clock(),
            services.fs_client(),
            manager.git_client(),
            services.session_update_versions(),
        );

        let handles = manager
            .session_handles_or_err(session_id)
            .map_err(|_| SessionError::HandlesNotFound)?;
        let (child_pid, output, status) = (
            Arc::clone(&handles.child_pid),
            Arc::clone(&handles.output),
            Arc::clone(&handles.status),
        );
        let restore_context = MergeStartRestoreContext {
            app_event_tx: &app_event_tx,
            clock: clock.as_ref(),
            db: &db,
            session_id: &id,
            session_update_versions: &session_update_versions,
            status: &status,
        };
        if !SessionTaskService::update_status(
            &status,
            clock.as_ref(),
            &db,
            &app_event_tx,
            &session_update_versions,
            &id,
            Status::Merging,
        )
        .await
        {
            return Err(SessionError::Workflow(
                "Invalid status transition to Merging".to_string(),
            ));
        }

        let base_branch = Self::load_merge_base_branch(&restore_context).await?;
        let repo_root = Self::find_merge_repo_root(
            git_client.as_ref(),
            projects.working_dir().to_path_buf(),
            &restore_context,
        )
        .await?;
        Self::ensure_merge_target_clean(
            git_client.as_ref(),
            repo_root.clone(),
            &base_branch,
            &restore_context,
        )
        .await?;

        let merge_task_input = MergeTaskInput {
            app_event_tx,
            base_branch,
            child_pid,
            clock,
            db,
            folder,
            fs_client,
            git_client,
            id: id.clone(),
            output,
            repo_root,
            session_model,
            session_update_versions,
            source_branch: session_branch(&id),
            status,
        };
        tokio::spawn(async move {
            SessionManager::run_merge_task(merge_task_input).await;
        });

        Ok(())
    }

    /// Restores a failed merge-start attempt to `Review` status without
    /// masking the original error.
    async fn restore_review_status(context: &MergeStartRestoreContext<'_>) {
        if !SessionTaskService::update_status(
            context.status,
            context.clock,
            context.db,
            context.app_event_tx,
            context.session_update_versions,
            context.session_id,
            Status::Review,
        )
        .await
        {
            warn!(
                session_id = context.session_id,
                "skipped restoring review status because the in-memory status was already current"
            );
        }
    }

    /// Loads the persisted base branch for a mergeable session or restores the
    /// session to `Review` when required metadata is missing.
    ///
    /// # Errors
    /// Returns an error when the session lacks a worktree-backed base branch
    /// or when reading the metadata from persistence fails.
    async fn load_merge_base_branch(
        context: &MergeStartRestoreContext<'_>,
    ) -> Result<String, SessionError> {
        match context
            .db
            .sessions()
            .get_session_base_branch(context.session_id)
            .await
        {
            Ok(Some(base_branch)) => Ok(base_branch),
            Ok(None) => {
                Self::restore_review_status(context).await;

                Err(SessionError::Workflow(
                    "No git worktree for this session".to_string(),
                ))
            }
            Err(error) => {
                Self::restore_review_status(context).await;

                Err(SessionError::Db(error))
            }
        }
    }

    /// Resolves the main repository root for a mergeable session or restores
    /// the session to `Review` when the repository cannot be found.
    ///
    /// # Errors
    /// Returns an error when the repository root cannot be discovered.
    async fn find_merge_repo_root(
        git_client: &dyn GitClient,
        working_dir: PathBuf,
        context: &MergeStartRestoreContext<'_>,
    ) -> Result<PathBuf, SessionError> {
        if let Some(repo_root) = git_client.find_git_repo_root(working_dir).await {
            return Ok(repo_root);
        }

        Self::restore_review_status(context).await;

        Err(SessionError::Workflow(
            "Failed to find git repository root".to_string(),
        ))
    }

    /// Verifies the merge target checkout is clean before starting a squash
    /// merge task, restoring the session to `Review` when merge cannot start.
    ///
    /// # Errors
    /// Returns an error when git status cannot be inspected or when the target
    /// checkout has local changes that would make the merge unsafe.
    async fn ensure_merge_target_clean(
        git_client: &dyn GitClient,
        repo_root: PathBuf,
        base_branch: &str,
        context: &MergeStartRestoreContext<'_>,
    ) -> Result<(), SessionError> {
        let is_clean = match git_client.is_worktree_clean(repo_root).await {
            Ok(is_clean) => is_clean,
            Err(error) => {
                Self::restore_review_status(context).await;

                return Err(SessionError::Workflow(format!(
                    "Failed to inspect `{base_branch}` before merge: {error}"
                )));
            }
        };
        if is_clean {
            return Ok(());
        }

        Self::restore_review_status(context).await;

        Err(SessionError::Workflow(format!(
            "Merge cannot run while `{base_branch}` has uncommitted changes.\nCommit or stash \
             changes in `{base_branch}`, then try again."
        )))
    }

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for rebase, required git
    /// metadata is missing, or enqueueing the rebase command fails.
    async fn rebase_session(
        &self,
        manager: &mut SessionManager,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let (base_branch, persisted_session_id) =
            Self::load_rebase_start_context(manager, services, session_id).await?;

        let handles = manager
            .session_handles_or_err(session_id)
            .map_err(|_| SessionError::HandlesNotFound)?;
        let status = Arc::clone(&handles.status);
        let db = services.db().clone();
        let app_event_tx = services.event_sender();
        let clock = services.clock();
        let session_update_versions = services.session_update_versions();

        if !SessionTaskService::update_status(
            &status,
            clock.as_ref(),
            &db,
            &app_event_tx,
            &session_update_versions,
            &persisted_session_id,
            Status::Rebasing,
        )
        .await
        {
            return Err(SessionError::Workflow(
                "Invalid status transition to Rebasing".to_string(),
            ));
        }

        let command = SessionCommand::Rebase {
            base_branch,
            operation_id: uuid::Uuid::new_v4().to_string(),
        };
        if let Err(error) = manager
            .enqueue_session_command(services, &persisted_session_id, command)
            .await
        {
            let _ = SessionTaskService::update_status(
                &status,
                clock.as_ref(),
                &db,
                &app_event_tx,
                &session_update_versions,
                &persisted_session_id,
                Status::Review,
            )
            .await;

            return Err(error);
        }

        Ok(())
    }

    /// Starts automatic rebase commands for review-ready stacked children
    /// after their parent branch receives a completed turn.
    ///
    /// Draft children are skipped because they do not have a materialized
    /// branch yet, and active children are skipped so an existing turn,
    /// question, merge, or sync is never interrupted. All eligible review
    /// children are moved to `Rebasing` before enqueueing so the stack
    /// maintenance fan-out is treated as one automatic parent-turn effect
    /// instead of separate user branch actions.
    async fn rebase_stacked_children_after_parent_turn(
        &self,
        manager: &mut SessionManager,
        services: &AppServices,
        parent_session_id: &str,
    ) -> Vec<(SessionId, SessionError)> {
        let child_session_ids =
            Self::rebaseable_stacked_child_session_ids(manager, parent_session_id);
        let mut failures = Vec::new();
        for child_session_id in child_session_ids {
            if let Err(error) = Self::enqueue_stacked_child_auto_rebase(
                manager,
                services,
                child_session_id.as_str(),
            )
            .await
            {
                failures.push((child_session_id, error));
            }
        }

        failures
    }

    /// Returns the materialized stacked children that should sync after the
    /// parent reaches a review-ready state.
    fn rebaseable_stacked_child_session_ids(
        manager: &SessionManager,
        parent_session_id: &str,
    ) -> Vec<SessionId> {
        let Some(parent_session) = manager
            .sessions()
            .iter()
            .find(|session| session.id.as_str() == parent_session_id)
        else {
            return Vec::new();
        };
        if !parent_session.status.allows_review_actions() {
            return Vec::new();
        }

        manager
            .sessions()
            .iter()
            .filter(|session| {
                session
                    .parent_session_id
                    .as_ref()
                    .is_some_and(|parent_id| parent_id.as_str() == parent_session_id)
                    && session.status.allows_review_actions()
            })
            .map(|session| session.id.clone())
            .collect()
    }

    /// Moves one stacked child into `Rebasing` and enqueues the existing
    /// worker-backed rebase command.
    async fn enqueue_stacked_child_auto_rebase(
        manager: &mut SessionManager,
        services: &AppServices,
        child_session_id: &str,
    ) -> Result<(), SessionError> {
        let (base_branch, previous_status, persisted_session_id) = {
            let session = manager
                .session_or_err(child_session_id)
                .map_err(|_| SessionError::NotFound)?;
            let base_branch = services
                .db()
                .sessions()
                .get_session_base_branch(&session.id)
                .await?
                .ok_or_else(|| {
                    SessionError::Workflow("No git worktree for this session".to_string())
                })?;

            (base_branch, session.status, session.id.clone())
        };

        let handles = manager
            .session_handles_or_err(child_session_id)
            .map_err(|_| SessionError::HandlesNotFound)?;
        let status = Arc::clone(&handles.status);
        let db = services.db().clone();
        let app_event_tx = services.event_sender();
        let clock = services.clock();
        let session_update_versions = services.session_update_versions();

        if !SessionTaskService::update_status(
            &status,
            clock.as_ref(),
            &db,
            &app_event_tx,
            &session_update_versions,
            &persisted_session_id,
            Status::Rebasing,
        )
        .await
        {
            return Err(SessionError::Workflow(
                "Invalid status transition to Rebasing".to_string(),
            ));
        }

        let command = SessionCommand::Rebase {
            base_branch,
            operation_id: uuid::Uuid::new_v4().to_string(),
        };
        if let Err(error) = manager
            .enqueue_session_command(services, &persisted_session_id, command)
            .await
        {
            let _ = SessionTaskService::update_status(
                &status,
                clock.as_ref(),
                &db,
                &app_event_tx,
                &session_update_versions,
                &persisted_session_id,
                previous_status,
            )
            .await;

            return Err(error);
        }

        Ok(())
    }

    /// Loads and validates the immutable inputs needed to enqueue a session
    /// rebase command.
    ///
    /// # Errors
    /// Returns an error when the session cannot be rebased or has no
    /// worktree-backed base branch.
    async fn load_rebase_start_context(
        manager: &SessionManager,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(String, SessionId), SessionError> {
        let session = manager
            .session_or_err(session_id)
            .map_err(|_| SessionError::NotFound)?;
        if !session.status.allows_review_actions() {
            return Err(SessionError::Workflow(
                "Session must be in review status".to_string(),
            ));
        }
        if !manager.can_mutate_session_branch_in_stack(session_id) {
            return Err(SessionError::Workflow(
                "Stacked branch work can only run when no other stack session is active and \
                 parent branch edits are not blocked by materialized children"
                    .to_string(),
            ));
        }

        let base_branch = services
            .db()
            .sessions()
            .get_session_base_branch(&session.id)
            .await?
            .ok_or_else(|| {
                SessionError::Workflow("No git worktree for this session".to_string())
            })?;

        Ok((base_branch, session.id.clone()))
    }
}

impl SessionManager {
    /// Starts a squash merge for a review-ready or queued session branch in
    /// the background.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the status transition to `Merging` fails.
    pub async fn merge_session(
        &self,
        session_id: &str,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<(), SessionError> {
        self.merge_service()
            .merge_session(self, session_id, projects, services)
            .await
    }

    async fn run_merge_task(input: MergeTaskInput) {
        let output = Arc::clone(&input.output);
        let clock = Arc::clone(&input.clock);
        let db = input.db.clone();
        let app_event_tx = input.app_event_tx.clone();
        let id = input.id.clone();
        let status = Arc::clone(&input.status);
        let session_update_versions = input.session_update_versions.clone();

        let merge_result = Self::execute_merge_workflow(input).await;
        Self::finalize_merge_task(FinalizeMergeInput {
            clock: clock.as_ref(),
            db: &db,
            app_event_tx: &app_event_tx,
            id: &id,
            output: &output,
            result: merge_result,
            session_update_versions: &session_update_versions,
            status: &status,
        })
        .await;
    }

    /// Executes the merge workflow for one session branch.
    ///
    /// # Errors
    /// Returns an error when the rebase step fails, the canonical session
    /// commit message cannot be loaded, squash-merge git commands fail, status
    /// transitions are invalid, or worktree cleanup fails.
    async fn execute_merge_workflow(input: MergeTaskInput) -> Result<String, SessionError> {
        let rebase_input = Self::merge_rebase_input(&input);
        let MergeTaskInput {
            app_event_tx,
            base_branch,
            clock,
            db,
            folder,
            fs_client,
            git_client,
            id,
            output: _,
            repo_root,
            source_branch,
            session_update_versions,
            status,
            ..
        } = input;

        // Rebase onto the base branch first to ensure the merge is clean and
        // includes all recent changes. This also handles auto-commit and
        // conflict resolution via the agent.
        if let Err(error) = Self::execute_rebase_workflow(rebase_input).await {
            return Err(SessionError::Workflow(format!(
                "Merge failed during rebase step: {error}"
            )));
        }

        let squash_diff = Self::load_squash_diff(
            git_client.as_ref(),
            repo_root.clone(),
            source_branch.clone(),
            base_branch.clone(),
        )
        .await?;
        let authoritative_commit_message = if squash_diff.trim().is_empty() {
            None
        } else {
            Some(
                Self::load_authoritative_session_commit_message(
                    git_client.as_ref(),
                    folder.clone(),
                )
                .await?,
            )
        };
        let merge_outcome = if let Some(commit_message) = authoritative_commit_message.as_ref() {
            let repo_root = repo_root.clone();
            let source_branch = source_branch.clone();
            let base_branch = base_branch.clone();
            let commit_message = commit_message.clone();

            git_client
                .squash_merge(repo_root, source_branch, base_branch, commit_message)
                .await?
        } else {
            git::SquashMergeOutcome::AlreadyPresentInTarget
        };
        let merged_commit_hash =
            Self::load_merged_commit_hash(git_client.as_ref(), repo_root.clone(), merge_outcome)
                .await?;

        Self::cleanup_merged_session_worktree(
            folder.clone(),
            Arc::clone(&fs_client),
            Arc::clone(&git_client),
            source_branch.clone(),
            Some(repo_root),
        )
        .await
        .map_err(|error| {
            SessionError::Workflow(format!(
                "Merged successfully but failed to remove worktree: {error}"
            ))
        })?;

        Self::persist_merged_session_metadata(
            &db,
            &id,
            authoritative_commit_message.as_deref(),
            merged_commit_hash.as_deref(),
            &app_event_tx,
        )
        .await?;
        Self::restack_child_sessions_after_parent_merge(&db, &id, &base_branch).await?;

        if !SessionTaskService::update_status(
            &status,
            clock.as_ref(),
            &db,
            &app_event_tx,
            &session_update_versions,
            &id,
            Status::Done,
        )
        .await
        {
            return Err(SessionError::Workflow(
                "Invalid status transition to Done".to_string(),
            ));
        }

        Ok(Self::merge_success_message(
            &source_branch,
            &base_branch,
            merge_outcome,
        ))
    }

    /// Clears stacked-child parent links after a parent branch merges.
    ///
    /// Children are retargeted to the merged parent's original base branch so
    /// a staged draft child becomes a root draft that can start normally.
    ///
    /// # Errors
    /// Returns an error when child metadata cannot be updated.
    pub(crate) async fn restack_child_sessions_after_parent_merge(
        db: &AppRepositories,
        parent_session_id: &str,
        base_branch: &str,
    ) -> Result<(), SessionError> {
        db.sessions()
            .restack_child_sessions_after_parent_merge(parent_session_id, base_branch)
            .await?;

        Ok(())
    }

    /// Persists post-merge metadata derived from the authoritative session
    /// commit message and merged base-branch commit hash.
    ///
    /// # Errors
    /// Returns an error when the merged commit hash cannot be saved.
    async fn persist_merged_session_metadata(
        db: &AppRepositories,
        session_id: &str,
        authoritative_commit_message: Option<&str>,
        merged_commit_hash: Option<&str>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
    ) -> Result<(), SessionError> {
        if let Some(commit_message) = authoritative_commit_message {
            Self::update_session_title_from_commit_message(
                db,
                session_id,
                commit_message,
                app_event_tx,
            )
            .await;
            Self::update_done_session_summary_from_commit_message(db, session_id, commit_message)
                .await;
        }
        if let Some(merged_commit_hash) = merged_commit_hash {
            db.sessions()
                .update_session_merged_commit_hash(session_id, Some(merged_commit_hash.to_string()))
                .await?;
        }

        Ok(())
    }

    /// Loads the target-branch `HEAD` hash created by one completed squash
    /// merge, when the merge produced a new commit.
    ///
    /// # Errors
    /// Returns an error when the merged commit hash cannot be inspected after
    /// a successful squash commit.
    async fn load_merged_commit_hash(
        git_client: &dyn GitClient,
        repo_root: PathBuf,
        merge_outcome: git::SquashMergeOutcome,
    ) -> Result<Option<String>, SessionError> {
        if merge_outcome != git::SquashMergeOutcome::Committed {
            return Ok(None);
        }

        let merged_commit_hash = git_client.head_hash(repo_root).await?;

        Ok(Some(merged_commit_hash))
    }

    /// Builds rebase input used by merge workflows.
    fn merge_rebase_input(input: &MergeTaskInput) -> RebaseAssistInput {
        RebaseAssistInput {
            app_event_tx: input.app_event_tx.clone(),
            assist_mode: RebaseAssistMode::OneShot,
            child_pid: Arc::clone(&input.child_pid),
            db: input.db.clone(),
            folder: input.folder.clone(),
            fs_client: Arc::clone(&input.fs_client),
            git_client: Arc::clone(&input.git_client),
            id: input.id.clone(),
            output: Arc::clone(&input.output),
            rebase_target: input.base_branch.clone(),
            session_model: input.session_model,
            session_update_versions: input.session_update_versions.clone(),
        }
    }

    /// Loads the squash-diff preview for one merge candidate.
    ///
    /// # Errors
    /// Returns an error when the diff cannot be generated.
    async fn load_squash_diff(
        git_client: &dyn GitClient,
        repo_root: PathBuf,
        source_branch: String,
        base_branch: String,
    ) -> Result<String, SessionError> {
        git_client
            .squash_merge_diff(repo_root, source_branch, base_branch)
            .await
            .map_err(|error| {
                SessionError::Workflow(format!("Failed to inspect merge diff: {error}"))
            })
    }

    /// Loads the canonical session commit message from the worktree `HEAD` for
    /// reuse during squash merge.
    ///
    /// # Errors
    /// Returns an error when `HEAD` cannot be inspected or does not contain a
    /// non-blank commit message.
    async fn load_authoritative_session_commit_message(
        git_client: &dyn GitClient,
        folder: PathBuf,
    ) -> Result<String, SessionError> {
        let commit_message = git_client.head_commit_message(folder).await?;
        let Some(commit_message) = commit_message else {
            return Err(SessionError::Workflow(
                "Session branch has no commit message to reuse for merge".to_string(),
            ));
        };
        let trimmed_commit_message = commit_message.trim();
        if trimmed_commit_message.is_empty() {
            return Err(SessionError::Workflow(
                "Session branch has a blank commit message to reuse for merge".to_string(),
            ));
        }

        Ok(trimmed_commit_message.to_string())
    }

    /// Finalizes one merge task by reporting the outcome and restoring review
    /// state only when the merge failed.
    ///
    /// Successful merges request an immediate git-status refresh so footer
    /// branch stats reflect the new refs without waiting for the periodic
    /// poller. The success notice is transient so the completed transcript does
    /// not persist merge bookkeeping as standalone chat content. The `Done`
    /// status transition also emits a full session refresh, which refreshes
    /// active-project roadmap task data.
    async fn finalize_merge_task(input: FinalizeMergeInput<'_>) {
        let FinalizeMergeInput {
            clock,
            db,
            app_event_tx,
            id,
            output,
            result,
            session_update_versions,
            status,
        } = input;

        match result {
            Ok(message) => {
                let merge_message = TranscriptNotice::Merge.format_line(message);
                SessionTaskService::emit_session_workflow_notice(app_event_tx, id, merge_message);
                SessionTaskService::request_git_status_refresh(app_event_tx);
            }
            Err(error) => {
                let merge_error = TranscriptNotice::MergeError.format(error);
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    &merge_error,
                )
                .await;
                if !SessionTaskService::update_status(
                    status,
                    clock,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    Status::Review,
                )
                .await
                {
                    warn!(
                        session_id = id,
                        "skipped restoring review status after merge error because the in-memory \
                         status was already current"
                    );
                }
            }
        }
    }

    /// Builds merge success output text for commit and no-op outcomes.
    fn merge_success_message(
        source_branch: &str,
        base_branch: &str,
        merge_outcome: git::SquashMergeOutcome,
    ) -> String {
        match merge_outcome {
            git::SquashMergeOutcome::Committed => {
                format!("Successfully merged {source_branch} into {base_branch}")
            }
            git::SquashMergeOutcome::AlreadyPresentInTarget => {
                format!("Session changes from {source_branch} are already present in {base_branch}")
            }
        }
    }

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for rebase, required git
    /// metadata is missing, or enqueueing the rebase command fails.
    pub async fn rebase_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        SessionMergeService
            .rebase_session(self, services, session_id)
            .await
    }

    /// Starts automatic rebase commands for materialized stacked children
    /// after a parent session turn finishes in review.
    pub(crate) async fn rebase_stacked_children_after_parent_turn(
        &mut self,
        services: &AppServices,
        parent_session_id: &str,
    ) -> Vec<(SessionId, SessionError)> {
        SessionMergeService
            .rebase_stacked_children_after_parent_turn(self, services, parent_session_id)
            .await
    }

    /// Synchronizes a project branch with upstream using only project context.
    ///
    /// This helper is reused by background-triggered sync workflows and tests.
    /// On success returns a [`SyncMainOutcome`] for popup status messaging.
    ///
    /// # Errors
    /// Returns a [`SyncSessionStartError`] when project context is invalid or
    /// updating the default branch fails (`git pull --rebase`, assisted
    /// conflict resolution, or `git push`).
    pub(crate) async fn sync_main_for_project(
        default_branch: Option<String>,
        working_dir: PathBuf,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
    ) -> Result<SyncMainOutcome, SyncSessionStartError> {
        let fs_client: Arc<dyn FsClient> = Arc::new(fs::RealFsClient);
        let sync_assist_client: Arc<dyn SyncAssistClient> = Arc::new(RealSyncAssistClient);

        Self::sync_main_for_project_with_assist_client(
            default_branch,
            working_dir,
            fs_client,
            git_client,
            session_model,
            sync_assist_client,
        )
        .await
    }

    /// Synchronizes the selected project branch with optional mocked assistance
    /// support for tests.
    ///
    /// # Errors
    /// Returns a [`SyncSessionStartError`] when project context is invalid,
    /// git operations fail, or rebase conflicts remain unresolved after
    /// assisted attempts.
    async fn sync_main_for_project_with_assist_client(
        default_branch: Option<String>,
        working_dir: PathBuf,
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
        sync_assist_client: Arc<dyn SyncAssistClient>,
    ) -> Result<SyncMainOutcome, SyncSessionStartError> {
        let default_branch = default_branch.ok_or_else(|| {
            SyncSessionStartError::Other("Active project has no git branch".to_string())
        })?;

        let _repo_root = git_client
            .find_git_repo_root(working_dir.clone())
            .await
            .ok_or_else(|| {
                SyncSessionStartError::Other("Failed to find git repository root".to_string())
            })?;

        let is_default_branch_clean = git_client
            .is_worktree_clean(working_dir.clone())
            .await
            .map_err(|error| SyncSessionStartError::Other(error.to_string()))?;
        if !is_default_branch_clean {
            return Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: default_branch.clone(),
            });
        }

        let ahead_behind_before_pull = git_client.get_ahead_behind(working_dir.clone()).await.ok();
        let pulled_commit_titles = git_client
            .list_upstream_commit_titles(working_dir.clone())
            .await
            .unwrap_or_default();

        let pull_result = git_client
            .pull_rebase(working_dir.clone())
            .await
            .map_err(|error| SyncSessionStartError::Other(error.to_string()))?;
        let mut resolved_conflict_files = Vec::new();
        if let git::PullRebaseResult::Conflict { detail } = pull_result {
            let sync_rebase_input = SyncRebaseAssistInput {
                base_branch: default_branch.clone(),
                folder: working_dir.clone(),
                fs_client: Arc::clone(&fs_client),
                git_client: Arc::clone(&git_client),
                session_model,
                sync_assist_client,
            };
            resolved_conflict_files =
                match Self::run_sync_rebase_assist_loop(sync_rebase_input, detail.clone()).await {
                    Ok(resolved_conflict_files) => resolved_conflict_files,
                    Err(error) => {
                        return Err(SyncSessionStartError::Other(format!(
                            "Sync stopped on rebase conflicts while updating `{default_branch}`: \
                             {detail}. Assisted resolution failed: {error}"
                        )));
                    }
                };
        }
        let ahead_behind_after_pull = git_client.get_ahead_behind(working_dir.clone()).await.ok();
        let pushed_commit_titles = git_client
            .list_local_commit_titles(working_dir.clone())
            .await
            .unwrap_or_default();

        git_client
            .push_current_branch(working_dir)
            .await
            .map_err(|error| SyncSessionStartError::Other(error.to_string()))?;

        let (pulled_commits, pushed_commits) = Self::summarize_sync_ahead_behind_counts(
            ahead_behind_before_pull,
            ahead_behind_after_pull,
        );

        Ok(SyncMainOutcome {
            pulled_commit_titles,
            pulled_commits,
            pushed_commit_titles,
            pushed_commits,
            resolved_conflict_files,
        })
    }

    /// Runs assisted conflict resolution for a main-project rebase in progress.
    ///
    /// # Errors
    /// Returns an error when conflicts remain unresolved after all attempts or
    /// when git/agent operations fail. On success returns resolved conflict
    /// file paths observed during assistance.
    async fn run_sync_rebase_assist_loop(
        input: SyncRebaseAssistInput,
        initial_conflict_detail: String,
    ) -> Result<Vec<String>, SessionError> {
        Self::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Project(input),
            Some(initial_conflict_detail),
        )
        .await
        .map(|outcome| outcome.resolved_conflict_files)
    }

    /// Returns pull/push counts inferred around one completed sync run.
    fn summarize_sync_ahead_behind_counts(
        ahead_behind_before_pull: Option<(u32, u32)>,
        ahead_behind_after_pull: Option<(u32, u32)>,
    ) -> (Option<u32>, Option<u32>) {
        let pulled_commits = ahead_behind_before_pull.map(|(_ahead, behind)| behind);
        let pushed_commits = ahead_behind_after_pull
            .map(|(ahead, _behind)| ahead)
            .or_else(|| ahead_behind_before_pull.map(|(ahead, _behind)| ahead));

        (pulled_commits, pushed_commits)
    }

    /// Loads current conflicted files for sync assistance.
    ///
    /// # Errors
    /// Returns an error if conflicted-file inspection fails.
    async fn load_sync_conflicted_files(
        input: &SyncRebaseAssistInput,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, SessionError> {
        let folder = input.folder.clone();
        let mut conflicted = input
            .git_client
            .list_conflicted_files(folder.clone())
            .await?;

        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
            .await?;
        for file in staged_with_markers {
            if !conflicted.contains(&file) {
                conflicted.push(file);
            }
        }
        conflicted.sort_unstable();

        Ok(conflicted)
    }

    /// Executes one agent-assisted sync rebase conflict resolution attempt.
    ///
    /// # Errors
    /// Returns an error when the assistance command fails.
    async fn run_sync_rebase_assist_agent(
        input: &SyncRebaseAssistInput,
        conflicted_files: &[String],
    ) -> Result<(), SessionError> {
        let prompt = Self::rebase_assist_prompt(&input.base_branch, conflicted_files)?;
        input
            .sync_assist_client
            .resolve_rebase_conflicts(input.folder.clone(), prompt, input.session_model)
            .await
            .map_err(|error| error.with_context("Sync rebase assistance failed"))
    }

    /// Stages sync edits and checks whether rebase conflicts remain.
    ///
    /// # Errors
    /// Returns an error when staging or conflict checks fail.
    async fn stage_and_check_for_sync_conflicts(
        input: &SyncRebaseAssistInput,
        conflict_files: &[String],
    ) -> Result<bool, SessionError> {
        let folder = input.folder.clone();
        input.git_client.stage_all(folder).await?;

        let folder = input.folder.clone();
        if input.git_client.has_unmerged_paths(folder).await? {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, conflict_files.to_vec())
            .await?;

        Ok(!staged_with_markers.is_empty())
    }

    /// Continues the in-progress sync rebase.
    ///
    /// # Errors
    /// Returns an error when `git rebase --continue` fails with non-conflict
    /// errors.
    async fn run_sync_rebase_continue(
        input: &SyncRebaseAssistInput,
    ) -> Result<git::RebaseStepResult, SessionError> {
        let folder = input.folder.clone();
        let result = input.git_client.rebase_continue(folder).await?;

        Ok(result)
    }

    /// Aborts an in-progress sync rebase after assistance failure.
    async fn abort_sync_rebase_after_assist_failure(input: &SyncRebaseAssistInput) {
        let folder = input.folder.clone();
        if let Err(error) = input.git_client.abort_rebase(folder).await {
            warn!(
                base_branch = input.base_branch,
                error = %error,
                "failed to abort sync rebase after assistance failure"
            );
        }
    }

    /// Runs a rebase command and finalizes its user-visible status/output.
    ///
    /// # Errors
    /// Returns an error when the rebase workflow fails after finalization has
    /// appended the corresponding transcript notice and restored `Review`.
    pub(super) async fn run_rebase_command(input: RebaseCommandInput) -> Result<(), SessionError> {
        let RebaseCommandInput {
            app_event_tx,
            assist_mode,
            base_branch,
            child_pid,
            clock,
            db,
            folder,
            fs_client,
            git_client,
            id,
            output,
            session_model,
            session_update_versions,
            status,
        } = input;

        let rebase_result: Result<String, SessionError> = async {
            let rebase_target = Self::resolve_session_rebase_target(
                &db,
                git_client.as_ref(),
                &folder,
                &id,
                &base_branch,
            )
            .await?;
            let rebase_input = RebaseAssistInput {
                app_event_tx: app_event_tx.clone(),
                assist_mode,
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: folder.clone(),
                fs_client: Arc::clone(&fs_client),
                git_client: Arc::clone(&git_client),
                id: id.clone(),
                output: Arc::clone(&output),
                rebase_target,
                session_model,
                session_update_versions: session_update_versions.clone(),
            };

            Self::execute_rebase_workflow(rebase_input).await
        }
        .await;
        let command_error = rebase_result.as_ref().err().map(ToString::to_string);

        Self::finalize_rebase_task(FinalizeRebaseInput {
            app_event_tx: &app_event_tx,
            clock: clock.as_ref(),
            db: &db,
            folder: &folder,
            git_client: &git_client,
            id: &id,
            output: &output,
            rebase_result,
            session_update_versions: &session_update_versions,
            status: &status,
        })
        .await;

        if let Some(error) = command_error {
            return Err(SessionError::Workflow(error));
        }

        Ok(())
    }

    /// Resolves the git ref used for rebasing one session branch.
    ///
    /// Unpublished sessions rebase against the stored local base branch. Once
    /// a session branch is published, the rebase first fetches and targets the
    /// remote base ref from the same remote as the published upstream so the
    /// pull request comparison is updated against the forge-visible base.
    ///
    /// # Errors
    /// Returns an error when the published-session fetch fails.
    async fn resolve_session_rebase_target(
        db: &AppRepositories,
        git_client: &dyn GitClient,
        folder: &Path,
        session_id: &str,
        base_branch: &str,
    ) -> Result<String, SessionError> {
        let Some(published_upstream_ref) = db
            .sessions()
            .load_session_published_upstream_ref(session_id)
            .await
            .map_err(SessionError::Db)?
        else {
            return Ok(base_branch.to_string());
        };

        let Some((remote_name, _branch_name)) = published_upstream_ref.split_once('/') else {
            return Ok(base_branch.to_string());
        };

        git_client
            .fetch_remote(folder.to_path_buf())
            .await
            .map_err(|error| {
                SessionError::Workflow(format!(
                    "Failed to fetch `{remote_name}` before rebasing published session branch: \
                     {error}"
                ))
            })?;

        Ok(format!("{remote_name}/{base_branch}"))
    }

    /// Executes one assisted rebase workflow for a session worktree.
    ///
    /// Aborts any in-progress rebase when the assist loop fails so stale
    /// rebase metadata does not leak into later merge or sync operations.
    ///
    /// # Errors
    /// Returns an error when pre-sync auto-commit fails or assisted rebase
    /// cannot be completed.
    ///
    /// Emits user-visible commit output before sync starts so users can see
    /// whether pending changes were committed or there was nothing to commit.
    /// The pre-sync auto-commit reuses the active project's fast-model
    /// default when generating or repairing the session commit message, and a
    /// successful commit requests an immediate git-status refresh.
    async fn execute_rebase_workflow(input: RebaseAssistInput) -> Result<String, SessionError> {
        // Auto-commit any pending changes before rebasing to avoid
        // "cannot rebase: You have unstaged changes".
        let include_coauthored_by_agentty =
            SessionTaskService::load_include_coauthored_by_agentty_setting(&input.db, &input.id)
                .await;
        let auto_commit_model = SessionTaskService::load_auto_commit_model_setting(
            &input.db,
            &input.id,
            input.session_model,
        )
        .await;
        match SessionTaskService::commit_session_changes(
            input.git_client.as_ref(),
            &input.folder,
            &input.rebase_target,
            auto_commit_model,
            true,
            include_coauthored_by_agentty,
        )
        .await
        {
            Ok(outcome) => {
                Self::update_session_title_from_commit_message(
                    &input.db,
                    &input.id,
                    &outcome.commit_message,
                    &input.app_event_tx,
                )
                .await;

                let commit_message = TranscriptNotice::Commit
                    .format_line(format!("committed with hash `{}`", outcome.commit_hash));
                SessionTaskService::emit_session_workflow_notice(
                    &input.app_event_tx,
                    &input.id,
                    commit_message,
                );
                SessionTaskService::request_git_status_refresh(&input.app_event_tx);
            }
            Err(error) if error.to_string().contains("Nothing to commit") => {
                let commit_message = TranscriptNotice::Commit.format_line("No changes to commit.");
                SessionTaskService::emit_session_workflow_notice(
                    &input.app_event_tx,
                    &input.id,
                    commit_message,
                );
            }
            Err(error) => {
                return Err(SessionError::Workflow(format!(
                    "Failed to commit pending changes before sync: {error}"
                )));
            }
        }

        if let Err(error) = Self::run_rebase_assist_loop(input.clone()).await {
            Self::abort_rebase_after_assist_failure(&input).await;

            return Err(SessionError::Workflow(format!("Failed to sync: {error}")));
        }

        let source_branch = session_branch(&input.id);
        let rebase_target = &input.rebase_target;

        Ok(format!(
            "Successfully synced {source_branch} onto {rebase_target}"
        ))
    }

    /// Finalizes one rebase task by appending the outcome and restoring the
    /// session lifecycle state.
    ///
    /// Successful rebases request an immediate git-status refresh so footer
    /// branch stats do not wait for the periodic poller, and trigger an
    /// auto-push when the session has a previously published upstream branch.
    async fn finalize_rebase_task(input: FinalizeRebaseInput<'_>) {
        let FinalizeRebaseInput {
            app_event_tx,
            clock,
            db,
            folder,
            git_client,
            id,
            output,
            rebase_result,
            session_update_versions,
            status,
        } = input;

        match rebase_result {
            Ok(message) => {
                let rebase_message = TranscriptNotice::Rebase.format(message);
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    &rebase_message,
                )
                .await;
                SessionTaskService::request_git_status_refresh(app_event_tx);

                Self::start_auto_push_after_rebase(
                    db,
                    app_event_tx,
                    folder,
                    git_client,
                    output,
                    id,
                    session_update_versions,
                )
                .await;
            }
            Err(error) => {
                let rebase_error = TranscriptNotice::RebaseError.format(error);
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    &rebase_error,
                )
                .await;
            }
        }

        if !SessionTaskService::update_status(
            status,
            clock,
            db,
            app_event_tx,
            session_update_versions,
            id,
            Status::Review,
        )
        .await
        {
            warn!(
                session_id = id,
                "skipped restoring review status after rebase because the in-memory status was \
                 already current"
            );
        }
    }

    /// Starts a detached auto-push task when the rebased session has a
    /// previously published upstream branch.
    async fn start_auto_push_after_rebase(
        db: &AppRepositories,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        folder: &Path,
        git_client: &Arc<dyn GitClient>,
        output: &Arc<Mutex<String>>,
        session_id: &str,
        session_update_versions: &SessionUpdateVersionMap,
    ) {
        let published_upstream_ref = db
            .sessions()
            .load_session_published_upstream_ref(session_id)
            .await
            .ok()
            .flatten();

        let Some(published_upstream_ref) = published_upstream_ref else {
            return;
        };

        let sync_operation_id = uuid::Uuid::new_v4().to_string();

        if app_event_tx
            .send(AppEvent::PublishedBranchSyncUpdated {
                session_id: SessionId::from(session_id),
                sync_operation_id: sync_operation_id.clone(),
                sync_status: PublishedBranchSyncStatus::InProgress,
            })
            .is_err()
        {
            warn!(
                session_id = session_id,
                sync_operation_id = sync_operation_id,
                "failed to publish branch sync start because the app event receiver is closed"
            );
        }

        let app_event_tx = app_event_tx.clone();
        let db = db.clone();
        let folder = folder.to_path_buf();
        let git_client = Arc::clone(git_client);
        let output = Arc::clone(output);
        let session_id = SessionId::from(session_id);
        let auto_push_input = PublishedBranchAutoPushInput {
            app_event_tx,
            db,
            folder,
            git_client,
            output,
            published_upstream_ref,
            review_request_metadata_sync: None,
            session_id,
            session_update_versions: session_update_versions.clone(),
            sync_operation_id,
        };

        tokio::spawn(async move {
            published_branch::run_published_branch_auto_push(auto_push_input).await;
        });
    }

    /// Updates the persisted session title from the canonical commit message.
    pub(crate) async fn update_session_title_from_commit_message(
        db: &AppRepositories,
        session_id: &str,
        commit_message: &str,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
    ) {
        let title = Self::session_title_from_commit_message(commit_message);

        if let Err(error) = db.sessions().update_session_title(session_id, &title).await {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to persist session title from commit message"
            );
        }

        if app_event_tx.send(AppEvent::RefreshSessions).is_err() {
            warn!(
                session_id = session_id,
                "failed to refresh sessions after commit-title update because the app event \
                 receiver is closed"
            );
        }
    }

    /// Updates the persisted done-session summary by formatting the latest
    /// persisted agent session-summary text, extracting `summary.session`
    /// from raw JSON payloads when needed, and canonical commit message into
    /// markdown sections.
    async fn update_done_session_summary_from_commit_message(
        db: &AppRepositories,
        session_id: &str,
        commit_message: &str,
    ) {
        let summary = Self::session_summary_with_commit_message(
            Self::persisted_session_summary(db, session_id)
                .await
                .as_deref(),
            commit_message,
        );

        if let Err(error) = db
            .sessions()
            .update_session_summary(session_id, &summary)
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to persist done-session summary from commit message"
            );
        }
    }

    /// Loads the currently persisted session summary text for one session.
    async fn persisted_session_summary(db: &AppRepositories, session_id: &str) -> Option<String> {
        db.sessions()
            .load_session_summary(session_id)
            .await
            .ok()
            .flatten()
    }

    /// Extracts the first non-empty line from one session commit message for
    /// use as the session title.
    fn session_title_from_commit_message(commit_message: &str) -> String {
        let trimmed_message = commit_message.trim();
        if trimmed_message.is_empty() {
            return "Apply session updates".to_string();
        }

        trimmed_message
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("Apply session updates")
            .to_string()
    }

    /// Builds the persisted done-session summary with markdown sections.
    ///
    /// Includes `# Summary` from the final agent session-summary text,
    /// extracting `summary.session` from persisted JSON payloads when needed,
    /// and `# Commit` from the canonical session commit message.
    fn session_summary_with_commit_message(
        session_summary: Option<&str>,
        commit_message: &str,
    ) -> String {
        let trimmed_summary = session_summary.map(str::trim).unwrap_or_default();
        let summary_text = serde_json::from_str::<AgentResponseSummary>(trimmed_summary)
            .map_or_else(
                |_| trimmed_summary.to_string(),
                |summary_payload| summary_payload.session,
            );
        let trimmed_commit_message = commit_message.trim();

        format!("# Summary\n\n{summary_text}\n\n# Commit\n\n{trimmed_commit_message}")
    }

    /// Runs a bounded rebase-assistance loop until conflicts are resolved.
    ///
    /// # Errors
    /// Returns an error when conflict resolution fails after all attempts or
    /// when git/agent operations fail.
    async fn run_rebase_assist_loop(input: RebaseAssistInput) -> Result<(), SessionError> {
        let rebase_in_progress = Self::is_rebase_in_progress(&input).await?;
        let mut initial_conflict_detail = None;
        if !rebase_in_progress {
            let initial_step = Self::run_rebase_start(&input).await?;
            match initial_step {
                git::RebaseStepResult::Completed => return Ok(()),
                git::RebaseStepResult::Conflict { detail } => {
                    initial_conflict_detail = Some(detail);
                }
            }
        }

        Self::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            initial_conflict_detail,
        )
        .await
        .map(|_| ())
    }

    /// Executes shared bounded assistance loop for both session rebases and
    /// main-project sync rebases.
    ///
    /// # Errors
    /// Returns an error when assistance fails to make progress, rebase remains
    /// conflicted after all attempts, or git operations fail. On every error
    /// path (including early `?` failures), the in-progress rebase is aborted
    /// before returning.
    async fn run_rebase_assist_loop_core(
        assist_input: RebaseAssistLoopInput,
        initial_conflict_detail: Option<String>,
    ) -> Result<RebaseAssistOutcome, SessionError> {
        let assist_result =
            Self::run_rebase_assist_attempts(&assist_input, initial_conflict_detail).await;

        match assist_result {
            Ok(assist_outcome) => Ok(assist_outcome),
            Err(error) => {
                assist_input.abort_rebase_after_assist_failure().await;

                Err(error)
            }
        }
    }

    /// Runs each bounded rebase-assistance attempt until completion or
    /// exhaustion.
    ///
    /// # Errors
    /// Returns an error when conflict resolution fails, git operations fail,
    /// or the retry budget is exhausted.
    async fn run_rebase_assist_attempts(
        assist_input: &RebaseAssistLoopInput,
        initial_conflict_detail: Option<String>,
    ) -> Result<RebaseAssistOutcome, SessionError> {
        let mut failure_tracker =
            Self::rebase_failure_tracker_with_initial_detail(initial_conflict_detail);
        let mut assist_outcome = RebaseAssistOutcome::empty();
        let mut previous_conflict_files: Vec<String> = vec![];

        for assist_attempt in 1..=REBASE_ASSIST_POLICY.max_attempts {
            let attempt_outcome = Self::run_rebase_assist_attempt(
                assist_input,
                &mut failure_tracker,
                &mut assist_outcome,
                &mut previous_conflict_files,
                assist_attempt,
            )
            .await?;

            if let RebaseAssistAttemptOutcome::Completed = attempt_outcome {
                return Ok(assist_outcome);
            }
        }

        Err(SessionError::Workflow(assist_input.exhausted_error()))
    }

    /// Builds the no-progress tracker and seeds it with the initial conflict
    /// detail when the rebase starts from a known conflict state.
    fn rebase_failure_tracker_with_initial_detail(
        initial_conflict_detail: Option<String>,
    ) -> FailureTracker {
        let mut failure_tracker =
            FailureTracker::new(REBASE_ASSIST_POLICY.max_identical_failure_streak);

        if let Some(initial_conflict_detail) = initial_conflict_detail {
            let _ = failure_tracker.observe(&initial_conflict_detail);
        }

        failure_tracker
    }

    /// Executes one rebase-assistance attempt and records loop state needed
    /// by the next attempt.
    ///
    /// # Errors
    /// Returns an error when conflict inspection, assistance, staging, or
    /// rebase continuation fails.
    async fn run_rebase_assist_attempt(
        assist_input: &RebaseAssistLoopInput,
        failure_tracker: &mut FailureTracker,
        assist_outcome: &mut RebaseAssistOutcome,
        previous_conflict_files: &mut Vec<String>,
        assist_attempt: usize,
    ) -> Result<RebaseAssistAttemptOutcome, SessionError> {
        let conflicted_files = assist_input
            .load_conflicted_files(previous_conflict_files)
            .await?;

        if conflicted_files.is_empty() {
            return Self::continue_rebase_after_assist_attempt(
                assist_input,
                failure_tracker,
                assist_attempt,
            )
            .await;
        }

        Self::resolve_conflicted_files_with_assist(
            assist_input,
            failure_tracker,
            assist_outcome,
            previous_conflict_files,
            assist_attempt,
            conflicted_files,
        )
        .await
    }

    /// Applies assistance to conflicted files, stages the result, and
    /// continues the rebase when conflicts clear.
    ///
    /// # Errors
    /// Returns an error when assistance makes no progress, the retry budget is
    /// exhausted, or git operations fail.
    async fn resolve_conflicted_files_with_assist(
        assist_input: &RebaseAssistLoopInput,
        failure_tracker: &mut FailureTracker,
        assist_outcome: &mut RebaseAssistOutcome,
        previous_conflict_files: &mut Vec<String>,
        assist_attempt: usize,
        conflicted_files: Vec<String>,
    ) -> Result<RebaseAssistAttemptOutcome, SessionError> {
        Self::record_conflicted_file_fingerprint(assist_input, failure_tracker, &conflicted_files)
            .await?;
        assist_outcome.extend_resolved_conflict_files(&conflicted_files);

        assist_input
            .run_assist_attempt(assist_attempt, &conflicted_files)
            .await?;

        let still_has_conflicts = assist_input
            .stage_and_check_for_conflicts(&conflicted_files)
            .await?;
        *previous_conflict_files = conflicted_files;

        if still_has_conflicts {
            return Self::evaluate_remaining_assist_conflicts(assist_attempt);
        }

        Self::continue_rebase_after_assist_attempt(assist_input, failure_tracker, assist_attempt)
            .await
    }

    /// Records the content fingerprint for the current conflicted file set.
    ///
    /// # Errors
    /// Returns an error when the conflict fingerprint matches the previous
    /// no-progress observations too many times.
    async fn record_conflicted_file_fingerprint(
        assist_input: &RebaseAssistLoopInput,
        failure_tracker: &mut FailureTracker,
        conflicted_files: &[String],
    ) -> Result<(), SessionError> {
        let conflict_fingerprint = Self::conflicted_file_fingerprint(
            assist_input.fs_client(),
            assist_input.folder(),
            conflicted_files,
        )
        .await;

        if failure_tracker.observe(&conflict_fingerprint) {
            return Err(SessionError::Workflow(
                assist_input.unchanged_conflict_files_error(),
            ));
        }

        Ok(())
    }

    /// Converts unresolved conflicts after an assistance attempt into either
    /// another retry or the final retry-exhaustion error.
    ///
    /// # Errors
    /// Returns an error when the maximum assistance attempt has already been
    /// used.
    fn evaluate_remaining_assist_conflicts(
        assist_attempt: usize,
    ) -> Result<RebaseAssistAttemptOutcome, SessionError> {
        if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
            return Err(SessionError::Workflow(
                "Conflicts remain unresolved after maximum assistance attempts".to_string(),
            ));
        }

        Ok(RebaseAssistAttemptOutcome::Continue)
    }

    /// Runs `git rebase --continue` after one assistance attempt and evaluates
    /// the result.
    ///
    /// # Errors
    /// Returns an error when rebase continuation fails, repeats an unchanged
    /// conflict detail, or reports a final unresolved conflict.
    async fn continue_rebase_after_assist_attempt(
        assist_input: &RebaseAssistLoopInput,
        failure_tracker: &mut FailureTracker,
        assist_attempt: usize,
    ) -> Result<RebaseAssistAttemptOutcome, SessionError> {
        let continue_step = assist_input.run_rebase_continue().await?;

        Self::evaluate_rebase_continue_step(
            assist_input,
            failure_tracker,
            assist_attempt,
            continue_step,
        )
    }

    /// Interprets one rebase continuation result for the assistance loop.
    ///
    /// # Errors
    /// Returns an error when the continuation reports repeated or final
    /// conflict detail.
    fn evaluate_rebase_continue_step(
        assist_input: &RebaseAssistLoopInput,
        failure_tracker: &mut FailureTracker,
        assist_attempt: usize,
        continue_step: git::RebaseStepResult,
    ) -> Result<RebaseAssistAttemptOutcome, SessionError> {
        match continue_step {
            git::RebaseStepResult::Completed => Ok(RebaseAssistAttemptOutcome::Completed),
            git::RebaseStepResult::Conflict { detail } => {
                Self::record_rebase_conflict_detail(
                    assist_input,
                    failure_tracker,
                    assist_attempt,
                    &detail,
                )?;

                Ok(RebaseAssistAttemptOutcome::Continue)
            }
        }
    }

    /// Records conflict detail from rebase continuation and enforces
    /// no-progress and retry-exhaustion policies.
    ///
    /// # Errors
    /// Returns an error when the conflict detail repeats too often or the
    /// final attempt still reports a conflict.
    fn record_rebase_conflict_detail(
        assist_input: &RebaseAssistLoopInput,
        failure_tracker: &mut FailureTracker,
        assist_attempt: usize,
        detail: &str,
    ) -> Result<(), SessionError> {
        if failure_tracker.observe(detail) {
            return Err(SessionError::Workflow(
                assist_input.repeated_conflict_state_error(detail),
            ));
        }

        if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
            return Err(SessionError::Workflow(
                assist_input.still_conflicted_error(detail),
            ));
        }

        Ok(())
    }

    /// Returns whether the session worktree has an in-progress rebase.
    ///
    /// # Errors
    /// Returns an error if git state cannot be queried.
    async fn is_rebase_in_progress(input: &RebaseAssistInput) -> Result<bool, SessionError> {
        let folder = input.folder.clone();
        let is_rebase_in_progress = input.git_client.is_rebase_in_progress(folder).await?;

        Ok(is_rebase_in_progress)
    }

    /// Starts the rebase step for an assisted rebase flow.
    ///
    /// When git reports stale rebase metadata (`rebase-merge`/`rebase-apply`)
    /// this helper attempts one cleanup pass via `git rebase --abort` and then
    /// retries the start command once.
    ///
    /// # Errors
    /// Returns an error if spawning the git process fails or git returns a
    /// non-conflict failure that cannot be recovered.
    async fn run_rebase_start(
        input: &RebaseAssistInput,
    ) -> Result<git::RebaseStepResult, SessionError> {
        let folder = input.folder.clone();
        let rebase_target = input.rebase_target.clone();
        match input
            .git_client
            .rebase_start(folder.clone(), rebase_target.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(error) => {
                let error_string = error.to_string();
                if !Self::is_stale_rebase_state_error(&error_string) {
                    return Err(SessionError::Workflow(error_string));
                }

                Self::recover_from_stale_rebase_start_error(input, &error_string).await?;

                input
                    .git_client
                    .rebase_start(folder, rebase_target)
                    .await
                    .map_err(SessionError::Git)
            }
        }
    }

    /// Returns whether a rebase start error indicates stale rebase metadata.
    fn is_stale_rebase_state_error(error: &str) -> bool {
        let normalized_error = error.to_ascii_lowercase();

        normalized_error.contains("already a rebase-merge directory")
            || normalized_error.contains("already a rebase-apply directory")
            || normalized_error.contains("middle of another rebase")
    }

    /// Tries to clean stale rebase metadata before retrying rebase start.
    ///
    /// # Errors
    /// Returns an error when `git rebase --abort` cannot clean up stale state.
    async fn recover_from_stale_rebase_start_error(
        input: &RebaseAssistInput,
        start_error: &str,
    ) -> Result<(), SessionError> {
        let folder = input.folder.clone();
        input
            .git_client
            .abort_rebase(folder)
            .await
            .map_err(|abort_error| {
                SessionError::Workflow(format!(
                    "Detected stale rebase metadata after failed rebase start: {start_error}. \
                     Cleanup with `git rebase --abort` failed: {abort_error}"
                ))
            })?;

        Ok(())
    }

    /// Loads all conflicted files from the worktree.
    ///
    /// Returns the union of two sets:
    /// - Files with *unmerged* index entries (classic rebase conflict state).
    /// - Files that were staged (`git add`) while still containing `<<<<<<<`
    ///   conflict markers, scoped to the provided `previous_conflict_files`.
    ///   This catches the case where an agent partially resolves a conflict and
    ///   stages the file without removing all markers, which would otherwise
    ///   make the file appear resolved.
    ///
    /// On the first call (no known prior conflicts), pass an empty slice for
    /// `previous_conflict_files`; only unmerged entries will be returned.
    ///
    /// # Errors
    /// Returns an error if either git query fails.
    async fn load_conflicted_files(
        input: &RebaseAssistInput,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, SessionError> {
        let folder = input.folder.clone();
        let mut conflicted = input
            .git_client
            .list_conflicted_files(folder.clone())
            .await?;

        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
            .await?;
        for file in staged_with_markers {
            if !conflicted.contains(&file) {
                conflicted.push(file);
            }
        }
        conflicted.sort_unstable();

        Ok(conflicted)
    }

    /// Appends an informational header for one rebase assistance attempt.
    async fn append_rebase_assist_header(
        input: &RebaseAssistInput,
        assist_attempt: usize,
        conflicted_files: &[String],
    ) {
        let conflict_summary = Self::format_conflicted_file_list(conflicted_files);
        append_assist_header(
            &Self::assist_context(input),
            TranscriptNotice::RebaseAssist,
            assist_attempt,
            REBASE_ASSIST_POLICY.max_attempts,
            "Resolving conflicts in:",
            &conflict_summary,
        )
        .await;
    }

    /// Runs an agent task to resolve the provided conflicted files.
    ///
    /// # Errors
    /// Returns an error if the agent process fails.
    async fn run_rebase_assist_agent(
        input: &RebaseAssistInput,
        conflicted_files: &[String],
    ) -> Result<(), SessionError> {
        let prompt = Self::rebase_assist_prompt(&input.rebase_target, conflicted_files)?;
        match &input.assist_mode {
            RebaseAssistMode::ExistingSession(assist_client) => assist_client
                .resolve_rebase_conflicts(prompt)
                .await
                .map_err(|error| error.with_context("Rebase assistance failed")),
            RebaseAssistMode::OneShot => {
                let assist_context = Self::assist_context(input);

                run_agent_assist(&assist_context, &prompt)
                    .await
                    .map_err(|error| error.with_context("Rebase assistance failed"))
            }
        }
    }

    /// Stages all worktree edits and checks whether any conflicts remain.
    ///
    /// Performs two checks after staging:
    /// 1. Unmerged index entries — files that were never resolved (`git add`d).
    /// 2. Staged content with `<<<<<<<` markers in `conflict_files` — files
    ///    that were staged while still containing residual conflict markers.
    ///
    /// Both checks are required because `git add` transitions a file from
    /// "Unmerged" to "Modified-in-index", making it invisible to the unmerged
    /// check even when conflict markers remain in its content.
    ///
    /// # Errors
    /// Returns an error when staging or either conflict check fails.
    async fn stage_and_check_for_conflicts(
        input: &RebaseAssistInput,
        conflict_files: &[String],
    ) -> Result<bool, SessionError> {
        let folder = input.folder.clone();
        input.git_client.stage_all(folder).await?;

        let folder = input.folder.clone();
        if input.git_client.has_unmerged_paths(folder).await? {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, conflict_files.to_vec())
            .await?;

        Ok(!staged_with_markers.is_empty())
    }

    /// Continues an in-progress rebase after conflict edits are applied.
    ///
    /// # Errors
    /// Returns an error if git reports a non-conflict failure.
    async fn run_rebase_continue(
        input: &RebaseAssistInput,
    ) -> Result<git::RebaseStepResult, SessionError> {
        let folder = input.folder.clone();
        let result = input.git_client.rebase_continue(folder).await?;

        Ok(result)
    }

    /// Renders the rebase-assist prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error if Askama template rendering fails.
    fn rebase_assist_prompt(
        base_branch: &str,
        conflicted_files: &[String],
    ) -> Result<String, SessionError> {
        let conflicted_files = Self::format_conflicted_file_list(conflicted_files);
        let template = RebaseAssistPromptTemplate {
            base_branch,
            conflicted_files: &conflicted_files,
        };

        template.render().map_err(|error| {
            SessionError::Workflow(format!(
                "Failed to render `rebase_assist_prompt.md`: {error}"
            ))
        })
    }

    /// Formats conflicted file paths as a bullet list for prompt rendering.
    fn format_conflicted_file_list(conflicted_files: &[String]) -> String {
        format_detail_lines(&conflicted_files.join("\n"))
    }

    /// Computes a content-based fingerprint for the current set of conflicted
    /// files.
    ///
    /// Reads each file from disk and hashes both its path and content so that
    /// partial progress made by the rebase-assist agent (e.g. removing some
    /// but not all conflict markers) changes the fingerprint and prevents the
    /// [`FailureTracker`] from firing prematurely. The fingerprint is
    /// order-independent because paths are sorted before hashing.
    async fn conflicted_file_fingerprint(
        fs_client: &dyn FsClient,
        folder: &Path,
        conflicted_files: &[String],
    ) -> String {
        let mut sorted_files = conflicted_files.to_vec();
        sorted_files.sort_unstable();

        let mut hasher = DefaultHasher::new();
        for file in &sorted_files {
            file.hash(&mut hasher);
            let file_path = folder.join(file);
            if let Ok(content) = fs_client.read_file(file_path).await {
                content.hash(&mut hasher);
            }
        }

        format!("{:016x}", hasher.finish())
    }

    /// Builds shared assistance context from rebase input state.
    fn assist_context(input: &RebaseAssistInput) -> AssistContext {
        AssistContext {
            app_event_tx: input.app_event_tx.clone(),
            child_pid: Arc::clone(&input.child_pid),
            db: input.db.clone(),
            folder: input.folder.clone(),
            git_client: Arc::clone(&input.git_client),
            id: input.id.to_string(),
            output: Arc::clone(&input.output),
            session_model: input.session_model,
            session_update_versions: input.session_update_versions.clone(),
        }
    }

    /// Aborts rebase after assistance fails to keep worktree state clean.
    async fn abort_rebase_after_assist_failure(input: &RebaseAssistInput) {
        let folder = input.folder.clone();
        if let Err(error) = input.git_client.abort_rebase(folder).await {
            warn!(
                session_id = %input.id,
                error = %error,
                "failed to abort rebase after assistance failure"
            );
        }
    }

    /// Removes a merged session worktree and deletes its source branch.
    ///
    /// When `repo_root` is not provided, this resolves the shared repository
    /// root through `git rev-parse` via `GitClient`.
    ///
    /// # Errors
    /// Returns an error if worktree or branch cleanup fails.
    pub(crate) async fn cleanup_merged_session_worktree(
        folder: PathBuf,
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn GitClient>,
        source_branch: String,
        repo_root: Option<PathBuf>,
    ) -> Result<(), SessionError> {
        let repo_root = match repo_root {
            Some(repo_root) => Some(repo_root),
            None => git_client.main_repo_root(folder.clone()).await.ok(),
        };

        git_client.remove_worktree(folder.clone()).await?;

        if let Some(repo_root) = repo_root {
            git_client.delete_branch(repo_root, source_branch).await?;
        }

        if let Err(error) = agent::cleanup_session_worktree_artifacts(&folder) {
            warn!(
                error = %error,
                "failed to remove provider worktree artifacts during merged session cleanup"
            );
        }

        if let Err(error) = fs_client.remove_dir_all(folder).await {
            warn!(
                error = %error,
                "failed to remove merged session worktree directory"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mockall::Sequence;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::infra::git::GitError;

    /// Builds a filesystem mock that delegates operations to local disk.
    fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_create_dir_all()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::create_dir_all(path)
                        .await
                        .map_err(fs::FsError::from)
                })
            });
        mock_fs_client
            .expect_remove_dir_all()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::remove_dir_all(path)
                        .await
                        .map_err(fs::FsError::from)
                })
            });
        mock_fs_client
            .expect_read_file()
            .times(0..)
            .returning(|path| {
                Box::pin(async move { tokio::fs::read(path).await.map_err(fs::FsError::from) })
            });
        mock_fs_client
            .expect_remove_file()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(|path| path.is_dir());

        mock_fs_client
    }

    /// Returns a fresh mocked filesystem client trait object for tests.
    fn test_fs_client() -> Arc<dyn FsClient> {
        Arc::new(create_passthrough_mock_fs_client())
    }

    /// Builds rebase assistance input with the provided git client for unit
    /// tests.
    async fn build_rebase_assist_input_for_test(
        git_client: Arc<dyn GitClient>,
    ) -> (TempDir, RebaseAssistInput) {
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let db = AppRepositories::in_memory().await;
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().to_path_buf();

        (
            temp_dir,
            RebaseAssistInput {
                app_event_tx,
                assist_mode: RebaseAssistMode::OneShot,
                child_pid: Arc::new(Mutex::new(None)),
                db,
                folder,
                fs_client: test_fs_client(),
                git_client,
                id: "session-123".into(),
                output: Arc::new(Mutex::new(String::new())),
                rebase_target: "main".to_string(),
                session_model: AgentModel::AntigravityGemini3FlashPreview,
                session_update_versions: Arc::default(),
            },
        )
    }

    /// Builds merge-task input with injected git client for deterministic
    /// workflow tests.
    async fn build_merge_task_input_for_test(
        git_client: Arc<dyn GitClient>,
    ) -> (TempDir, MergeTaskInput) {
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let db = AppRepositories::in_memory().await;
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().join("session-worktree");
        let repo_root = temp_dir.path().join("repo-root");

        (
            temp_dir,
            MergeTaskInput {
                app_event_tx,
                base_branch: "main".to_string(),
                child_pid: Arc::new(Mutex::new(None)),
                clock: Arc::new(crate::infra::clock::RealClock),
                db,
                folder,
                fs_client: test_fs_client(),
                git_client,
                id: "session-123".into(),
                output: Arc::new(Mutex::new(String::new())),
                repo_root,
                session_update_versions: Arc::default(),
                session_model: AgentModel::AntigravityGemini3FlashPreview,
                source_branch: "wt/session-123".to_string(),
                status: Arc::new(Mutex::new(Status::Merging)),
            },
        )
    }

    /// Builds sync rebase assistance input with injected git and assistance
    /// clients for project-level conflict tests.
    fn build_sync_rebase_input_for_test(
        folder: PathBuf,
        git_client: Arc<dyn GitClient>,
        sync_assist_client: Arc<dyn SyncAssistClient>,
    ) -> SyncRebaseAssistInput {
        SyncRebaseAssistInput {
            base_branch: "main".to_string(),
            folder,
            fs_client: test_fs_client(),
            git_client,
            session_model: AgentModel::AntigravityGemini3FlashPreview,
            sync_assist_client,
        }
    }

    #[tokio::test]
    /// Ensures [`SessionError`] from the sync assist client propagates through
    /// `run_sync_rebase_assist_agent` with an operation-specific context
    /// prefix.
    async fn test_sync_rebase_assist_agent_adds_context_to_workflow_error() {
        // Arrange
        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(1)
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(SessionError::Workflow(
                        "agent backend unavailable".to_string(),
                    ))
                })
            });
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(git::MockGitClient::new()),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let result =
            SessionManager::run_sync_rebase_assist_agent(&input, &["src/lib.rs".to_string()]).await;

        // Assert
        let error = result.expect_err("assist failure should propagate");
        assert!(
            matches!(error, SessionError::Workflow(_)),
            "expected SessionError::Workflow, got: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Sync rebase assistance failed: agent backend unavailable"
        );
    }

    #[test]
    fn test_rebase_assist_prompt_includes_branch_and_files() {
        // Arrange
        let base_branch = "main";
        let conflicted_files = vec!["src/lib.rs".to_string(), "README.md".to_string()];

        // Act
        let prompt = SessionManager::rebase_assist_prompt(base_branch, &conflicted_files)
            .expect("rebase assist prompt should render");

        // Assert
        assert!(prompt.contains("rebasing onto `main`"));
        assert!(prompt.contains("- src/lib.rs"));
        assert!(prompt.contains("- README.md"));
        assert!(prompt.contains("repository-defined quality checks"));
        assert!(prompt.contains("affected dependencies or dependents"));
    }

    #[test]
    fn test_format_conflicted_file_list_returns_bulleted_lines() {
        // Arrange
        let conflicted_files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];

        // Act
        let summary = SessionManager::format_conflicted_file_list(&conflicted_files);

        // Assert
        assert_eq!(summary, "- src/main.rs\n- src/lib.rs");
    }

    #[test]
    fn test_session_title_from_commit_message() {
        // Arrange
        let commit_message = "Refine merge flow\n\n- Update title handling";

        // Act
        let title = SessionManager::session_title_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Refine merge flow");
    }

    #[test]
    fn test_session_title_from_commit_message_skips_blank_prefix() {
        // Arrange
        let commit_message = "\n\nRefine merge flow\n\n- Update title handling";

        // Act
        let title = SessionManager::session_title_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Refine merge flow");
    }

    #[test]
    fn test_session_title_from_commit_message_empty_uses_fallback() {
        // Arrange
        let commit_message = "  \n";

        // Act
        let title = SessionManager::session_title_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Apply session updates");
    }

    #[test]
    fn test_session_summary_with_commit_message_builds_markdown_sections() {
        // Arrange
        let session_summary = Some("- Session branch now handles refresh races.");
        let commit_message = "Refine session summary\n\n- Append commit context";

        // Act
        let summary =
            SessionManager::session_summary_with_commit_message(session_summary, commit_message);

        // Assert
        assert_eq!(
            summary,
            "# Summary\n\n- Session branch now handles refresh races.\n\n# Commit\n\nRefine \
             session summary\n\n- Append commit context"
        );
    }

    #[test]
    fn test_session_summary_with_commit_message_formats_empty_summary_section() {
        // Arrange
        let session_summary = Some("   ");
        let commit_message = "Refine session summary";

        // Act
        let summary =
            SessionManager::session_summary_with_commit_message(session_summary, commit_message);

        // Assert
        assert_eq!(
            summary,
            "# Summary\n\n\n\n# Commit\n\nRefine session summary"
        );
    }

    #[test]
    fn test_session_summary_with_commit_message_extracts_session_text_from_json_payload() {
        // Arrange
        let session_summary = Some(
            r#"{"turn":"Updated the greeting flow.","session":"Session now greets users on startup."}"#,
        );
        let commit_message = "Refine session summary";

        // Act
        let summary =
            SessionManager::session_summary_with_commit_message(session_summary, commit_message);

        // Assert
        assert_eq!(
            summary,
            "# Summary\n\nSession now greets users on startup.\n\n# Commit\n\nRefine session \
             summary"
        );
    }

    #[tokio::test]
    async fn test_update_session_title_from_commit_message_preserves_existing_summary() {
        // Arrange
        let database = AppRepositories::in_memory().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session(
                "session-id",
                AgentModel::ClaudeSonnet46.as_str(),
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let existing_summary = "- Session branch updates README.";
        database
            .sessions()
            .update_session_summary("session-id", existing_summary)
            .await
            .expect("failed to persist existing summary");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let commit_message = "Refine session commit message\n\n- Keep title in sync";

        // Act
        SessionManager::update_session_title_from_commit_message(
            &database,
            "session-id",
            commit_message,
            &app_event_tx,
        )
        .await;
        let sessions = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(
            sessions[0].title.as_deref(),
            Some("Refine session commit message")
        );
        assert_eq!(sessions[0].summary.as_deref(), Some(existing_summary));
        assert_eq!(
            app_event_rx.try_recv().ok(),
            Some(AppEvent::RefreshSessions)
        );
    }

    #[tokio::test]
    async fn test_update_done_session_summary_from_commit_message_appends_commit_message() {
        // Arrange
        let database = AppRepositories::in_memory().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session(
                "session-id",
                AgentModel::ClaudeSonnet46.as_str(),
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let existing_summary = "- Session branch updates README.";
        let commit_message = "Refine session commit message\n\n- Keep title in sync";
        database
            .sessions()
            .update_session_summary("session-id", existing_summary)
            .await
            .expect("failed to persist existing summary");

        // Act
        SessionManager::update_done_session_summary_from_commit_message(
            &database,
            "session-id",
            commit_message,
        )
        .await;
        let sessions = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(
            sessions[0].summary.as_deref(),
            Some(
                "# Summary\n\n- Session branch updates README.\n\n# Commit\n\nRefine session \
                 commit message\n\n- Keep title in sync"
            )
        );
    }

    #[tokio::test]
    async fn test_execute_merge_workflow_reuses_session_head_commit_message() {
        // Arrange
        let expected_merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
        let canonical_commit_message = "Refine merge flow\n\n- Reuse the session commit body";
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock_git_client
            .expect_squash_merge_diff()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _| Box::pin(async { Ok("diff --git a/file b/file".to_string()) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let canonical_commit_message = canonical_commit_message.to_string();

                move |_| {
                    let canonical_commit_message = canonical_commit_message.clone();

                    Box::pin(async move { Ok(Some(canonical_commit_message)) })
                }
            });
        mock_git_client
            .expect_squash_merge()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let canonical_commit_message = canonical_commit_message.to_string();

                move |_, _, _, commit_message| {
                    let canonical_commit_message = canonical_commit_message.clone();

                    Box::pin(async move {
                        assert_eq!(commit_message, canonical_commit_message);

                        Ok(git::SquashMergeOutcome::Committed)
                    })
                }
            });
        mock_git_client
            .expect_head_hash()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let expected_merged_commit_hash = expected_merged_commit_hash.to_string();

                move |_| {
                    let expected_merged_commit_hash = expected_merged_commit_hash.clone();

                    Box::pin(async move { Ok(expected_merged_commit_hash) })
                }
            });
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) = build_merge_task_input_for_test(Arc::new(mock_git_client)).await;
        let project_id = input
            .db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        input
            .db
            .sessions()
            .insert_session("session-123", "gpt-5.5", "main", "Merging", project_id)
            .await
            .expect("failed to insert merge session row");
        let db = input.db.clone();

        // Act
        let result = SessionManager::execute_merge_workflow(input).await;

        // Assert
        let message = result.expect("merge workflow should succeed");
        assert_eq!(message, "Successfully merged wt/session-123 into main");
        let merged_commit_hash = db
            .sessions()
            .load_session_merged_commit_hash("session-123")
            .await
            .expect("failed to load merged commit hash");
        assert_eq!(
            merged_commit_hash.as_deref(),
            Some(expected_merged_commit_hash)
        );
    }

    #[tokio::test]
    async fn test_execute_merge_workflow_skips_commit_creation_for_empty_squash_diff() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock_git_client
            .expect_squash_merge_diff()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _| Box::pin(async { Ok("   ".to_string()) }));
        mock_git_client.expect_head_commit_message().times(0);
        mock_git_client.expect_squash_merge().times(0);
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) = build_merge_task_input_for_test(Arc::new(mock_git_client)).await;
        let project_id = input
            .db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        input
            .db
            .sessions()
            .insert_session("session-123", "gpt-5.5", "main", "Merging", project_id)
            .await
            .expect("failed to insert merge session row");
        let db = input.db.clone();

        // Act
        let result = SessionManager::execute_merge_workflow(input).await;

        // Assert
        let message = result.expect("merge workflow should succeed for empty diff");
        assert_eq!(
            message,
            "Session changes from wt/session-123 are already present in main"
        );
        let merged_commit_hash = db
            .sessions()
            .load_session_merged_commit_hash("session-123")
            .await
            .expect("failed to load merged commit hash");
        assert_eq!(merged_commit_hash, None);
    }

    #[tokio::test]
    async fn test_rebase_assist_input_clone() {
        // Arrange
        let (tx, _rx) = mpsc::unbounded_channel();
        let db = AppRepositories::in_memory().await;
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = RebaseAssistInput {
            app_event_tx: tx,
            assist_mode: RebaseAssistMode::OneShot,
            child_pid: Arc::new(Mutex::new(None)),
            db,
            folder: temp_dir.path().to_path_buf(),
            fs_client: test_fs_client(),
            git_client: Arc::new(git::RealGitClient),
            id: "session-123".into(),
            output: Arc::new(Mutex::new(String::new())),
            rebase_target: "origin/main".to_string(),
            session_model: AgentModel::AntigravityGemini3FlashPreview,
            session_update_versions: Arc::default(),
        };

        // Act
        let cloned_input = input.clone();

        // Assert
        assert_eq!(input.id, cloned_input.id);
        assert_eq!(input.folder, cloned_input.folder);
        assert_eq!(input.rebase_target, cloned_input.rebase_target);
        assert_eq!(input.session_model, cloned_input.session_model);
    }

    #[tokio::test]
    async fn test_resolve_session_rebase_target_keeps_local_base_for_unpublished_session() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let project_path = temp_dir.path().to_string_lossy().to_string();
        let project_id = db
            .projects()
            .upsert_project(&project_path, Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess-local", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client.expect_fetch_remote().times(0);
        let folder = temp_dir.path().join("sess-local");

        // Act
        let rebase_target = SessionManager::resolve_session_rebase_target(
            &db,
            &mock_git_client,
            &folder,
            "sess-local",
            "main",
        )
        .await
        .expect("failed to resolve local rebase target");

        // Assert
        assert_eq!(rebase_target, "main");
    }

    #[tokio::test]
    async fn test_resolve_session_rebase_target_fetches_remote_base_for_published_session() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let project_path = temp_dir.path().to_string_lossy().to_string();
        let project_id = db
            .projects()
            .upsert_project(&project_path, Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess-remote", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_published_upstream_ref(
                "sess-remote",
                Some("origin/wt/sess-remote".to_string()),
            )
            .await
            .expect("failed to set published upstream");
        let folder = temp_dir.path().join("sess-remote");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_fetch_remote()
            .once()
            .withf(|repo_path| repo_path.ends_with("sess-remote"))
            .returning(|_| Box::pin(async { Ok(()) }));

        // Act
        let rebase_target = SessionManager::resolve_session_rebase_target(
            &db,
            &mock_git_client,
            &folder,
            "sess-remote",
            "main",
        )
        .await
        .expect("failed to resolve remote rebase target");

        // Assert
        assert_eq!(rebase_target, "origin/main");
    }

    #[tokio::test]
    async fn test_resolve_session_rebase_target_reports_published_fetch_failure() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let project_path = temp_dir.path().to_string_lossy().to_string();
        let project_id = db
            .projects()
            .upsert_project(&project_path, Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess-fetch", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_published_upstream_ref(
                "sess-fetch",
                Some("origin/wt/sess-fetch".to_string()),
            )
            .await
            .expect("failed to set published upstream");
        let folder = temp_dir.path().join("sess-fetch");
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_fetch_remote()
            .once()
            .returning(|_| Box::pin(async { Err(GitError::OutputParse("fetch failed".into())) }));

        // Act
        let result = SessionManager::resolve_session_rebase_target(
            &db,
            &mock_git_client,
            &folder,
            "sess-fetch",
            "main",
        )
        .await;

        // Assert
        let error = result.expect_err("fetch failure should stop published rebase");
        assert!(
            error
                .to_string()
                .contains("Failed to fetch `origin` before rebasing published session branch"),
            "error should name the published upstream remote"
        );
    }

    #[tokio::test]
    async fn test_execute_rebase_workflow_aborts_when_assist_loop_fails() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|_, base_branch| base_branch == "origin/main")
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(Some("Existing session commit".to_string())) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|_, base_branch, _, _, _| base_branch == "origin/main")
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async { Err(GitError::OutputParse("state query failed".to_string())) })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, mut input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;
        input.rebase_target = "origin/main".to_string();

        // Act
        let result = SessionManager::execute_rebase_workflow(input).await;

        // Assert
        let error = result.expect_err("rebase workflow should fail");
        assert!(
            error
                .to_string()
                .contains("Failed to sync: state query failed"),
            "workflow error should include assist-loop failure reason"
        );
    }

    #[tokio::test]
    async fn test_run_rebase_assist_loop_core_aborts_on_early_error() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "failed to list conflicts".to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            None,
        )
        .await;

        // Assert
        let error = result.expect_err("assist loop should fail");
        assert_eq!(error.to_string(), "failed to list conflicts");
    }

    /// Verifies session rebase assistance stops when the same conflict detail
    /// repeats after the initial conflict state.
    #[tokio::test]
    async fn test_run_rebase_assist_loop_core_stops_on_repeated_conflict_detail() {
        // Arrange
        let repeated_detail = "CONFLICT (content): Merge conflict in src/lib.rs".to_string();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_rebase_continue()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning({
                let repeated_detail = repeated_detail.clone();

                move |_| {
                    let repeated_detail = repeated_detail.clone();

                    Box::pin(async move {
                        Ok(git::RebaseStepResult::Conflict {
                            detail: repeated_detail,
                        })
                    })
                }
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            Some(repeated_detail.clone()),
        )
        .await;

        // Assert
        let error = result.expect_err("assist loop should stop on repeated conflict detail");
        assert_eq!(
            error.to_string(),
            format!(
                "Rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {repeated_detail}"
            )
        );
    }

    /// Verifies the first `git rebase` conflict detail seeds no-progress
    /// tracking for the session rebase loop.
    #[tokio::test]
    async fn test_run_rebase_assist_loop_tracks_initial_rebase_conflict_detail() {
        // Arrange
        let repeated_detail = "CONFLICT (content): Merge conflict in src/lib.rs".to_string();
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let repeated_detail = repeated_detail.clone();

                move |_, _| {
                    let repeated_detail = repeated_detail.clone();

                    Box::pin(async move {
                        Ok(git::RebaseStepResult::Conflict {
                            detail: repeated_detail,
                        })
                    })
                }
            });
        for _ in 0..REBASE_ASSIST_POLICY.max_attempts {
            mock_git_client
                .expect_list_conflicted_files()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Box::pin(async { Ok(Vec::new()) }));
            mock_git_client
                .expect_list_staged_conflict_marker_files()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
            mock_git_client
                .expect_rebase_continue()
                .times(1)
                .in_sequence(&mut sequence)
                .returning({
                    let repeated_detail = repeated_detail.clone();

                    move |_| {
                        let repeated_detail = repeated_detail.clone();

                        Box::pin(async move {
                            Ok(git::RebaseStepResult::Conflict {
                                detail: repeated_detail,
                            })
                        })
                    }
                });
        }
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop(input).await;

        // Assert
        let error = result.expect_err("assist loop should stop on repeated conflict detail");
        assert_eq!(
            error.to_string(),
            format!(
                "Rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {repeated_detail}"
            )
        );
    }

    /// Verifies session rebase assistance surfaces the final conflict detail
    /// when every retry hits a distinct conflict state until the retry budget
    /// is exhausted.
    #[tokio::test]
    async fn test_run_rebase_assist_loop_core_reports_retry_exhaustion_detail() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        for detail in [
            "CONFLICT (content): Merge conflict in src/lib.rs",
            "CONFLICT (content): Merge conflict in src/main.rs",
            "CONFLICT (content): Merge conflict in README.md",
        ] {
            mock_git_client
                .expect_list_conflicted_files()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Box::pin(async { Ok(Vec::new()) }));
            mock_git_client
                .expect_list_staged_conflict_marker_files()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
            mock_git_client
                .expect_rebase_continue()
                .times(1)
                .in_sequence(&mut sequence)
                .returning({
                    let detail = detail.to_string();

                    move |_| {
                        let detail = detail.clone();

                        Box::pin(async move { Ok(git::RebaseStepResult::Conflict { detail }) })
                    }
                });
        }
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            None,
        )
        .await;

        // Assert
        let error = result.expect_err("assist loop should report the final retry conflict");
        assert_eq!(
            error.to_string(),
            "Rebase still has conflicts after assistance: CONFLICT (content): Merge conflict in \
             README.md"
        );
    }

    #[tokio::test]
    async fn test_run_rebase_start_recovers_stale_rebase_state_and_retries() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "fatal: It seems that there is already a rebase-merge directory"
                            .to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_start(&input).await;

        // Assert
        let step_result = result.expect("rebase start should succeed");
        assert_eq!(step_result, git::RebaseStepResult::Completed);
    }

    #[tokio::test]
    async fn test_run_rebase_start_uses_resolved_rebase_target() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_rebase_start()
            .once()
            .withf(|_, rebase_target| rebase_target == "origin/main")
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        let (_temp_dir, mut input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;
        input.rebase_target = "origin/main".to_string();

        // Act
        let result = SessionManager::run_rebase_start(&input).await;

        // Assert
        let step_result = result.expect("rebase start should succeed");
        assert_eq!(step_result, git::RebaseStepResult::Completed);
    }

    #[tokio::test]
    async fn test_run_rebase_start_reports_cleanup_failure_for_stale_rebase_state() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "fatal: It seems that there is already a rebase-merge directory"
                            .to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async { Err(GitError::OutputParse("abort failed".to_string())) })
            });
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_start(&input).await;

        // Assert
        let error = result.expect_err("cleanup failure should stop retry flow");
        assert!(
            error
                .to_string()
                .contains("Cleanup with `git rebase --abort` failed: abort failed"),
            "error should include abort failure detail"
        );
    }

    /// Verifies merged-session cleanup surfaces branch deletion failures after
    /// the worktree itself has already been removed.
    #[tokio::test]
    async fn test_cleanup_merged_session_worktree_reports_delete_branch_failure() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().join("session-worktree");
        let repo_root = temp_dir.path().join("repo-root");
        let source_branch = "wt/session-123".to_string();
        let mut mock_git_client = git::MockGitClient::new();
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Err(GitError::OutputParse("delete failed".to_string())) })
            });
        mock_fs_client.expect_remove_dir_all().times(0);

        // Act
        let result = SessionManager::cleanup_merged_session_worktree(
            folder,
            Arc::new(mock_fs_client),
            Arc::new(mock_git_client),
            source_branch,
            Some(repo_root),
        )
        .await;

        // Assert
        let error = result.expect_err("cleanup should fail on branch deletion error");
        assert_eq!(error.to_string(), "delete failed");
    }

    #[test]
    fn test_detail_message_for_uncommitted_changes_uses_sentence_lines() {
        // Arrange
        let sync_error = SyncSessionStartError::MainHasUncommittedChanges {
            default_branch: "main".to_string(),
        };

        // Act
        let detail_message = sync_error.detail_message();

        // Assert
        assert_eq!(
            detail_message,
            "Sync cannot run while `main` has uncommitted changes.\nCommit or stash changes in \
             `main`, then try again."
        );
    }

    #[tokio::test]
    async fn test_ensure_merge_target_clean_blocks_dirty_main_checkout() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        db.sessions()
            .insert_session("session-123", "gpt-5.5", "main", "Merging", project_id)
            .await
            .expect("failed to insert merge session row");
        let status = Arc::new(Mutex::new(Status::Merging));
        let session_update_versions = Arc::default();
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        let restore_context = MergeStartRestoreContext {
            app_event_tx: &app_event_tx,
            clock: &crate::infra::clock::RealClock,
            db: &db,
            session_id: "session-123",
            session_update_versions: &session_update_versions,
            status: &status,
        };

        // Act
        let result = SessionMergeService::ensure_merge_target_clean(
            &mock_git_client,
            PathBuf::from("/tmp/project"),
            "main",
            &restore_context,
        )
        .await;

        // Assert
        let error = result.expect_err("dirty merge target should block merge");
        assert_eq!(
            error.to_string(),
            "Merge cannot run while `main` has uncommitted changes.\nCommit or stash changes in \
             `main`, then try again."
        );
        assert_eq!(
            *status.lock().expect("status lock poisoned"),
            Status::Review
        );
    }

    #[tokio::test]
    async fn test_sync_main_for_project_resolves_conflicts_with_assistance() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let working_dir = temp_dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(|folder| Box::pin(async move { Some(folder) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_get_ahead_behind()
            .times(1)
            .return_once(|_| Box::pin(async { Ok((1, 2)) }));
        mock_git_client
            .expect_list_upstream_commit_titles()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(vec![
                        "Update changelog format".to_string(),
                        "Fix sync popup copy".to_string(),
                    ])
                })
            });
        mock_git_client
            .expect_get_ahead_behind()
            .times(1)
            .return_once(|_| Box::pin(async { Ok((1, 0)) }));
        mock_git_client
            .expect_list_local_commit_titles()
            .times(1)
            .returning(|_| {
                Box::pin(async { Ok(vec!["Refine sync conflict messaging".to_string()]) })
            });
        mock_git_client
            .expect_pull_rebase()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(git::PullRebaseResult::Conflict {
                        detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                    })
                })
            });
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(2)
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        mock_git_client
            .expect_stage_all()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_continue()
            .times(1)
            .returning(|_| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock_git_client
            .expect_push_current_branch()
            .times(1)
            .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
        mock_git_client.expect_abort_rebase().times(0);

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        // Act
        let result = SessionManager::sync_main_for_project_with_assist_client(
            Some("main".to_string()),
            working_dir,
            test_fs_client(),
            Arc::new(mock_git_client),
            AgentModel::AntigravityGemini3FlashPreview,
            Arc::new(mock_sync_assist_client),
        )
        .await;

        // Assert
        assert_eq!(
            result,
            Ok(SyncMainOutcome {
                pulled_commit_titles: vec![
                    "Update changelog format".to_string(),
                    "Fix sync popup copy".to_string(),
                ],
                pulled_commits: Some(2),
                pushed_commit_titles: vec!["Refine sync conflict messaging".to_string()],
                pushed_commits: Some(1),
                resolved_conflict_files: vec!["src/lib.rs".to_string()],
            }),
            "sync should succeed after assistance with summary details"
        );
    }

    #[tokio::test]
    async fn test_sync_main_for_project_fails_after_max_assistance_attempts() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let working_dir = temp_dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(|folder| Box::pin(async move { Some(folder) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_get_ahead_behind()
            .times(1)
            .returning(|_| Box::pin(async { Ok((0, 1)) }));
        mock_git_client
            .expect_list_upstream_commit_titles()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec!["Upstream patch".to_string()]) }));
        mock_git_client
            .expect_pull_rebase()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(git::PullRebaseResult::Conflict {
                        detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                    })
                })
            });
        mock_git_client
            .expect_list_conflicted_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        mock_git_client
            .expect_stage_all()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client.expect_rebase_continue().times(0);
        mock_git_client.expect_push_current_branch().times(0);
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        // Act
        let result = SessionManager::sync_main_for_project_with_assist_client(
            Some("main".to_string()),
            working_dir,
            test_fs_client(),
            Arc::new(mock_git_client),
            AgentModel::AntigravityGemini3FlashPreview,
            Arc::new(mock_sync_assist_client),
        )
        .await;

        // Assert
        let error = result.expect_err("sync should fail when conflicts remain unresolved");
        assert!(matches!(error, SyncSessionStartError::Other(_)));
        assert!(
            error
                .detail_message()
                .contains("Conflicts remain unresolved after maximum assistance attempts"),
            "error detail should mention unresolved conflicts"
        );
    }

    /// Verifies sync assistance merges tracked conflicts with staged conflict
    /// marker files and returns a sorted unique list.
    #[tokio::test]
    async fn test_load_sync_conflicted_files_merges_and_sorts_results() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .returning(|_| {
                Box::pin(async { Ok(vec!["src/b.rs".to_string(), "src/c.rs".to_string()]) })
            });
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Ok(vec!["src/a.rs".to_string(), "src/c.rs".to_string()]) })
            });

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(0);
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let conflicted_files = SessionManager::load_sync_conflicted_files(&input, &[]).await;

        // Assert
        let files = conflicted_files.expect("load_sync_conflicted_files should succeed");
        assert_eq!(
            files,
            vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ]
        );
    }

    /// Verifies sync conflict checks keep the loop in assistance mode when
    /// staged files still contain conflict markers.
    #[tokio::test]
    async fn test_stage_and_check_for_sync_conflicts_detects_remaining_markers() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_stage_all()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(0);
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let still_has_conflicts =
            SessionManager::stage_and_check_for_sync_conflicts(&input, &["src/lib.rs".to_string()])
                .await;

        // Assert
        let has_conflicts = still_has_conflicts.expect("stage_and_check should succeed");
        assert!(has_conflicts);
    }

    /// Verifies sync assistance aborts when the conflicted file fingerprint
    /// repeats across attempts without any file changes.
    #[tokio::test]
    async fn test_run_sync_rebase_assist_loop_aborts_for_unchanged_conflict_files() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let conflict_file = temp_dir.path().join("src/lib.rs");
        std::fs::create_dir_all(
            conflict_file
                .parent()
                .expect("conflict file should have a parent directory"),
        )
        .expect("create conflict directory");
        std::fs::write(&conflict_file, "<<<<<<< HEAD\none\n=======\ntwo\n>>>>>>>")
            .expect("write conflict file");

        let fingerprint_fs_client = create_passthrough_mock_fs_client();
        let fingerprint = SessionManager::conflicted_file_fingerprint(
            &fingerprint_fs_client,
            temp_dir.path(),
            &["src/lib.rs".to_string()],
        )
        .await;

        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        mock_git_client
            .expect_stage_all()
            .times(REBASE_ASSIST_POLICY.max_attempts - 1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(REBASE_ASSIST_POLICY.max_attempts - 1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(REBASE_ASSIST_POLICY.max_attempts - 1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let result = SessionManager::run_sync_rebase_assist_loop(input, fingerprint).await;

        // Assert
        assert_eq!(
            result.map_err(|error| error.to_string()),
            Err(
                "Sync rebase assistance made no progress: conflicted files did not change"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_changes_with_file_content() {
        // Arrange
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_content_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("conflict.rs");
        let files = vec!["conflict.rs".to_string()];

        // Act
        std::fs::write(&file_path, "<<<<<<< HEAD\nfoo\n=======\nbar\n>>>>>>>")
            .expect("write initial content");
        let fingerprint_before =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
        std::fs::write(
            &file_path,
            "<<<<<<< HEAD\nfoo_patched\n=======\nbar\n>>>>>>>",
        )
        .expect("write patched content");
        let fingerprint_after =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

        // Assert — partial progress changes the fingerprint
        assert_ne!(fingerprint_before, fingerprint_after);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_stable_for_unchanged_content() {
        // Arrange
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_stable_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        std::fs::write(temp_dir.join("conflict.rs"), "same content").expect("write file");
        let files = vec!["conflict.rs".to_string()];

        // Act
        let fingerprint_a =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
        let fingerprint_b =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

        // Assert — identical content produces identical fingerprint
        assert_eq!(fingerprint_a, fingerprint_b);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_order_independent() {
        // Arrange
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_order_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        std::fs::write(temp_dir.join("a.rs"), "content a").expect("write a.rs");
        std::fs::write(temp_dir.join("b.rs"), "content b").expect("write b.rs");

        // Act
        let fingerprint_ab = SessionManager::conflicted_file_fingerprint(
            &fs_client,
            &temp_dir,
            &["a.rs".to_string(), "b.rs".to_string()],
        )
        .await;
        let fingerprint_ba = SessionManager::conflicted_file_fingerprint(
            &fs_client,
            &temp_dir,
            &["b.rs".to_string(), "a.rs".to_string()],
        )
        .await;

        // Assert — order of file list does not affect the fingerprint
        assert_eq!(fingerprint_ab, fingerprint_ba);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_missing_file_is_stable() {
        // Arrange — reference a file that does not exist on disk
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_missing_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let files = vec!["nonexistent.rs".to_string()];

        // Act — should not panic; missing files are silently skipped
        let fingerprint_a =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
        let fingerprint_b =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

        // Assert — deterministic even when file is absent
        assert_eq!(fingerprint_a, fingerprint_b);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    /// Verifies that a successful rebase triggers an auto-push and reports the
    /// successful sync state when the session has a previously published
    /// upstream branch.
    async fn test_finalize_rebase_task_triggers_auto_push_for_published_branch() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess-rebase",
                "gemini-3-flash-preview",
                "main",
                "Rebasing",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_published_upstream_ref(
                "sess-rebase",
                Some("origin/wt/sess-rebase".to_string()),
            )
            .await
            .expect("failed to set published upstream ref");

        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let temp_dir = tempdir().expect("failed to create temp dir");
        let folder = temp_dir.path().join("sess-rebase");
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new(Status::Rebasing));

        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|session_folder, remote_branch_name| {
                session_folder.ends_with("sess-rebase") && remote_branch_name == "wt/sess-rebase"
            })
            .returning(|_, _| Box::pin(async { Ok("origin/wt/sess-rebase".to_string()) }));
        let git_client: Arc<dyn GitClient> = Arc::new(mock_git_client);

        // Act
        SessionManager::finalize_rebase_task(FinalizeRebaseInput {
            app_event_tx: &app_event_tx,
            clock: &crate::infra::clock::RealClock,
            db: &db,
            folder: &folder,
            git_client: &git_client,
            id: "sess-rebase",
            output: &output,
            rebase_result: Ok("Successfully synced wt/sess-rebase onto main".to_string()),
            session_update_versions: &Arc::default(),
            status: &status,
        })
        .await;

        // Assert — collect sync events emitted by the auto-push task.
        let sync_events = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated {
                    session_id,
                    sync_operation_id,
                    sync_status,
                } = event
                {
                    sync_events.push((session_id, sync_operation_id, sync_status));
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");

        assert_eq!(sync_events[0].0, "sess-rebase");
        assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
        assert_eq!(sync_events[1].0, "sess-rebase");
        assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
        assert_eq!(sync_events[0].1, sync_events[1].1);

        let output_text = output.lock().expect("output lock poisoned").clone();
        assert!(output_text.contains("[Sync] Successfully synced"));
    }

    #[tokio::test]
    /// Verifies that a successful rebase does not trigger auto-push when the
    /// session has no published upstream branch.
    async fn test_finalize_rebase_task_skips_auto_push_without_published_branch() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess-no-push",
                "gemini-3-flash-preview",
                "main",
                "Rebasing",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let temp_dir = tempdir().expect("failed to create temp dir");
        let folder = temp_dir.path().join("sess-no-push");
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new(Status::Rebasing));
        let git_client: Arc<dyn GitClient> = Arc::new(git::MockGitClient::new());

        // Act
        SessionManager::finalize_rebase_task(FinalizeRebaseInput {
            app_event_tx: &app_event_tx,
            clock: &crate::infra::clock::RealClock,
            db: &db,
            folder: &folder,
            git_client: &git_client,
            id: "sess-no-push",
            output: &output,
            rebase_result: Ok("Successfully synced wt/sess-no-push onto main".to_string()),
            session_update_versions: &Arc::default(),
            status: &status,
        })
        .await;

        // Assert — no PublishedBranchSyncUpdated events should be emitted.
        // Drain remaining events and check that none are sync events.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut sync_event_count = 0;
        while let Ok(event) = app_event_rx.try_recv() {
            if matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }) {
                sync_event_count += 1;
            }
        }
        assert_eq!(
            sync_event_count, 0,
            "should not emit sync events without published branch"
        );

        let output_text = output.lock().expect("output lock poisoned").clone();
        assert!(output_text.contains("[Sync] Successfully synced"));
    }
}
