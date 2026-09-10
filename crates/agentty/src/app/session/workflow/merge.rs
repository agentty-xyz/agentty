//! Merge, rebase, and cleanup workflows for session branches.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use ag_agent::{self as agent, OneShotClient};
use ag_forge as forge;
use ag_git::{self as git, GitClient};
use askama::Template;
use tokio::sync::{OwnedMutexGuard, mpsc};
use tracing::warn;

use super::published_branch::{self, PublishedBranchAutoPushInput};
use super::worker::{SessionCommand, has_unfinished_rebase_operation};
use super::{SessionTaskService, StatusTransition, session_branch};
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError};
use crate::app::sync::SyncMainEventContext;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel};
use crate::domain::session::{PublishedBranchSyncStatus, SessionId, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::AppRepositories;
use crate::infra::fs::{self as fs, FsClient};

const REBASE_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 3,
    // Allow up to 3 consecutive identical-content observations before
    // giving up, so the agent gets a genuine second chance when partial
    // progress is made inside a file without fully clearing all markers.
    max_identical_failure_streak: 3,
};

/// Askama view model for rendering rebase conflict-assistance prompts.
#[derive(Template)]
#[template(path = "rebase_assist_prompt.md", escape = "none")]
struct RebaseAssistPromptTemplate<'a> {
    base_branch: &'a str,
    conflicted_files: &'a str,
    workspace_note: &'a str,
}

/// Identifies which repository checkout a rebase-conflict assist runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebaseAssistWorkspace {
    /// The session's isolated git worktree used by session merge rebases.
    SessionWorktree,
    /// The user's main repository checkout used by user-triggered sync
    /// rebases.
    MainCheckout,
}

impl RebaseAssistWorkspace {
    /// Returns the prompt note that tells the assist agent which checkout it
    /// is editing and how far its write access extends.
    fn note(self) -> &'static str {
        match self {
            Self::SessionWorktree => {
                "You are working inside the session's isolated git worktree; keep every change \
                 inside this worktree and never touch the repository's main checkout."
            }
            Self::MainCheckout => {
                "You are working inside the user's main repository checkout for a user-approved \
                 sync; change only the conflicted files listed below."
            }
        }
    }
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

/// Git rebase shape for one assisted session rebase.
#[derive(Clone, Debug, Eq, PartialEq)]
enum RebasePlan {
    /// Runs a standard `git rebase <target>` workflow.
    Target {
        /// Branch or ref that receives the current branch commits.
        target: String,
    },
    /// Runs `git rebase --onto <new_base> <old_base>` to replay only commits
    /// after a recorded stack base.
    Onto {
        /// Ref that receives the replayed commits.
        new_base: String,
        /// Recorded parent/base commit that should be left behind.
        old_base: String,
    },
}

impl RebasePlan {
    /// Builds a standard target rebase plan.
    fn target(target: String) -> Self {
        Self::Target { target }
    }

    /// Returns the destination ref shown to users and used for pre-rebase
    /// auto-commit base detection.
    fn target_label(&self) -> &str {
        match self {
            Self::Target { target } => target,
            Self::Onto { new_base, .. } => new_base,
        }
    }

    /// Returns the recorded stack base for `--onto` rebases, when present.
    fn old_base(&self) -> Option<&str> {
        match self {
            Self::Target { .. } => None,
            Self::Onto { old_base, .. } => Some(old_base),
        }
    }
}

/// Shared context needed to restore a failed merge-start attempt back to
/// `Review`.
struct MergeStartRestoreContext<'a> {
    /// Repository boundary used to load merge-start metadata.
    db: &'a AppRepositories,
    /// Session whose merge-start attempt is being restored.
    session_id: &'a str,
    /// Bound transition context used for best-effort rollback.
    status_transition: &'a StatusTransition,
}

struct MergeTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    archive_diff: bool,
    base_branch: String,
    child_pid: Arc<Mutex<Option<u32>>>,
    clock: Arc<dyn Clock>,
    db: AppRepositories,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    id: SessionId,
    one_shot_client: Arc<dyn OneShotClient>,
    repo_root: PathBuf,
    session_agent: AgentSelection,
    session_update_versions: SessionUpdateVersionMap,
    source_branch: String,
    status: Arc<Mutex<Status>>,
    transcript: Arc<Mutex<SessionTranscript>>,
}

/// Metadata needed to finish a successful merge after git work completes.
struct SuccessfulMergeCompletion<'a> {
    /// Event channel used for status and stacked-child rebase notifications.
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    /// Canonical session commit message used for title and summary sync.
    authoritative_commit_message: Option<&'a str>,
    /// Branch that received the squash merge.
    base_branch: &'a str,
    /// Repository boundary for session metadata updates.
    db: &'a AppRepositories,
    /// Session that finished merging.
    id: &'a SessionId,
    /// Commit hash created on the target branch, when the merge produced one.
    merged_commit_hash: Option<&'a str>,
    /// Pre-merge parent tip used as the old base for child restacks.
    parent_commit_hash: String,
    /// Bound transition context for moving the merged session to `Done`.
    status_transition: &'a StatusTransition,
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
    one_shot_client: Arc<dyn OneShotClient>,
    rebase_plan: RebasePlan,
    session_agent: AgentSelection,
    session_update_versions: SessionUpdateVersionMap,
    transcript: Arc<Mutex<SessionTranscript>>,
}

/// Input required to run a session rebase command to completion.
pub(super) struct RebaseCommandInput {
    /// Reducer event sender used for status and transcript updates.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Agent submission mode used when rebase conflicts need edits.
    pub(super) assist_mode: RebaseAssistMode,
    /// Stored base branch for the session worktree.
    pub(super) base_branch: String,
    /// Serializes post-rebase publish ownership with other queued/running
    /// branch operations for the same session.
    pub(super) branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
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
    /// Session identifier receiving transcript/status updates.
    pub(super) id: SessionId,
    /// Provider-neutral boundary used by pre-rebase auto-commit prompts.
    pub(super) one_shot_client: Arc<dyn OneShotClient>,
    /// Forge boundary used for optional linked PR/MR metadata refresh after
    /// post-rebase auto-push.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Agent/model selection used by agent-assisted conflict prompts.
    pub(super) session_agent: AgentSelection,
    /// Per-app update versions for targeted session refresh events.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Shared session lifecycle status.
    pub(super) status: Arc<Mutex<Status>>,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

/// Bundled context for finalizing one rebase task.
struct FinalizeRebaseInput<'a> {
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    /// Retains branch-operation ownership through rebase finalization and any
    /// post-rebase auto-push.
    branch_operation_guard: OwnedMutexGuard<()>,
    clock: &'a Arc<dyn Clock>,
    db: &'a AppRepositories,
    folder: &'a Path,
    git_client: &'a Arc<dyn GitClient>,
    id: &'a str,
    one_shot_client: &'a Arc<dyn OneShotClient>,
    rebase_result: Result<String, SessionError>,
    review_request_client: &'a Arc<dyn forge::ReviewRequestClient>,
    session_agent: AgentSelection,
    session_update_versions: &'a SessionUpdateVersionMap,
    /// Bound transition context for restoring `Review` after rebase cleanup.
    status_transition: &'a StatusTransition,
    transcript: &'a Arc<Mutex<SessionTranscript>>,
}

/// Bundled context for starting post-rebase auto-publish.
struct RebaseAutoPushInput<'a> {
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    /// Transfers branch-operation ownership from rebase to auto-push.
    branch_operation_guard: OwnedMutexGuard<()>,
    clock: &'a Arc<dyn Clock>,
    db: &'a AppRepositories,
    folder: &'a Path,
    git_client: &'a Arc<dyn GitClient>,
    one_shot_client: &'a Arc<dyn OneShotClient>,
    review_request_client: &'a Arc<dyn forge::ReviewRequestClient>,
    session_agent: AgentSelection,
    session_id: &'a str,
    session_update_versions: &'a SessionUpdateVersionMap,
    transcript: &'a Arc<Mutex<SessionTranscript>>,
}

/// Bundled context for finalizing one merge task.
struct FinalizeMergeInput<'a> {
    app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    db: &'a AppRepositories,
    id: &'a str,
    result: Result<String, SessionError>,
    session_update_versions: &'a SessionUpdateVersionMap,
    /// Bound transition context for restoring failed merges to `Review`.
    status_transition: &'a StatusTransition,
    transcript: &'a Arc<Mutex<SessionTranscript>>,
}

/// Input context for assisted conflict resolution during `sync main`.
struct SyncRebaseAssistInput {
    base_branch: String,
    event_context: Option<SyncMainEventContext>,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    session_agent: AgentSelection,
    sync_assist_client: Arc<dyn SyncAssistClient>,
}

/// Polymorphic input for shared assisted rebase loop orchestration.
///
/// The session variant is boxed because its input carries the whole repository
/// bundle plus per-turn client handles, which makes it far larger than the
/// project variant.
enum RebaseAssistLoopInput {
    Session(Box<RebaseAssistInput>),
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

    /// Returns the git boundary used by assist operations.
    fn git_client(&self) -> &dyn GitClient {
        match self {
            Self::Session(input) => input.git_client.as_ref(),
            Self::Project(input) => input.git_client.as_ref(),
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
        SessionManager::load_rebase_conflicted_files(
            self.git_client(),
            self.folder(),
            previous_conflict_files,
        )
        .await
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
                SessionManager::emit_sync_rebase_conflict_status(input, conflicted_files);
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
        SessionManager::stage_and_check_rebase_conflicts(
            self.git_client(),
            self.folder(),
            conflict_files,
        )
        .await
    }

    /// Runs the effective pre-commit hook against the staged conflict
    /// resolution.
    ///
    /// # Errors
    /// Returns an error when the hook rejects the staged changes or cannot be
    /// executed.
    async fn run_pre_commit_hook(&self) -> Result<(), SessionError> {
        self.git_client()
            .run_pre_commit_hook(self.folder().to_path_buf())
            .await
            .map_err(|error| {
                SessionError::Workflow(format!(
                    "Pre-commit hook rejected resolved rebase conflicts: {error}"
                ))
            })
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
        session_agent: AgentSelection,
    ) -> SyncAssistFuture<Result<(), SessionError>>;
}

/// Production sync-assistance executor backed by real agent commands.
struct RealSyncAssistClient {
    one_shot_client: Arc<dyn OneShotClient>,
}

impl RealSyncAssistClient {
    /// Creates the production sync-assistance executor.
    fn new() -> Self {
        Self {
            one_shot_client: Arc::new(agent::RealOneShotClient::new(None)),
        }
    }

    /// Runs one sync conflict assistance command through the shared one-shot
    /// agent submission path.
    ///
    /// # Errors
    /// Returns an error when the one-shot agent command fails.
    async fn run_assist_command(
        one_shot_client: &dyn OneShotClient,
        folder: PathBuf,
        prompt: String,
        session_agent: AgentSelection,
    ) -> Result<(), SessionError> {
        // Success payload unused; run for side effects only.
        let _ = one_shot_client
            .submit(agent::OneShotRequest {
                provider_call_budget: None,
                agent_kind: session_agent.kind(),
                child_pid: None,
                folder,
                model: session_agent.model(),
                permission_mode: ag_agent::PermissionMode::AutoEdit,
                prompt,
                request_kind: ag_agent::AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: crate::domain::agent::SpeedMode::Normal,
            })
            .await?;

        Ok(())
    }
}

impl SyncAssistClient for RealSyncAssistClient {
    fn resolve_rebase_conflicts(
        &self,
        folder: PathBuf,
        prompt: String,
        session_agent: AgentSelection,
    ) -> SyncAssistFuture<Result<(), SessionError>> {
        let one_shot_client = Arc::clone(&self.one_shot_client);

        Box::pin(async move {
            Self::run_assist_command(one_shot_client.as_ref(), folder, prompt, session_agent).await
        })
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
    /// Local branch updated by the completed manual sync.
    pub(crate) default_branch: String,
    /// Merged sessions that could not be archived after the branch update.
    pub(crate) deferred_merged_session_ids: Vec<SessionId>,
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

/// Coordinates merge/rebase session workflows behind a dedicated service
/// boundary.
pub(crate) struct SessionMergeService;

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
        if !session.owns_branch_changes() {
            return Err(SessionError::Workflow(
                "Orchestrator sessions do not own branch changes".to_string(),
            ));
        }
        if !(session.status.allows_review_actions() || session.status == Status::Queued) {
            return Err(SessionError::Workflow(
                "Session must be in review or queued status".to_string(),
            ));
        }
        if !manager.can_merge_session_branch_in_stack(session_id) {
            return Err(SessionError::Workflow(
                "Merge cannot run for linked review requests or while another stack session is \
                 active"
                    .to_string(),
            ));
        }

        let (archive_diff, db, folder, id, session_agent) = (
            session.is_managed(),
            services.db().clone(),
            session.folder.clone(),
            session.id.clone(),
            session.agent,
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
        let (child_pid, status, transcript) = (
            Arc::clone(&handles.child_pid),
            Arc::clone(&handles.status),
            Arc::clone(&handles.transcript),
        );
        let status_transition = StatusTransition::from_parts(
            app_event_tx.clone(),
            Arc::clone(&clock),
            db.clone(),
            id.clone(),
            Arc::clone(&session_update_versions),
            Arc::clone(&status),
        );
        let restore_context = MergeStartRestoreContext {
            db: &db,
            session_id: &id,
            status_transition: &status_transition,
        };
        status_transition
            .apply_or_invalid_transition(Status::Merging)
            .await?;

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
            archive_diff,
            base_branch,
            child_pid,
            clock,
            db,
            folder,
            fs_client,
            git_client,
            id: id.clone(),
            one_shot_client: services.one_shot_client(),
            repo_root,
            session_agent,
            session_update_versions,
            source_branch: session_branch(&id),
            status,
            transcript,
        };
        tokio::spawn(async move {
            SessionManager::run_merge_task(merge_task_input).await;
        });

        Ok(())
    }

    /// Restores a failed merge-start attempt to `Review` status without
    /// masking the original error.
    async fn restore_review_status(context: &MergeStartRestoreContext<'_>) {
        if !context.status_transition.apply(Status::Review).await {
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

    /// Starts or queues a session branch rebase onto its base branch.
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
        let (base_branch, current_status, persisted_session_id) =
            Self::load_rebase_start_context(manager, services, session_id).await?;
        let branch_operation_lock = manager
            .session_handles_or_err(session_id)
            .map(|handles| Arc::clone(&handles.branch_operation_lock))
            .map_err(|_| SessionError::HandlesNotFound)?;
        // Reserve an idle branch while the operation is persisted. An active
        // owner already serializes execution in the worker, so the foreground
        // event loop must never wait here.
        let _branch_operation_guard = branch_operation_lock.try_lock_owned().ok();

        let command = SessionCommand::Rebase {
            base_branch,
            operation_id: uuid::Uuid::new_v4().to_string(),
        };
        if current_status == Status::InProgress {
            let unfinished_operations = services
                .db()
                .operations()
                .load_unfinished_session_operations()
                .await?;
            if has_unfinished_rebase_operation(
                &unfinished_operations,
                persisted_session_id.as_str(),
            ) {
                return Ok(());
            }

            let queued_order = manager
                .worker_service_mut()
                .enqueue_existing_session_command(services, &persisted_session_id, command)
                .await?;
            manager.queue_session_sync(persisted_session_id.as_str(), queued_order);
        } else {
            let handles = manager
                .session_handles_or_err(session_id)
                .map_err(|_| SessionError::HandlesNotFound)?;
            let status_transition =
                StatusTransition::from_services(services, handles, persisted_session_id.clone());

            status_transition
                .apply_or_invalid_transition(Status::Rebasing)
                .await?;
            Self::enqueue_with_status_rollback(
                manager,
                services,
                &status_transition,
                &persisted_session_id,
                command,
                current_status,
            )
            .await?;
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

    /// Starts automatic deterministic rebases for children retargeted by a
    /// completed parent merge.
    async fn rebase_sessions_after_parent_merge(
        &self,
        manager: &mut SessionManager,
        services: &AppServices,
        child_session_ids: Vec<SessionId>,
    ) -> Vec<(SessionId, SessionError)> {
        let mut failures = Vec::new();
        for child_session_id in child_session_ids {
            let should_rebase_child = manager
                .sessions()
                .iter()
                .find(|session| session.id == child_session_id)
                .is_some_and(|session| session.status.allows_review_actions());
            if !should_rebase_child {
                continue;
            }

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
        let status_transition =
            StatusTransition::from_services(services, handles, persisted_session_id.clone());
        status_transition
            .apply_or_invalid_transition(Status::Rebasing)
            .await?;

        let command = SessionCommand::Rebase {
            base_branch,
            operation_id: uuid::Uuid::new_v4().to_string(),
        };
        Self::enqueue_with_status_rollback(
            manager,
            services,
            &status_transition,
            &persisted_session_id,
            command,
            previous_status,
        )
        .await?;

        Ok(())
    }

    /// Enqueues a worker command and restores the previous status if the
    /// queue handoff cannot be recorded or delivered.
    ///
    /// # Errors
    /// Returns the enqueue failure after the best-effort rollback attempt.
    async fn enqueue_with_status_rollback(
        manager: &mut SessionManager,
        services: &AppServices,
        status_transition: &StatusTransition,
        session_id: &SessionId,
        command: SessionCommand,
        fallback_status: Status,
    ) -> Result<(), SessionError> {
        if let Err(error) = manager
            .enqueue_session_command(services, session_id, command)
            .await
        {
            let _ = status_transition.apply(fallback_status).await;

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
    ) -> Result<(String, Status, SessionId), SessionError> {
        let session = manager
            .session_or_err(session_id)
            .map_err(|_| SessionError::NotFound)?;
        if !session.status.allows_review_actions() && session.status != Status::InProgress {
            return Err(SessionError::Workflow(
                "Session must be in review status or in progress".to_string(),
            ));
        }
        if !manager.can_rebase_session_branch_in_stack(session_id) {
            return Err(SessionError::Workflow(
                "Stacked sync can only run when no other stack session is active".to_string(),
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

        Ok((base_branch, session.status, session.id.clone()))
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
        let clock = Arc::clone(&input.clock);
        let db = input.db.clone();
        let app_event_tx = input.app_event_tx.clone();
        let id = input.id.clone();
        let status = Arc::clone(&input.status);
        let session_update_versions = input.session_update_versions.clone();
        let transcript = Arc::clone(&input.transcript);

        let merge_result = Self::execute_merge_workflow(input).await;
        let status_transition = StatusTransition::from_parts(
            app_event_tx.clone(),
            Arc::clone(&clock),
            db.clone(),
            id.clone(),
            Arc::clone(&session_update_versions),
            Arc::clone(&status),
        );
        Self::finalize_merge_task(FinalizeMergeInput {
            app_event_tx: &app_event_tx,
            db: &db,
            id: &id,
            result: merge_result,
            session_update_versions: &session_update_versions,
            status_transition: &status_transition,
            transcript: &transcript,
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
            archive_diff,
            base_branch,
            clock,
            db,
            folder,
            fs_client,
            git_client,
            id,
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
        let parent_commit_hash = git_client.head_hash(folder.clone()).await?;

        let squash_diff = Self::load_squash_diff(
            git_client.as_ref(),
            repo_root.clone(),
            source_branch.clone(),
            base_branch.clone(),
        )
        .await?;
        if archive_diff {
            db.sessions()
                .update_session_archived_diff(&id, Some(squash_diff.clone()))
                .await?;
        }
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

        let status_transition = StatusTransition::from_parts(
            app_event_tx.clone(),
            Arc::clone(&clock),
            db.clone(),
            id.clone(),
            Arc::clone(&session_update_versions),
            Arc::clone(&status),
        );

        Self::complete_successful_merge(SuccessfulMergeCompletion {
            app_event_tx: &app_event_tx,
            authoritative_commit_message: authoritative_commit_message.as_deref(),
            base_branch: &base_branch,
            db: &db,
            id: &id,
            merged_commit_hash: merged_commit_hash.as_deref(),
            parent_commit_hash,
            status_transition: &status_transition,
        })
        .await?;

        Ok(Self::merge_success_message(
            &source_branch,
            &base_branch,
            merge_outcome,
        ))
    }

    /// Persists merge metadata, retargets stacked children, emits follow-up
    /// restack events, and moves the parent session to `Done`.
    ///
    /// # Errors
    /// Returns an error when metadata persistence, child restacking, or the
    /// final status transition fails.
    async fn complete_successful_merge(
        input: SuccessfulMergeCompletion<'_>,
    ) -> Result<(), SessionError> {
        Self::persist_merged_session_metadata(
            input.db,
            input.id,
            input.authoritative_commit_message,
            input.merged_commit_hash,
            input.app_event_tx,
        )
        .await?;
        let restacked_child_session_ids = Self::restack_child_sessions_after_parent_merge(
            input.db,
            input.id,
            input.base_branch,
            Some(input.parent_commit_hash),
        )
        .await?;
        Self::emit_stacked_parent_merge_completed(input.app_event_tx, restacked_child_session_ids);

        input
            .status_transition
            .apply_or_invalid_transition(Status::Done)
            .await
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
        parent_commit_hash: Option<String>,
    ) -> Result<Vec<SessionId>, SessionError> {
        let child_session_ids = db
            .sessions()
            .restack_child_sessions_after_parent_merge(
                parent_session_id,
                base_branch,
                parent_commit_hash,
            )
            .await?
            .into_iter()
            .map(SessionId::from)
            .collect();

        Ok(child_session_ids)
    }

    /// Emits the reducer event that starts post-merge child restack rebases.
    pub(crate) fn emit_stacked_parent_merge_completed(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        child_session_ids: Vec<SessionId>,
    ) {
        if child_session_ids.is_empty() {
            return;
        }

        if app_event_tx
            .send(AppEvent::StackedParentMergeCompleted { child_session_ids })
            .is_err()
        {
            warn!(
                "failed to enqueue post-merge stacked child restacks because the app event \
                 receiver is closed"
            );
        }
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
            one_shot_client: Arc::clone(&input.one_shot_client),
            rebase_plan: RebasePlan::target(input.base_branch.clone()),
            session_agent: input.session_agent,
            session_update_versions: input.session_update_versions.clone(),
            transcript: Arc::clone(&input.transcript),
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
            app_event_tx,
            db,
            id,
            result,
            session_update_versions,
            status_transition,
            transcript,
        } = input;

        match result {
            Ok(message) => {
                let merge_message = TranscriptNotice::Merge.format_line(message);
                SessionTaskService::emit_session_workflow_notice(app_event_tx, id, merge_message);
                SessionTaskService::request_git_status_refresh(app_event_tx);
            }
            Err(error) => {
                let merge_error = TranscriptNotice::MergeError.format(error);
                SessionTaskService::append_workflow_notice(
                    transcript,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    &merge_error,
                )
                .await;
                if !status_transition.apply(Status::Review).await {
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

    /// Starts deterministic restack rebases for children whose parent merged.
    pub(crate) async fn rebase_sessions_after_parent_merge(
        &mut self,
        services: &AppServices,
        child_session_ids: Vec<SessionId>,
    ) -> Vec<(SessionId, SessionError)> {
        SessionMergeService
            .rebase_sessions_after_parent_merge(self, services, child_session_ids)
            .await
    }

    /// Re-enqueues durable post-merge stack restacks that were interrupted
    /// after persistence had already recorded the stack-base hash.
    pub(crate) async fn rebase_pending_stack_restacks_for_project(
        &mut self,
        services: &AppServices,
        project_id: i64,
    ) -> Vec<(SessionId, SessionError)> {
        let child_session_ids = services
            .db()
            .sessions()
            .load_pending_stack_restack_session_ids(project_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(SessionId::from)
            .collect();

        self.rebase_sessions_after_parent_merge(services, child_session_ids)
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
        event_context: Option<SyncMainEventContext>,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
    ) -> Result<SyncMainOutcome, SyncSessionStartError> {
        let fs_client: Arc<dyn FsClient> = Arc::new(fs::RealFsClient);
        let sync_assist_client: Arc<dyn SyncAssistClient> = Arc::new(RealSyncAssistClient::new());
        let session_agent = crate::domain::agent::resolve_agent_selection_for_model(
            session_model,
            AgentKind::Antigravity,
            AgentKind::ALL,
        );

        Self::sync_main_for_project_with_assist_client(
            default_branch,
            working_dir,
            event_context,
            fs_client,
            git_client,
            session_agent,
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
        event_context: Option<SyncMainEventContext>,
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn GitClient>,
        session_agent: AgentSelection,
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
                event_context,
                base_branch: default_branch.clone(),
                folder: working_dir.clone(),
                fs_client: Arc::clone(&fs_client),
                git_client: Arc::clone(&git_client),
                session_agent,
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
            default_branch,
            deferred_merged_session_ids: Vec::new(),
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

    /// Emits a non-modal sync update for files currently under assistance.
    fn emit_sync_rebase_conflict_status(
        input: &SyncRebaseAssistInput,
        conflicted_files: &[String],
    ) {
        let Some(event_context) = &input.event_context else {
            return;
        };

        let _ = event_context
            .app_event_tx
            .send(AppEvent::SyncMainConflictResolutionStarted {
                conflicted_files: conflicted_files.to_vec(),
                operation: event_context.operation.clone(),
            });
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

    /// Executes one agent-assisted sync rebase conflict resolution attempt.
    ///
    /// # Errors
    /// Returns an error when the assistance command fails.
    async fn run_sync_rebase_assist_agent(
        input: &SyncRebaseAssistInput,
        conflicted_files: &[String],
    ) -> Result<(), SessionError> {
        let prompt = Self::rebase_assist_prompt(
            &input.base_branch,
            conflicted_files,
            RebaseAssistWorkspace::MainCheckout,
        )?;
        input
            .sync_assist_client
            .resolve_rebase_conflicts(input.folder.clone(), prompt, input.session_agent)
            .await
            .map_err(|error| error.with_context("Sync rebase assistance failed"))
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
            branch_operation_lock,
            child_pid,
            clock,
            db,
            folder,
            fs_client,
            git_client,
            id,
            one_shot_client,
            review_request_client,
            session_agent,
            session_update_versions,
            status,
            transcript,
        } = input;
        let branch_operation_guard = branch_operation_lock.lock_owned().await;

        let status_transition = StatusTransition::from_parts(
            app_event_tx.clone(),
            Arc::clone(&clock),
            db.clone(),
            id.clone(),
            Arc::clone(&session_update_versions),
            Arc::clone(&status),
        );
        status_transition
            .apply_or_invalid_transition(Status::Rebasing)
            .await?;
        let _ = app_event_tx.send(AppEvent::SessionQueuedSyncResolved {
            session_id: id.clone(),
        });

        let rebase_result: Result<String, SessionError> = async {
            let rebase_plan = Self::resolve_session_rebase_plan(
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
                one_shot_client: Arc::clone(&one_shot_client),
                rebase_plan,
                session_agent,
                session_update_versions: session_update_versions.clone(),
                transcript: Arc::clone(&transcript),
            };

            Self::execute_rebase_workflow(rebase_input).await
        }
        .await;
        let command_error = rebase_result.as_ref().err().map(ToString::to_string);

        Self::finalize_rebase_task(FinalizeRebaseInput {
            app_event_tx: &app_event_tx,
            branch_operation_guard,
            clock: &clock,
            db: &db,
            folder: &folder,
            git_client: &git_client,
            id: &id,
            one_shot_client: &one_shot_client,
            rebase_result,
            review_request_client: &review_request_client,
            session_agent,
            session_update_versions: &session_update_versions,
            status_transition: &status_transition,
            transcript: &transcript,
        })
        .await;

        if let Some(error) = command_error {
            return Err(SessionError::Workflow(error));
        }

        Ok(())
    }

    /// Resolves the git rebase plan used for one session branch.
    ///
    /// Unpublished sessions rebase against the stored local base branch. Once
    /// a session branch is published, the rebase first fetches and targets the
    /// remote base ref from the same remote as the published upstream so the
    /// pull request comparison is updated against the forge-visible base. When
    /// the session has a recorded stack-base commit, the plan uses
    /// `git rebase --onto` so parent commits are left behind deterministically.
    ///
    /// # Errors
    /// Returns an error when the published-session fetch fails or stack-base
    /// metadata cannot be loaded.
    async fn resolve_session_rebase_plan(
        db: &AppRepositories,
        git_client: &dyn GitClient,
        folder: &Path,
        session_id: &str,
        base_branch: &str,
    ) -> Result<RebasePlan, SessionError> {
        let rebase_target =
            Self::resolve_session_rebase_target(db, git_client, folder, session_id, base_branch)
                .await?;
        let stack_base_commit_hash = db
            .sessions()
            .get_session_stack_base_commit_hash(session_id)
            .await
            .map_err(SessionError::Db)?;

        Ok(match stack_base_commit_hash {
            Some(old_base) => RebasePlan::Onto {
                new_base: rebase_target,
                old_base,
            },
            None => RebasePlan::target(rebase_target),
        })
    }

    /// Resolves the git ref used as the destination for one session rebase.
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
    /// The pre-sync auto-commit reuses the active project's fast agent/model
    /// default when generating or repairing the session commit message, and
    /// a successful commit requests an immediate git-status refresh.
    async fn execute_rebase_workflow(input: RebaseAssistInput) -> Result<String, SessionError> {
        // Auto-commit any pending changes before rebasing to avoid
        // "cannot rebase: You have unstaged changes".
        let include_coauthored_by_agentty =
            SessionTaskService::load_include_coauthored_by_agentty_setting(&input.db, &input.id)
                .await;
        let auto_commit_agent = SessionTaskService::load_auto_commit_agent_setting(
            &input.db,
            &input.id,
            input.session_agent,
        )
        .await;
        let auto_commit_reasoning_level =
            SessionTaskService::load_auto_commit_reasoning_level(&input.db, &input.id).await;
        let auto_commit_speed_mode =
            SessionTaskService::load_auto_commit_speed_mode(&input.db, &input.id).await;
        match SessionTaskService::commit_session_changes(
            input.git_client.as_ref(),
            &input.folder,
            input.rebase_plan.target_label(),
            (
                auto_commit_agent,
                auto_commit_reasoning_level,
                auto_commit_speed_mode,
            ),
            input.one_shot_client.as_ref(),
            include_coauthored_by_agentty,
            input.transcript.as_ref(),
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
        Self::persist_stack_base_after_successful_rebase(&input).await?;

        let source_branch = session_branch(&input.id);
        let rebase_target = input.rebase_plan.target_label();

        Ok(format!(
            "Successfully synced {source_branch} onto {rebase_target}"
        ))
    }

    /// Updates stack-base metadata after a successful child rebase.
    ///
    /// Stacked children keep the resolved target hash so the next parent
    /// movement can use `git rebase --onto <new-parent> <old-parent-tip>`.
    /// Restacked children whose parent link has already been cleared drop the
    /// hash after the deterministic `--onto` completes.
    ///
    /// # Errors
    /// Returns an error when stack metadata cannot be loaded or persisted.
    async fn persist_stack_base_after_successful_rebase(
        input: &RebaseAssistInput,
    ) -> Result<(), SessionError> {
        let parent_session_id = input
            .db
            .sessions()
            .get_session_parent_session_id(&input.id)
            .await
            .map_err(SessionError::Db)?;

        if parent_session_id.is_some() {
            let target_hash = input
                .git_client
                .ref_hash(
                    input.folder.clone(),
                    input.rebase_plan.target_label().to_string(),
                )
                .await?;
            input
                .db
                .sessions()
                .update_session_stack_base_commit_hash(&input.id, Some(target_hash))
                .await
                .map_err(SessionError::Db)?;

            return Ok(());
        }

        if input.rebase_plan.old_base().is_some() {
            input
                .db
                .sessions()
                .update_session_stack_base_commit_hash(&input.id, None)
                .await
                .map_err(SessionError::Db)?;
        }

        Ok(())
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
            branch_operation_guard,
            clock,
            db,
            folder,
            git_client,
            id,
            one_shot_client,
            rebase_result,
            review_request_client,
            session_agent,
            session_update_versions,
            status_transition,
            transcript,
        } = input;

        let rebase_succeeded = rebase_result.is_ok();
        match rebase_result {
            Ok(message) => {
                let rebase_message = TranscriptNotice::Rebase.format(message);
                SessionTaskService::append_workflow_notice(
                    transcript,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    &rebase_message,
                )
                .await;
                SessionTaskService::request_git_status_refresh(app_event_tx);

                Self::start_auto_push_after_rebase(RebaseAutoPushInput {
                    app_event_tx,
                    branch_operation_guard,
                    clock,
                    db,
                    folder,
                    git_client,
                    one_shot_client,
                    review_request_client,
                    session_agent,
                    session_id: id,
                    session_update_versions,
                    transcript,
                })
                .await;
            }
            Err(error) => {
                let rebase_error = TranscriptNotice::RebaseError.format(error);
                SessionTaskService::append_workflow_notice(
                    transcript,
                    db,
                    app_event_tx,
                    session_update_versions,
                    id,
                    &rebase_error,
                )
                .await;
            }
        }

        let restored_review_status = status_transition.apply(Status::Review).await;
        if !restored_review_status {
            warn!(
                session_id = id,
                "skipped restoring review status after rebase because the in-memory status was \
                 already current"
            );
        }
        if rebase_succeeded && restored_review_status {
            let _ = app_event_tx.send(AppEvent::StackedParentSyncCompleted {
                session_id: SessionId::from(id),
            });
        }
    }

    /// Starts a detached auto-push task when the rebased session has a
    /// previously published upstream branch.
    async fn start_auto_push_after_rebase(input: RebaseAutoPushInput<'_>) {
        let RebaseAutoPushInput {
            app_event_tx,
            branch_operation_guard,
            clock,
            db,
            folder,
            git_client,
            one_shot_client,
            review_request_client,
            session_agent,
            session_id,
            session_update_versions,
            transcript,
        } = input;

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
                persistent_notice: None,
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
        let review_request_metadata_sync = Some(published_branch::ReviewRequestMetadataSyncInput {
            clock: Arc::clone(clock),
            commit_message: None,
            evaluation: published_branch::ReviewRequestMetadataEvaluationInput {
                one_shot_client: Arc::clone(one_shot_client),
                session_agent,
            },
            review_request_client: Arc::clone(review_request_client),
        });

        let app_event_tx = app_event_tx.clone();
        let db = db.clone();
        let folder = folder.to_path_buf();
        let git_client = Arc::clone(git_client);
        let transcript = Arc::clone(transcript);
        let session_id = SessionId::from(session_id);
        let auto_push_input = PublishedBranchAutoPushInput {
            app_event_tx,
            db,
            folder,
            git_client,
            published_upstream_ref,
            review_request_client: Arc::clone(review_request_client),
            review_request_metadata_sync,
            session_id,
            session_update_versions: session_update_versions.clone(),
            sync_operation_id,
            transcript,
        };

        tokio::spawn(async move {
            let _branch_operation_guard = branch_operation_guard;
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
            RebaseAssistLoopInput::Session(Box::new(input)),
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
        assist_input.run_pre_commit_hook().await?;
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
        let rebase_plan = input.rebase_plan.clone();
        match Self::start_git_rebase(input.git_client.as_ref(), folder.clone(), &rebase_plan).await
        {
            Ok(result) => Ok(result),
            Err(error) => {
                let error_string = error.to_string();
                if !Self::is_stale_rebase_state_error(&error_string) {
                    return Err(SessionError::Workflow(error_string));
                }

                Self::recover_from_stale_rebase_start_error(input, &error_string).await?;

                Self::start_git_rebase(input.git_client.as_ref(), folder, &rebase_plan)
                    .await
                    .map_err(SessionError::Git)
            }
        }
    }

    /// Starts the concrete git rebase command described by `rebase_plan`.
    ///
    /// # Errors
    /// Returns an error when git cannot start the requested rebase.
    async fn start_git_rebase(
        git_client: &dyn GitClient,
        folder: PathBuf,
        rebase_plan: &RebasePlan,
    ) -> Result<git::RebaseStepResult, git::GitError> {
        match rebase_plan {
            RebasePlan::Target { target } => git_client.rebase_start(folder, target.clone()).await,
            RebasePlan::Onto { new_base, old_base } => {
                git_client
                    .rebase_onto_start(folder, new_base.clone(), old_base.clone())
                    .await
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
    async fn load_rebase_conflicted_files(
        git_client: &dyn GitClient,
        folder: &Path,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, SessionError> {
        let folder = folder.to_path_buf();
        let mut conflicted = git_client.list_conflicted_files(folder.clone()).await?;

        let staged_with_markers = git_client
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
        let prompt = Self::rebase_assist_prompt(
            input.rebase_plan.target_label(),
            conflicted_files,
            RebaseAssistWorkspace::SessionWorktree,
        )?;
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
    async fn stage_and_check_rebase_conflicts(
        git_client: &dyn GitClient,
        folder: &Path,
        conflict_files: &[String],
    ) -> Result<bool, SessionError> {
        let folder = folder.to_path_buf();
        git_client.stage_all(folder.clone()).await?;

        if git_client.has_unmerged_paths(folder.clone()).await? {
            return Ok(true);
        }

        let staged_with_markers = git_client
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
        workspace: RebaseAssistWorkspace,
    ) -> Result<String, SessionError> {
        let conflicted_files = Self::format_conflicted_file_list(conflicted_files);
        let template = RebaseAssistPromptTemplate {
            base_branch,
            conflicted_files: &conflicted_files,
            workspace_note: workspace.note(),
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
            one_shot_client: Arc::clone(&input.one_shot_client),
            session_agent: input.session_agent,
            session_update_versions: input.session_update_versions.clone(),
            transcript: Arc::clone(&input.transcript),
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
#[path = "merge_test.rs"]
mod tests;
