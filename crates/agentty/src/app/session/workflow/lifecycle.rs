//! Session lifecycle workflows and direct user actions.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ag_agent::{self as agent, AgentRequestKind, OneShotClient};
use ag_forge as forge;
use ag_git as git;
use ag_protocol::{AgentResponse, parse_agent_response_strict};
use askama::Template;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

use super::task::SessionTranscriptMessageAppend;
use super::worker::{SessionCommand, TurnMetadata};
use super::{
    SessionTaskService, StatusTransition, draft, isolation, session_branch, session_folder,
    unix_timestamp_from_system_time,
};
use crate::app::session::{SessionCreationKind, SessionCreationSettings, SessionError};
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager, agentty_home, setting};
use crate::domain::agent::{
    AgentKind, AgentSelection, AgentSelectionMetadata, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{
    QueuedMessage, ReviewRequest, SESSION_DATA_DIR, Session, SessionHandles, SessionId, Status,
    can_append_session_to_stack as stack_can_append_session,
    can_create_stacked_child as stack_can_create_stacked_child,
    can_merge_session_branch_in_stack as stack_can_merge_session_branch,
    can_mutate_session_branch_in_stack as stack_can_mutate_session_branch,
    can_rebase_session_branch_in_stack as stack_can_rebase_session_branch,
    can_reply_to_session_in_stack as stack_can_reply_to_session,
    can_start_staged_session_in_stack as stack_can_start_staged_session,
    has_reserved_branch_work_in_stack,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::session_order;
use crate::domain::setting::SettingName;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::infra::db;
use crate::infra::fs::{FsClient, FsError};

/// Maximum accepted length for generated session titles.
///
/// Longer candidates are treated as likely non-title prose instead of being
/// truncated into misleading session labels.
const GENERATED_SESSION_TITLE_MAX_CHARACTERS: usize = 72;
/// Marker appended when persisted context is shortened for title generation.
const SESSION_TITLE_CONTEXT_TRUNCATION_MARKER: &str = "\n[title context truncated]";
/// Maximum current-title bytes supplied to title generation.
const SESSION_TITLE_CURRENT_TITLE_MAX_BYTES: usize = 512;
/// Maximum provider submissions for one session-title generation request.
const SESSION_TITLE_GENERATION_MAX_ATTEMPTS: usize = 2;
/// Maximum unwrapped title prompt size, reserving transport-envelope headroom.
const SESSION_TITLE_GENERATION_PROMPT_MAX_BYTES: usize = 24 * 1024;
/// Source-template bytes included in every title-generation prompt.
const SESSION_TITLE_GENERATION_TEMPLATE_BYTES: usize =
    include_str!("../../template/session_title_generation_prompt.md").len();
/// Maximum latest-request bytes supplied to title generation.
const SESSION_TITLE_LATEST_REQUEST_MAX_BYTES: usize = 8 * 1024;
/// Maximum original-request bytes supplied to title generation.
const SESSION_TITLE_ORIGINAL_REQUEST_MAX_BYTES: usize = 8 * 1024;
const _: () = assert!(
    SESSION_TITLE_GENERATION_TEMPLATE_BYTES
        + SESSION_TITLE_CURRENT_TITLE_MAX_BYTES
        + SESSION_TITLE_LATEST_REQUEST_MAX_BYTES
        + SESSION_TITLE_ORIGINAL_REQUEST_MAX_BYTES
        <= SESSION_TITLE_GENERATION_PROMPT_MAX_BYTES
);
/// Progress/status prefixes that indicate the model returned process prose
/// instead of a requested-work title.
const GENERATED_SESSION_TITLE_PROGRESS_PREFIXES: &[&str] = &[
    "checking ",
    "confirming ",
    "gathering ",
    "inspecting ",
    "investigating ",
    "reviewing ",
    "validating ",
    "working ",
];
const USER_PROMPT_PREFIX: &str = " › ";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";

/// Input bag for constructing a queued session command.
struct BuildSessionCommandInput {
    is_first_message: bool,
    operation_id: Option<String>,
    prompt: TurnPrompt,
    published_upstream_ref: Option<String>,
    replay_transcript: Option<String>,
    review_comment_thread_ids: Vec<String>,
    session_agent: AgentSelection,
}

/// Intermediate values captured while preparing a session reply.
type ReplyContext = (Option<String>, bool, SessionId, Option<String>);

/// Status policy applied while preparing one reply command.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ReplyEligibility {
    /// Accept the normal draft, question, and review-ready reply states.
    Standard,
    /// Accept a structured question answer during turn finalization or while
    /// the session is already waiting in `Question`.
    QuestionAnswer,
}

/// Capability used to authorize and stop one terminal cancellation.
#[derive(Clone, Copy, Eq, PartialEq)]
enum CancellationCapability {
    /// Cancellation initiated through the ordinary user action.
    User,
    /// Coordinator-only cancellation of a managed worker.
    Managed,
    /// Forced cancellation of a descendant after its stack parent is canceled.
    StackedDescendant,
}

impl CancellationCapability {
    /// Returns whether this capability can cancel the current session state.
    fn allows(self, session: &Session) -> bool {
        match self {
            Self::User => session.allows_cancel_action(),
            Self::Managed => {
                session.allows_cancel_action()
                    || (session.is_managed()
                        && (matches!(session.status, Status::Draft | Status::InProgress)
                            || session.status.allows_review_actions()))
            }
            Self::StackedDescendant => {
                session.status != Status::Canceled
                    && session.status.can_transition_to(Status::Canceled)
            }
        }
    }
}

impl ReplyEligibility {
    /// Returns whether this reply kind can run from the current status.
    fn allows(self, status: Status, is_first_message: bool) -> bool {
        match self {
            Self::Standard => {
                status.allows_review_actions()
                    || status == Status::Question
                    || (is_first_message && status == Status::Draft)
            }
            Self::QuestionAnswer => matches!(status, Status::InProgress | Status::Question),
        }
    }
}

/// Transcript and output treatment for a submitted reply prompt.
#[derive(Clone, Copy)]
enum ReplyPromptPresentation {
    /// Persist and render a normal user prompt.
    Visible,
    /// Persist generated agent context without rendering it in chat.
    HiddenAgent,
}

impl ReplyPromptPresentation {
    /// Returns the durable transcript kind for this presentation mode.
    fn message_kind(self) -> SessionMessageKind {
        match self {
            Self::Visible => SessionMessageKind::UserPrompt,
            Self::HiddenAgent => SessionMessageKind::AgentPrompt,
        }
    }

    /// Returns whether the submitted prompt should render in session output.
    fn is_visible(self) -> bool {
        matches!(self, Self::Visible)
    }
}

/// Reply-command behavior selected by the caller.
struct ReplyOptions {
    defer_prompt_until_enqueued: bool,
    eligibility: ReplyEligibility,
    operation_id: Option<String>,
    persist_prompt: bool,
    prompt_presentation: ReplyPromptPresentation,
    requires_existing_worker: bool,
    review_comment_thread_ids: Vec<String>,
}

impl ReplyOptions {
    /// Builds the normal reply behavior with optional review-thread targets.
    fn standard(review_comment_thread_ids: Vec<String>) -> Self {
        Self {
            defer_prompt_until_enqueued: false,
            eligibility: ReplyEligibility::Standard,
            operation_id: None,
            persist_prompt: true,
            prompt_presentation: ReplyPromptPresentation::Visible,
            requires_existing_worker: false,
            review_comment_thread_ids,
        }
    }

    /// Builds structured question-answer behavior for the current worker
    /// state.
    fn question_answer(requires_existing_worker: bool) -> Self {
        Self {
            defer_prompt_until_enqueued: true,
            eligibility: ReplyEligibility::QuestionAnswer,
            operation_id: None,
            persist_prompt: true,
            prompt_presentation: ReplyPromptPresentation::Visible,
            requires_existing_worker,
            review_comment_thread_ids: Vec::new(),
        }
    }

    /// Builds idempotent coordinator-turn behavior for one durable operation.
    fn coordinator(operation_id: String, persist_prompt: bool) -> Self {
        Self {
            defer_prompt_until_enqueued: true,
            eligibility: ReplyEligibility::Standard,
            operation_id: Some(operation_id),
            persist_prompt,
            prompt_presentation: ReplyPromptPresentation::Visible,
            requires_existing_worker: false,
            review_comment_thread_ids: Vec::new(),
        }
    }

    /// Builds hidden generated-prompt behavior for forge review comments.
    fn review_comments(review_comment_thread_ids: Vec<String>) -> Self {
        Self {
            defer_prompt_until_enqueued: false,
            eligibility: ReplyEligibility::Standard,
            operation_id: None,
            persist_prompt: true,
            prompt_presentation: ReplyPromptPresentation::HiddenAgent,
            requires_existing_worker: false,
            review_comment_thread_ids,
        }
    }
}

/// Result of attempting to persist and enqueue one reply command.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ReplyEnqueueOutcome {
    /// A prior attempt already durably accepted the same operation.
    AlreadyAccepted,
    /// The command was newly persisted and reached the worker queue.
    Enqueued,
    /// Persistence or worker delivery failed.
    Failed,
}

/// Worker-queue behavior selected after reply preparation.
struct ReplyEnqueueOptions {
    idempotent: bool,
    report_failure_in_transcript: bool,
    requires_existing_worker: bool,
}

/// Cleanup payload for a deleted session's git and filesystem resources.
struct DeletedSessionCleanup {
    branch_name: String,
    folder: PathBuf,
    has_git_branch: bool,
    session_id: SessionId,
    staged_draft_root: PathBuf,
    working_dir: PathBuf,
}

/// Askama view model for rendering one-shot title-generation prompts.
#[derive(Template)]
#[template(path = "session_title_generation_prompt.md", escape = "none")]
struct SessionTitleGenerationPromptTemplate<'a> {
    current_title: &'a str,
    latest_request: &'a str,
    original_request: &'a str,
}

/// Persisted session context supplied to one title-generation request.
struct SessionTitleGenerationContext {
    current_title: String,
    latest_request: String,
    original_request: String,
}

/// Identifies one tracked draft-title generation task completion event.
struct TitleGenerationTaskCompletion {
    generation: u64,
    session_id: SessionId,
}

/// Inputs for one claimed title-generation task whose database revision is
/// ready to consume.
struct ClaimedSessionTitleGenerationTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    db: db::AppRepositories,
    folder: PathBuf,
    latest_request: String,
    one_shot_client: Arc<dyn OneShotClient>,
    reasoning_level: ReasoningLevel,
    session_agent: AgentSelection,
    session_id: SessionId,
    speed_mode: SpeedMode,
    title_generation: i64,
    tracked_completion: Option<TitleGenerationTaskCompletion>,
}

/// Fast-role defaults and workspace used by draft title generation.
struct DraftTitleGenerationContext {
    agent: AgentSelection,
    folder: PathBuf,
    reasoning_level: ReasoningLevel,
    speed_mode: SpeedMode,
}

/// Inputs for one detached session-title generation task.
pub(super) struct SessionTitleGenerationTaskInput {
    /// Event sink used to publish task completion and session refreshes.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Repository bundle used to persist a generated title.
    pub(super) db: db::AppRepositories,
    /// Project folder used as the isolated prompt working directory.
    pub(super) folder: PathBuf,
    /// Latest request that may establish or clarify the durable session goal.
    pub(super) latest_request: String,
    /// Provider-neutral boundary for the isolated title prompt.
    pub(super) one_shot_client: Arc<dyn OneShotClient>,
    /// Reasoning effort paired with the title-generation model.
    pub(super) reasoning_level: ReasoningLevel,
    /// Whether title generation should run only while the visible title is
    /// still a provisional user-prompt fallback.
    pub(super) requires_provisional_title: bool,
    /// Agent/model selection used for title generation.
    pub(super) session_agent: AgentSelection,
    /// Session receiving the generated title.
    pub(super) session_id: SessionId,
    /// Response speed paired with the title-generation model.
    pub(super) speed_mode: SpeedMode,
    /// Optional generation used to ignore superseded draft-title tasks.
    pub(super) tracked_generation: Option<u64>,
}

impl SessionManager {
    /// Moves selection to the next selectable session in grouped list order.
    ///
    /// Group header rows are non-selectable and are skipped by design.
    pub fn next(&mut self) {
        if let Some(index) = session_order::next_selectable_session_index(
            &self.state.sessions,
            self.state.table_state.selected(),
        ) {
            self.state.table_state.select(Some(index));
        }
    }

    /// Moves selection to the previous selectable session in grouped list
    /// order.
    ///
    /// Group header rows are non-selectable and are skipped by design.
    pub fn previous(&mut self) {
        if let Some(index) = session_order::previous_selectable_session_index(
            &self.state.sessions,
            self.state.table_state.selected(),
        ) {
            self.state.table_state.select(Some(index));
        }
    }

    /// Creates a blank session with an empty prompt and output.
    ///
    /// Returns the identifier of the newly created session.
    /// The session is created with `Draft` status and no agent is started —
    /// call [`SessionManager::start_session`] to submit a prompt and launch
    /// the agent.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, database record, or
    /// backend setup cannot be created.
    pub async fn create_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<String, SessionError> {
        let base_branch = projects.git_branch().ok_or_else(|| {
            SessionError::Workflow("Git branch is required to create a session".to_string())
        })?;

        self.create_session_for_project(
            services,
            projects.active_project_id(),
            base_branch,
            projects.working_dir().to_path_buf(),
            None,
            SessionCreationKind::Worker,
        )
        .await
    }

    /// Creates a blank draft session that stages prompts until explicitly
    /// started.
    ///
    /// Draft sessions defer worktree creation until the staged bundle starts
    /// so the session branch can be based on the latest local base-branch
    /// state.
    ///
    /// Returns the identifier of the newly created session.
    ///
    /// # Errors
    /// Returns an error if the session files or database record cannot be
    /// created, or if regular-session worktree/backend setup fails.
    pub async fn create_draft_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<String, SessionError> {
        let base_branch = projects.git_branch().ok_or_else(|| {
            SessionError::Workflow("Git branch is required to create a session".to_string())
        })?;

        self.create_draft_session_for_project(services, projects.active_project_id(), base_branch)
            .await
    }

    /// Creates a blank draft session stacked on top of a selected parent
    /// session branch.
    ///
    /// The child remains an explicit draft while prompts are staged. It can
    /// start once the parent is review-ready and no other stack member is
    /// doing branch work. Its lazy worktree is based on the stored parent
    /// branch, and the parent link is kept so review publishing can target the
    /// parent branch while the stack is active.
    ///
    /// # Errors
    /// Returns an error when the parent is missing, already stacked, terminal,
    /// an unmaterialized draft, missing project metadata, or when draft
    /// persistence fails.
    pub async fn create_stacked_draft_session(
        &mut self,
        services: &AppServices,
        parent_session_id: &str,
    ) -> Result<String, SessionError> {
        self.create_stacked_draft_session_with_optional_settings(services, parent_session_id, None)
            .await
    }

    /// Moves one independent review-ready session beneath another session and
    /// queues a branch sync onto the new parent branch.
    ///
    /// # Errors
    /// Returns an error when either session is ineligible, stack policy would
    /// be violated, metadata cannot be persisted, or the sync cannot start.
    pub async fn append_session_to_stack(
        &mut self,
        services: &AppServices,
        session_id: &str,
        parent_session_id: &str,
    ) -> Result<(), SessionError> {
        if !stack_can_append_session(&self.state.sessions, session_id, parent_session_id)
            || self.has_competing_preparation(session_id)
            || has_reserved_branch_work_in_stack(&self.state.sessions, parent_session_id, |id| {
                self.worker_service.has_preparation_reservation(id)
            })
        {
            return Err(SessionError::Workflow(
                "Append to stack requires an independent Review or AgentReview session and an \
                 idle review-ready parent"
                    .to_string(),
            ));
        }

        let (old_base_branch, old_parent_session_id, parent_branch) = {
            let session = self.session_or_err(session_id)?;
            let parent_session = self.session_or_err(parent_session_id)?;
            let parent_branch = self
                .session_branch_name(&parent_session.id)
                .map_or_else(|| session_branch(&parent_session.id), str::to_string);

            (
                session.base_branch.clone(),
                session.parent_session_id.clone(),
                parent_branch,
            )
        };
        let old_stack_base_commit_hash = services
            .db()
            .sessions()
            .get_session_stack_base_commit_hash(session_id)
            .await?;
        services
            .db()
            .sessions()
            .update_session_stack_membership(
                session_id,
                Some(parent_session_id),
                &parent_branch,
                old_stack_base_commit_hash.clone(),
            )
            .await?;
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id.as_str() == session_id)
        {
            session.base_branch.clone_from(&parent_branch);
            session.parent_session_id = Some(SessionId::from(parent_session_id));
        }

        if let Err(error) = self.rebase_session(services, session_id).await {
            services
                .db()
                .sessions()
                .update_session_stack_membership(
                    session_id,
                    old_parent_session_id.as_deref(),
                    &old_base_branch,
                    old_stack_base_commit_hash,
                )
                .await?;
            if let Some(session) = self
                .state
                .sessions
                .iter_mut()
                .find(|session| session.id.as_str() == session_id)
            {
                session.base_branch = old_base_branch;
                session.parent_session_id = old_parent_session_id;
            }

            return Err(error);
        }

        services.emit_session_and_project_refresh_events();

        Ok(())
    }

    /// Creates one blank draft session for an explicit persisted project.
    ///
    /// Continuation flows use the source session project instead of the
    /// currently active project so the later lazy worktree is materialized from
    /// the same repository and base branch as the terminal source session.
    ///
    /// Returns the identifier of the newly created session.
    ///
    /// # Errors
    /// Returns an error if the session files or database record cannot be
    /// created.
    pub async fn create_draft_session_for_project(
        &mut self,
        services: &AppServices,
        project_id: i64,
        base_branch: &str,
    ) -> Result<String, SessionError> {
        self.create_draft_session_for_project_with_parent(
            services,
            project_id,
            base_branch,
            None,
            None,
        )
        .await
    }

    /// Creates one project-scoped draft with a deterministic launch-settings
    /// snapshot.
    ///
    /// # Errors
    /// Returns an error if session metadata cannot be persisted.
    pub(crate) async fn create_draft_session_for_project_with_settings(
        &mut self,
        services: &AppServices,
        project_id: i64,
        base_branch: &str,
        creation_settings: Option<SessionCreationSettings>,
    ) -> Result<String, SessionError> {
        self.create_draft_session_for_project_with_parent(
            services,
            project_id,
            base_branch,
            None,
            creation_settings,
        )
        .await
    }

    /// Creates one stacked draft with a deterministic launch-settings
    /// snapshot.
    ///
    /// # Errors
    /// Returns an error when the parent is ineligible or persistence fails.
    pub(crate) async fn create_stacked_draft_session_with_settings(
        &mut self,
        services: &AppServices,
        parent_session_id: &str,
        creation_settings: SessionCreationSettings,
    ) -> Result<String, SessionError> {
        self.create_stacked_draft_session_with_optional_settings(
            services,
            parent_session_id,
            Some(creation_settings),
        )
        .await
    }

    /// Creates one stacked draft with optional deterministic launch settings.
    async fn create_stacked_draft_session_with_optional_settings(
        &mut self,
        services: &AppServices,
        parent_session_id: &str,
        creation_settings: Option<SessionCreationSettings>,
    ) -> Result<String, SessionError> {
        let (base_branch, parent_id) = {
            let parent_session = self.session_or_err(parent_session_id)?;
            if !stack_can_create_stacked_child(&self.state.sessions, parent_session_id) {
                return Err(SessionError::Workflow(
                    "Stacked sessions require an active materialized parent below the five-level \
                     stack limit"
                        .to_string(),
                ));
            }

            let parent_branch = self
                .session_branch_name(&parent_session.id)
                .map_or_else(|| session_branch(&parent_session.id), str::to_string);

            (parent_branch, parent_session.id.clone())
        };
        let project_id = services
            .db()
            .sessions()
            .load_session_project_id(parent_id.as_str())
            .await?
            .ok_or_else(|| {
                SessionError::Workflow(
                    "Parent session has no project association for stacked draft creation"
                        .to_string(),
                )
            })?;

        self.create_draft_session_for_project_with_parent(
            services,
            project_id,
            &base_branch,
            Some(parent_id.as_str()),
            creation_settings,
        )
        .await
    }

    /// Creates one blank draft session with an optional persisted parent
    /// session id.
    async fn create_draft_session_for_project_with_parent(
        &mut self,
        services: &AppServices,
        project_id: i64,
        base_branch: &str,
        parent_session_id: Option<&str>,
        creation_settings: Option<SessionCreationSettings>,
    ) -> Result<String, SessionError> {
        let creation_settings = self
            .resolve_session_creation_settings(services, project_id, creation_settings)
            .await?;
        let session_agent = creation_settings.agent;
        let session_model = session_agent.model();
        let session_role = creation_settings.role.to_string();

        let session_id = Uuid::new_v4().to_string();
        let folder = session_folder(services.base_path(), &session_id);
        if services.fs_client().exists(folder.clone()) {
            return Err(SessionError::Workflow(format!(
                "Session folder {session_id} already exists"
            )));
        }

        let session_agent_kind = session_agent.kind().to_string();
        let status = Status::Draft.to_string();
        let insert_result = services
            .db()
            .sessions()
            .insert_session_with_agent(db::PersistedSessionCreation {
                agent: &session_agent_kind,
                base_branch,
                id: &session_id,
                is_draft: true,
                model: session_model.as_str(),
                orchestration_task_id: None,
                parent_session_id,
                permission_mode: creation_settings.permission_mode,
                personality_id: creation_settings.personality_id.as_deref(),
                project_id,
                reasoning_level: creation_settings.reasoning_level,
                response_style: creation_settings.response_style,
                role: Some(&session_role),
                speed_mode: creation_settings.speed_mode,
                status: &status,
            })
            .await;

        insert_result.map_err(|error| {
            SessionError::Workflow(format!("Failed to save session metadata: {error}"))
        })?;

        Self::record_session_creation_activity(services, &session_id).await;

        Ok(session_id)
    }

    /// Forks a root review-ready session into a new independent review
    /// session.
    ///
    /// The fork creates a new worktree branch from the source session branch,
    /// snapshots persisted transcript messages, clears provider-native
    /// conversation and publish/review-request linkage, and marks the new
    /// session for one-time history replay on its first reply.
    ///
    /// # Errors
    /// Returns an error if the source session is missing, not root
    /// review-ready, repository metadata cannot be resolved, the worktree
    /// cannot be created, or the metadata snapshot cannot be persisted.
    /// Preparation failures roll back the reserved fork and its resources.
    pub async fn fork_session(
        &mut self,
        services: &AppServices,
        source_session_id: &str,
    ) -> Result<String, SessionError> {
        let (session_id, repo_root) = self
            .reserve_fork_session_with_repo_root(services, source_session_id)
            .await?;
        if let Err(error) = Self::prepare_reserved_session(services, &session_id).await {
            let folder = session_folder(services.base_path(), &session_id);
            let has_worktree = services.fs_client().is_dir(folder.clone());
            Self::rollback_failed_session_creation(
                services,
                &folder,
                &repo_root,
                &session_id,
                &session_branch(&session_id),
                true,
                has_worktree,
            )
            .await;
            self.clear_history_replay_pending(&session_id);

            return Err(error);
        }
        services.emit_session_and_project_refresh_events();

        Ok(session_id)
    }

    /// Freezes fork history and its source commit before background checkout.
    pub(crate) async fn reserve_fork_session(
        &mut self,
        services: &AppServices,
        source_session_id: &str,
    ) -> Result<String, SessionError> {
        self.reserve_fork_session_with_repo_root(services, source_session_id)
            .await
            .map(|(session_id, _repo_root)| session_id)
    }

    /// Captures the repository root with the reservation so synchronous
    /// rollback never depends on a second, fallible repository lookup.
    async fn reserve_fork_session_with_repo_root(
        &mut self,
        services: &AppServices,
        source_session_id: &str,
    ) -> Result<(String, PathBuf), SessionError> {
        let source_branch = {
            let source_session = self.session_or_err(source_session_id)?;
            if !source_session.allows_fork_action() {
                return Err(SessionError::Workflow(
                    "Only root review-ready sessions can be forked".to_string(),
                ));
            }

            self.session_branch_name(&source_session.id)
                .map_or_else(|| session_branch(&source_session.id), str::to_string)
        };
        services
            .db()
            .sessions()
            .load_session_project_id(source_session_id)
            .await?
            .ok_or_else(|| {
                SessionError::Workflow(
                    "Source session has no project association for session forking".to_string(),
                )
            })?;

        let repo_root = self
            .load_session_repo_root(services, source_session_id)
            .await?;
        let session_id = Uuid::new_v4().to_string();
        let folder = session_folder(services.base_path(), &session_id);
        if services.fs_client().exists(folder.clone()) {
            return Err(SessionError::Workflow(format!(
                "Session folder {session_id} already exists"
            )));
        }

        let start_ref = services
            .git_client()
            .ref_hash(repo_root.clone(), source_branch)
            .await?;
        services
            .db()
            .sessions()
            .reserve_fork_session_snapshot(
                db::ForkSessionSnapshot {
                    new_session_id: &session_id,
                    source_session_id,
                    status: &Status::Review.to_string(),
                },
                &start_ref,
            )
            .await?;
        Self::record_session_creation_activity(services, &session_id).await;

        self.mark_history_replay_pending(&session_id);

        Ok((session_id, repo_root))
    }

    /// Creates one regular session whose worktree is materialized before the
    /// first prompt is submitted.
    ///
    /// # Errors
    /// Returns an error if the worktree, session files, database record, or
    /// backend setup cannot be created.
    pub(crate) async fn create_session_for_project(
        &mut self,
        services: &AppServices,
        project_id: i64,
        base_branch: &str,
        working_dir: PathBuf,
        creation_settings: Option<SessionCreationSettings>,
        creation_kind: SessionCreationKind,
    ) -> Result<String, SessionError> {
        let creation_settings = self
            .resolve_session_creation_settings(services, project_id, creation_settings)
            .await?;

        Self::materialize_session(
            services,
            project_id,
            base_branch,
            working_dir,
            creation_settings,
            creation_kind,
        )
        .await
    }

    /// Creates a ready workspace for callers that require readiness on return.
    /// Preparation failures roll back the reservation and its resources.
    pub(crate) async fn materialize_session(
        services: &AppServices,
        project_id: i64,
        base_branch: &str,
        working_dir: PathBuf,
        creation_settings: SessionCreationSettings,
        creation_kind: SessionCreationKind,
    ) -> Result<String, SessionError> {
        let session_id = Self::reserve_session(
            services,
            project_id,
            base_branch,
            creation_settings,
            creation_kind,
        )
        .await?;
        if let Err(error) = Self::prepare_reserved_session(services, &session_id).await {
            let folder = session_folder(services.base_path(), &session_id);
            let has_worktree = services.fs_client().is_dir(folder.clone());
            Self::rollback_failed_session_creation(
                services,
                &folder,
                &working_dir,
                &session_id,
                &session_branch(&session_id),
                true,
                has_worktree,
            )
            .await;

            return Err(error);
        }
        services.emit_session_and_project_refresh_events();

        Ok(session_id)
    }

    /// Saves a usable conversation identity before preparing its workspace.
    pub(crate) async fn reserve_session(
        services: &AppServices,
        project_id: i64,
        base_branch: &str,
        creation_settings: SessionCreationSettings,
        creation_kind: SessionCreationKind,
    ) -> Result<String, SessionError> {
        let session_id = Uuid::new_v4().to_string();
        services
            .db()
            .sessions()
            .reserve_session(db::PersistedSessionCreation {
                agent: &creation_settings.agent.kind().to_string(),
                base_branch,
                id: &session_id,
                is_draft: false,
                model: creation_settings.agent.model().as_str(),
                orchestration_task_id: creation_kind.orchestration_task_id(),
                parent_session_id: None,
                permission_mode: creation_settings.permission_mode,
                personality_id: creation_settings.personality_id.as_deref(),
                project_id,
                reasoning_level: creation_settings.reasoning_level,
                response_style: creation_settings.response_style,
                role: Some(&creation_kind.role().to_string()),
                speed_mode: creation_settings.speed_mode,
                status: &Status::Draft.to_string(),
            })
            .await?;
        Self::record_session_creation_activity(services, &session_id).await;

        Ok(session_id)
    }

    /// Creates the git worktree and session-local metadata directory for one
    /// session branch from an explicit start ref.
    ///
    /// Regular sessions pass the local base branch as the start ref, stacked
    /// drafts pass their parent branch, and forks pass the source session
    /// branch so the new worktree preserves the source branch state at fork
    /// time.
    ///
    /// # Errors
    /// Returns an error if git worktree creation fails or the `.agentty`
    /// metadata directory cannot be created inside the worktree.
    pub(super) async fn create_session_worktree(
        services: &AppServices,
        session_id: &str,
        folder: &Path,
        repo_root: &Path,
        worktree_branch: &str,
        start_ref: &str,
    ) -> Result<(), SessionError> {
        services
            .git_client()
            .create_worktree(
                repo_root.to_path_buf(),
                folder.to_path_buf(),
                worktree_branch.to_string(),
                start_ref.to_string(),
            )
            .await
            .map_err(|error| {
                SessionError::Workflow(format!("Failed to create git worktree: {error}"))
            })?;

        let data_dir = folder.join(SESSION_DATA_DIR);
        if let Err(error) = services.fs_client().create_dir_all(data_dir).await {
            Self::rollback_failed_session_creation(
                services,
                folder,
                repo_root,
                session_id,
                worktree_branch,
                false,
                true,
            )
            .await;

            return Err(SessionError::Workflow(format!(
                "Failed to create session metadata directory: {error}"
            )));
        }

        Ok(())
    }

    /// Ensures a draft session has a usable worktree and backend setup before
    /// its first live turn starts.
    ///
    /// Non-draft sessions are created eagerly and therefore skip this path.
    /// Draft sessions create their worktree lazily here so staged prompts can
    /// remain detached from the base branch until the user starts the session.
    ///
    /// # Errors
    /// Returns an error if repository discovery, worktree creation, or
    /// backend setup fails.
    async fn ensure_session_worktree_ready(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        if let Some(preparation) = services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
        {
            if preparation.state != db::SessionPreparationState::Ready {
                return Err(SessionError::Workflow(
                    "Session workspace is not ready".to_string(),
                ));
            }
            self.set_session_worktree_available(session_id, true);

            return Ok(());
        }
        let (base_branch, folder, parent_session_id, persisted_session_id, session_agent) = {
            let session = self.session_or_err(session_id)?;
            if !session.is_draft_session() {
                return Ok(());
            }

            (
                session.base_branch.clone(),
                session.folder.clone(),
                session.parent_session_id.clone(),
                session.id.clone(),
                session.agent,
            )
        };

        let worktree_branch = session_branch(&persisted_session_id);
        if services.fs_client().is_dir(folder.clone()) {
            isolation::validate_session_worktree(
                services.fs_client().as_ref(),
                services.git_client().as_ref(),
                &folder,
                &persisted_session_id,
            )
            .await?;
            agent::create_backend(session_agent.kind())
                .setup(&folder)
                .map_err(|error| {
                    SessionError::Workflow(format!("Failed to setup session backend: {error}"))
                })?;
            self.persist_stack_base_for_stacked_draft_worktree(
                services,
                &folder,
                parent_session_id.as_ref(),
                &persisted_session_id,
            )
            .await?;
            self.set_session_worktree_available(session_id, true);

            return Ok(());
        }

        let repo_root = self.load_session_repo_root(services, session_id).await?;

        Self::create_session_worktree(
            services,
            &persisted_session_id,
            &folder,
            &repo_root,
            &worktree_branch,
            &base_branch,
        )
        .await?;

        if let Err(error) = agent::create_backend(session_agent.kind()).setup(&folder) {
            let cleanup_errors = Self::cleanup_session_worktree_resources(
                services.fs_client().clone(),
                services.git_client(),
                folder,
                worktree_branch,
                Some(repo_root),
                true,
            )
            .await;

            if !cleanup_errors.is_empty() {
                return Err(SessionError::Workflow(format!(
                    "Failed to setup session backend: {error}. Cleanup also failed: {}",
                    cleanup_errors.join("; ")
                )));
            }

            return Err(SessionError::Workflow(format!(
                "Failed to setup session backend: {error}"
            )));
        }
        self.persist_stack_base_for_stacked_draft_worktree(
            services,
            &folder,
            parent_session_id.as_ref(),
            &persisted_session_id,
        )
        .await?;
        self.set_session_worktree_available(session_id, true);

        Ok(())
    }

    /// Clears the persisted and in-memory draft flag once a draft session
    /// starts its first live turn.
    ///
    /// The flag only means "still staging draft prompts", so it must not
    /// outlive the session start; a sticky flag would keep draft-only
    /// restrictions, such as the fork gate, active for the session's whole
    /// life. Non-draft sessions skip the write.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    async fn clear_session_draft_flag(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        if !self.session_or_err(session_id)?.is_draft_session() {
            return Ok(());
        }

        services
            .db()
            .sessions()
            .clear_session_draft_flag(session_id)
            .await?;

        let session_index = self.session_index_or_err(session_id)?;
        if let Some(session) = self.session_at_mut(session_index) {
            session.is_draft = false;
        }

        Ok(())
    }

    /// Persists the parent tip used by a stacked draft's newly materialized
    /// worktree.
    ///
    /// The stored hash lets later stacked-child rebases use
    /// `git rebase --onto` to replay only the child's commits when the parent
    /// branch moves or squash-merges.
    ///
    /// # Errors
    /// Returns an error when the worktree `HEAD` cannot be resolved or stack
    /// metadata cannot be persisted.
    async fn persist_stack_base_for_stacked_draft_worktree(
        &self,
        services: &AppServices,
        folder: &Path,
        parent_session_id: Option<&SessionId>,
        session_id: &str,
    ) -> Result<(), SessionError> {
        if parent_session_id.is_none() {
            return Ok(());
        }

        let stack_base_commit_hash = services
            .git_client()
            .head_hash(folder.to_path_buf())
            .await
            .map_err(SessionError::Git)?;
        services
            .db()
            .sessions()
            .update_session_stack_base_commit_hash(session_id, Some(stack_base_commit_hash))
            .await
            .map_err(SessionError::Db)?;

        Ok(())
    }

    /// Resolves the repository root for one persisted session.
    ///
    /// # Errors
    /// Returns an error if the session project cannot be resolved or no git
    /// repository root can be found for the project path.
    async fn load_session_repo_root(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<PathBuf, SessionError> {
        let project_id = services
            .db()
            .sessions()
            .load_session_project_id(session_id)
            .await?
            .ok_or_else(|| {
                SessionError::Workflow(
                    "Session project is required to create a worktree".to_string(),
                )
            })?;
        let project_path = self.load_project_path(services, project_id).await?;

        services
            .git_client()
            .find_git_repo_root(project_path)
            .await
            .ok_or_else(|| SessionError::Workflow("Failed to find git repository root".to_string()))
    }

    /// Loads the persisted project path for one project identifier.
    ///
    /// # Errors
    /// Returns an error if the project row does not exist or cannot be loaded.
    async fn load_project_path(
        &self,
        services: &AppServices,
        project_id: i64,
    ) -> Result<PathBuf, SessionError> {
        let project_row = services
            .db()
            .projects()
            .get_project(project_id)
            .await?
            .ok_or_else(|| {
                SessionError::Workflow(format!("Project with id `{project_id}` was not found"))
            })?;

        Ok(PathBuf::from(project_row.path))
    }

    async fn persist_staged_draft(
        services: &AppServices,
        session_id: &str,
        staged_attachments: &[TurnPromptAttachment],
        staged_prompt: &str,
        title_to_save: Option<&str>,
    ) -> Result<(), SessionError> {
        draft::store_staged_draft_attachments(
            services.fs_client().as_ref(),
            services.base_path(),
            session_id,
            staged_attachments,
        )
        .await?;
        services
            .db()
            .sessions()
            .update_session_prompt(session_id, staged_prompt)
            .await?;
        if let Some(title) = title_to_save {
            services
                .db()
                .sessions()
                .update_session_provisional_title(session_id, title)
                .await?;
        }
        Ok(())
    }

    /// Appends one staged draft message to a `Draft` session without launching
    /// the agent yet.
    ///
    /// This emits a [`AppEvent::SessionUpdated`] signal so memoized session
    /// views refresh immediately after local draft updates. The signal is
    /// best-effort after staged state has already been persisted; if the
    /// foreground event channel is closed, staging still succeeds and the next
    /// session refresh observes the committed prompt.
    ///
    /// The first staged prompt seeds a fallback title, while later staged
    /// prompts keep the current visible title in place until the refreshed
    /// generated title arrives.
    ///
    /// # Errors
    /// Returns an error if the session is missing, was not created as a draft
    /// session, is no longer `Draft`, or the staged bundle cannot be persisted.
    pub async fn stage_draft_message(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), SessionError> {
        let prompt = prompt.into();
        let session_index = self.session_index_or_err(session_id)?;
        let (
            folder,
            persisted_session_id,
            session_agent,
            staged_attachments,
            staged_prompt,
            title_to_save,
        ) = {
            let session = self
                .session_at(session_index)
                .ok_or(SessionError::NotFound)?;
            if !session.is_draft_session() {
                return Err(SessionError::Workflow(
                    "Only draft sessions can stage drafts".to_string(),
                ));
            }
            if session.status != Status::Draft {
                return Err(SessionError::Workflow(
                    "Only `Draft` sessions can stage drafts".to_string(),
                ));
            }

            let next_attachment_number = session.draft_attachments.len().saturating_add(1);
            let staged_prompt =
                Self::append_staged_prompt(&session.prompt, &prompt, next_attachment_number);
            let mut staged_attachments = session.draft_attachments.clone();
            staged_attachments.extend(Self::renumbered_attachments(
                &prompt,
                next_attachment_number,
            ));
            let title_to_save = session.title.is_none().then(|| prompt.transcript_text());

            (
                session.folder.clone(),
                session.id.clone(),
                session.agent,
                staged_attachments,
                staged_prompt,
                title_to_save,
            )
        };
        let project_id = services
            .db()
            .sessions()
            .load_session_project_id(&persisted_session_id)
            .await?
            .ok_or_else(|| {
                SessionError::Workflow(
                    "Session project is required to stage draft prompts".to_string(),
                )
            })?;
        let title_generation_context = self
            .draft_title_generation_context(services, project_id, session_agent, folder)
            .await?;

        Self::persist_staged_draft(
            services,
            &persisted_session_id,
            &staged_attachments,
            &staged_prompt,
            title_to_save.as_deref(),
        )
        .await?;

        let title_generation_prompt = staged_prompt.clone();

        if let Some(session) = self.session_at_mut(session_index) {
            session.prompt = staged_prompt;
            session.draft_attachments = staged_attachments;
            if let Some(title_to_save) = title_to_save {
                session.title = Some(title_to_save);
            }
        }

        let title_generation_task_generation =
            self.next_title_generation_task_generation(&persisted_session_id);
        let title_generation_task =
            Self::spawn_session_title_generation_task(SessionTitleGenerationTaskInput {
                app_event_tx: services.event_sender(),
                db: services.db().clone(),
                folder: title_generation_context.folder,
                latest_request: title_generation_prompt,
                one_shot_client: services.one_shot_client(),
                requires_provisional_title: false,
                reasoning_level: title_generation_context.reasoning_level,
                session_agent: title_generation_context.agent,
                session_id: persisted_session_id.clone(),
                speed_mode: title_generation_context.speed_mode,
                tracked_generation: Some(title_generation_task_generation),
            })
            .await;
        self.track_draft_title_generation_task(
            &persisted_session_id,
            title_generation_task_generation,
            title_generation_task,
        );

        SessionTaskService::emit_session_updated(
            &services.event_sender(),
            &services.session_update_versions(),
            persisted_session_id.as_str(),
        );

        Ok(())
    }

    /// Tracks a newly spawned draft-title task or clears a superseded task
    /// when the latest prompt does not require model generation.
    fn track_draft_title_generation_task(
        &mut self,
        session_id: &str,
        generation: u64,
        title_generation_task: Option<tokio::task::JoinHandle<()>>,
    ) {
        if let Some(title_generation_task) = title_generation_task {
            self.replace_title_generation_task(session_id, generation, title_generation_task);
        } else {
            self.abort_title_generation_task(session_id);
        }
    }

    /// Loads the folder and agent/model selection used for draft title
    /// generation.
    async fn draft_title_generation_context(
        &self,
        services: &AppServices,
        project_id: i64,
        session_agent: AgentSelection,
        session_folder: PathBuf,
    ) -> Result<DraftTitleGenerationContext, SessionError> {
        let project_working_dir = self.load_project_path(services, project_id).await?;
        let title_generation_agent =
            setting::load_default_fast_agent_setting(services, Some(project_id), session_agent)
                .await;
        let title_generation_reasoning_level = services
            .db()
            .settings()
            .load_project_reasoning_level(project_id, SettingName::DefaultFastReasoningLevel)
            .await?;
        let title_generation_speed_mode = services
            .db()
            .settings()
            .load_project_speed_mode(project_id, SettingName::DefaultFastSpeedMode)
            .await?;
        let title_generation_speed_mode = if title_generation_agent.kind().supports_speed_mode() {
            title_generation_speed_mode
        } else {
            SpeedMode::Normal
        };
        let title_generation_agent =
            title_generation_agent.compatible_with_speed_mode(title_generation_speed_mode);
        let title_generation_folder = if services.fs_client().is_dir(session_folder.clone()) {
            session_folder
        } else {
            project_working_dir
        };

        Ok(DraftTitleGenerationContext {
            agent: title_generation_agent,
            folder: title_generation_folder,
            reasoning_level: title_generation_reasoning_level,
            speed_mode: title_generation_speed_mode,
        })
    }

    /// Returns whether the selected session can parent another stacked draft.
    pub(crate) fn can_create_stacked_child(&self, session_id: &str) -> bool {
        stack_can_create_stacked_child(&self.state.sessions, session_id)
    }

    /// Returns whether a staged draft can start under the current stack
    /// constraints.
    pub(crate) fn can_start_staged_session(&self, session_id: &str) -> bool {
        stack_can_start_staged_session(&self.state.sessions, session_id)
            && self.can_retry_workspace_preparation(session_id)
            && !self.has_competing_preparation(session_id)
    }

    /// Prevents workspace retries from invalidating a queued or running
    /// saved-prompt handoff.
    pub(crate) fn can_retry_workspace_preparation(&self, session_id: &str) -> bool {
        !self.worker_service.has_preparation_reservation(session_id)
    }

    /// Returns whether a session can start branch-mutating work without
    /// competing with another member of its stack.
    pub(crate) fn can_mutate_session_branch_in_stack(&self, session_id: &str) -> bool {
        stack_can_mutate_session_branch(&self.state.sessions, session_id)
            && !self.has_competing_preparation(session_id)
    }

    /// Returns whether a session can enter the merge queue without competing
    /// with another member of its stack.
    pub(crate) fn can_merge_session_branch_in_stack(&self, session_id: &str) -> bool {
        stack_can_merge_session_branch(&self.state.sessions, session_id)
            && !self.has_competing_preparation(session_id)
    }

    /// Returns whether a session can start sync work without competing with
    /// another member of its stack.
    pub(crate) fn can_rebase_session_branch_in_stack(&self, session_id: &str) -> bool {
        stack_can_rebase_session_branch(&self.state.sessions, session_id)
            && !self.has_competing_preparation(session_id)
    }

    /// Returns whether a session can accept a reply without another stack
    /// member already owning active branch work.
    pub(crate) fn can_reply_to_session_in_stack(&self, session_id: &str) -> bool {
        stack_can_reply_to_session(&self.state.sessions, session_id)
            && !self.has_competing_preparation(session_id)
    }

    /// Includes queued saved turns in stack policy without publishing a
    /// premature user-visible status. Foreground actions are serialized, so
    /// claiming at enqueue closes the gap before worker acceptance.
    fn has_competing_preparation(&self, session_id: &str) -> bool {
        has_reserved_branch_work_in_stack(&self.state.sessions, session_id, |candidate_id| {
            candidate_id != session_id
                && self
                    .worker_service
                    .has_preparation_reservation(candidate_id)
        })
    }

    /// Starts a `Draft` session from its persisted staged draft bundle.
    ///
    /// This materializes the deferred draft worktree before launching the
    /// first live turn. Stacked drafts additionally wait for a review-ready
    /// parent and an otherwise idle stack so only one branch-mutating session
    /// runs in that stack.
    ///
    /// # Errors
    /// Returns an error if the session is missing, is not a draft session, no
    /// drafts are staged, or launching the first turn fails.
    pub async fn start_staged_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let prompt = {
            let session = self.session_or_err(session_id)?;
            if !session.is_draft_session() {
                return Err(SessionError::Workflow(
                    "Only draft sessions can be started from staged drafts".to_string(),
                ));
            }
            if session.status != Status::Draft {
                return Err(SessionError::Workflow(
                    "Only `Draft` sessions can be started from staged drafts".to_string(),
                ));
            }
            if session.prompt.is_empty() {
                return Err(SessionError::Workflow(
                    "Stage at least one draft before starting the session".to_string(),
                ));
            }
            if !self.can_start_staged_session(session_id) {
                return Err(SessionError::Workflow(
                    "Stacked sessions can only start when their parent is in review and the stack \
                     has no other active branch work"
                        .to_string(),
                ));
            }

            TurnPrompt {
                attachments: session.draft_attachments.clone(),
                text: session.prompt.clone(),
                text_source: TurnPromptTextSource::UserPrompt,
            }
        };

        self.start_session(services, session_id, prompt).await?;

        self.clear_started_draft_attachments(services, session_id)
            .await;

        Ok(())
    }

    /// Releases draft attachment staging after the saved turn is accepted.
    pub(crate) async fn clear_started_draft_attachments(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) {
        if let Ok(session_index) = self.session_index_or_err(session_id)
            && let Some(session) = self.session_at_mut(session_index)
        {
            session.draft_attachments.clear();
        }

        if let Err(error) = draft::store_staged_draft_attachments(
            services.fs_client().as_ref(),
            services.base_path(),
            session_id,
            &[],
        )
        .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to clear staged draft attachments after session start"
            );
        }
    }

    /// Submits the first prompt for a blank session and starts the agent.
    ///
    /// The first prompt is persisted as both session prompt and session title.
    /// A detached one-shot title-generation task may replace that provisional
    /// title when the prompt contains actionable intent.
    ///
    /// # Errors
    /// Returns an error if the session is missing, its worktree cannot be
    /// prepared, or prompt persistence fails.
    pub async fn start_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), SessionError> {
        let prompt = prompt.into();
        self.ensure_session_worktree_ready(services, session_id)
            .await?;

        let session_index = self.session_index_or_err(session_id)?;
        let session = self.session_or_err(session_id)?;
        let persisted_session_id = session.id.clone();
        let session_agent = session.agent;
        let has_saved_prompt = services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
            .is_some_and(|row| row.prompt.is_some());
        let operation_id = if has_saved_prompt {
            format!("workspace:{session_id}")
        } else {
            Uuid::new_v4().to_string()
        };
        let command = SessionCommand::Run {
            operation_id,
            request_kind: AgentRequestKind::SessionStart,
            replay_transcript: None,
            prompt: prompt.clone(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent,
            },
        };
        let ready_tx = match self
            .enqueue_gated_session_command(services, &persisted_session_id, command)
            .await
        {
            Ok(ready_tx) => ready_tx,
            Err(error) => {
                // Saved prompts still own their attachments until a retry
                // hands them to the worker successfully.
                if !has_saved_prompt {
                    self.cleanup_prompt_attachment_files(services, &prompt)
                        .await;
                }

                return Err(error);
            }
        };

        let title = {
            let session = self
                .session_at_mut(session_index)
                .ok_or(SessionError::NotFound)?;

            session.prompt.clone_from(&prompt.text);

            let title = prompt.text.clone();
            session.title = Some(title.clone());

            title
        };

        let handles = self.session_handles_or_err(&persisted_session_id)?;
        let transcript = Arc::clone(&handles.transcript);
        let status_transition =
            StatusTransition::from_services(services, handles, persisted_session_id.clone());
        let app_event_tx = services.event_sender();

        self.persist_first_message_metadata(services, &persisted_session_id, &prompt.text, &title)
            .await;

        if !has_saved_prompt {
            let prompt_transcript_text = prompt.transcript_text();
            let initial_output = Self::formatted_prompt_output(&prompt, false);
            SessionTaskService::append_session_transcript_message(
                &transcript,
                services.db(),
                &app_event_tx,
                &services.session_update_versions(),
                &persisted_session_id,
                SessionTranscriptMessageAppend {
                    kind: SessionMessageKind::UserPrompt,
                    raw_content: &prompt_transcript_text,
                },
            )
            .await;
            self.set_active_prompt_output(&persisted_session_id, initial_output);

            if !status_transition.apply(Status::InProgress).await {
                warn!(
                    session_id = %persisted_session_id,
                    "skipped session start status update because the in-memory status did not transition to in-progress"
                );
            }
        }

        // Saved prompts leave draft mode in the worker's acceptance
        // transaction, so a rejected handoff still checks the parent on retry.
        if !has_saved_prompt
            && let Err(error) = self.clear_session_draft_flag(services, session_id).await
        {
            warn!(
                session_id,
                %error,
                "failed to clear draft flag after session start"
            );
        }
        // The worker may now execute without racing the foreground's
        // initial transcript or status publication.
        let _ = ready_tx.send(());

        Ok(())
    }

    /// Submits a follow-up prompt to an existing session.
    ///
    /// Returns `true` when the reply command was enqueued on the session
    /// worker, letting callers gate optimistic status advances on a real
    /// enqueue.
    pub async fn reply(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> bool {
        let prompt = prompt.into();
        let Ok(session) = self.session_or_err(session_id) else {
            return false;
        };
        if session.status.is_read_only() {
            return false;
        }
        let session_agent = session.agent;

        self.reply_impl(
            services,
            session_id,
            prompt,
            session_agent,
            ReplyOptions::standard(Vec::new()),
        )
        .await
    }

    /// Queues a validated structured question answer directly on the
    /// per-session worker.
    ///
    /// Unlike ordinary chat submitted during `InProgress`, this reply must
    /// not enter the in-memory prompt queue: the active turn can transition
    /// to `Question`, where chat-queue drainage intentionally pauses. A
    /// worker command remains ordered behind the active turn and resumes it
    /// regardless of that transition.
    pub(crate) async fn reply_to_question_answers(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> bool {
        let prompt = prompt.into();
        let Ok(session) = self.session_or_err(session_id) else {
            return false;
        };
        let requires_existing_worker = session.status == Status::InProgress;
        let session_agent = session.agent;

        self.reply_impl(
            services,
            session_id,
            prompt,
            session_agent,
            ReplyOptions::question_answer(requires_existing_worker),
        )
        .await
    }

    /// Queues one coordinator-owned prompt directly on the serialized worker.
    ///
    /// Callers gate this path to idle controller states, so it never falls
    /// back to the lossy in-memory chat queue used by ordinary messages
    /// submitted during an active turn.
    pub(crate) async fn reply_to_coordinator_message(
        &mut self,
        services: &AppServices,
        session_id: &str,
        operation_id: String,
        persist_prompt: bool,
        prompt: impl Into<TurnPrompt>,
    ) -> bool {
        let prompt = prompt.into();
        let Ok(session) = self.session_or_err(session_id) else {
            return false;
        };
        let session_agent = session.agent;

        self.reply_impl(
            services,
            session_id,
            prompt,
            session_agent,
            ReplyOptions::coordinator(operation_id, persist_prompt),
        )
        .await
    }

    /// Submits a follow-up prompt with an allowlist of forge review threads
    /// eligible for post-push reply and resolution.
    ///
    /// Returns `true` when the command reaches the session worker.
    pub async fn reply_to_review_comments(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
        review_comment_thread_ids: Vec<String>,
    ) -> bool {
        let prompt = prompt.into();
        let Ok(session) = self.session_or_err(session_id) else {
            return false;
        };
        if session.status.is_read_only() {
            return false;
        }
        let session_agent = session.agent;

        self.reply_impl(
            services,
            session_id,
            prompt,
            session_agent,
            ReplyOptions::review_comments(review_comment_thread_ids),
        )
        .await
    }

    /// Stages one chat prompt into the in-memory queue for the active turn or
    /// rebase.
    ///
    /// The queue is owned by [`SessionHandles::queued_messages`] and lives
    /// only for the active app session, so queued prompts are discarded on
    /// `agentty` restart. The session worker drains the queue between turns
    /// without bouncing through `Review` and pauses drainage while the
    /// session sits in `Question`. `Ctrl+C` on the running turn drops the
    /// most recently queued chat message (LIFO) one press at a time without
    /// interrupting the running turn, and once the queue is empty a further
    /// press cancels the active turn.
    ///
    /// The just-pushed entry is mirrored into the render snapshot via
    /// [`SessionState::sync_session_from_handle`] so the inline `≡ queued ›`
    /// row appears on the very next frame, and the targeted
    /// [`AppEvent::SessionUpdated`] event triggers a single-session redraw
    /// without paying for a full DB-backed `RefreshSessions` reload.
    ///
    /// # Errors
    /// Returns [`SessionError::NotFound`] when the session id does not
    /// resolve to a known session, or [`SessionError::Workflow`] when the
    /// payload is empty after trimming.
    pub fn enqueue_message(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), SessionError> {
        let prompt = prompt.into();
        if prompt.is_empty() {
            return Err(SessionError::Workflow(
                "Cannot queue an empty chat message".to_string(),
            ));
        }

        if self.session_or_err(session_id)?.status.is_read_only() {
            return Err(SessionError::Workflow(
                "Merged sessions cannot queue chat messages".to_string(),
            ));
        }

        let handles = self.session_handles_or_err(session_id)?;

        // Sync critical section (single push, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        let order = handles.next_queued_work_order();
        if let Ok(mut guard) = handles.queued_messages.lock() {
            guard.push_back(QueuedMessage::new(order, prompt));
        }

        self.state.sync_session_from_handle(session_id);

        SessionTaskService::emit_session_updated(
            &services.event_sender(),
            &services.session_update_versions(),
            session_id,
        );

        Ok(())
    }

    /// Updates and persists the agent/model selection for a single session.
    ///
    /// When `LastUsedModelAsDefault` is enabled, this also persists the chosen
    /// session agent/model pair as `DefaultSmartAgent` and
    /// `DefaultSmartModel`.
    ///
    /// When the model changes, this also clears any persisted provider-native
    /// conversation identifier so incompatible runtimes do not attempt resume
    /// with stale ids, and drops the existing session worker so the next turn
    /// creates a fresh worker with the correct [`AgentChannel`] type.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_model(
        &mut self,
        services: &AppServices,
        session_id: &str,
        session_agent: AgentSelection,
    ) -> Result<(), SessionError> {
        self.set_session_model_with_default_persistence(services, session_id, session_agent, true)
            .await
    }

    /// Updates and persists the reasoning level for a single session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_reasoning_level(
        &mut self,
        services: &AppServices,
        session_id: &str,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), SessionError> {
        self.session_index_or_err(session_id)?;

        services
            .db()
            .sessions()
            .update_session_reasoning_level(session_id, reasoning_level)
            .await?;

        services.emit_app_event(AppEvent::SessionReasoningLevelUpdated {
            reasoning_level,
            session_id: SessionId::from(session_id),
        });

        Ok(())
    }

    /// Updates and persists the response style for a single session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_response_style(
        &mut self,
        services: &AppServices,
        session_id: &str,
        response_style: ResponseStyle,
    ) -> Result<(), SessionError> {
        self.session_index_or_err(session_id)?;

        services
            .db()
            .sessions()
            .update_session_response_style(session_id, response_style)
            .await?;

        services.emit_app_event(AppEvent::SessionResponseStyleUpdated {
            response_style,
            session_id: SessionId::from(session_id),
        });

        Ok(())
    }

    /// Updates and persists the provider permission mode for a single
    /// session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_permission_mode(
        &mut self,
        services: &AppServices,
        session_id: &str,
        permission_mode: PermissionMode,
    ) -> Result<(), SessionError> {
        self.session_index_or_err(session_id)?;

        services
            .db()
            .sessions()
            .update_session_permission_mode(session_id, permission_mode)
            .await?;

        services.emit_app_event(AppEvent::SessionPermissionModeUpdated {
            permission_mode,
            session_id: SessionId::from(session_id),
        });

        Ok(())
    }

    /// Updates and persists the response-speed preference for a single
    /// session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_speed_mode(
        &mut self,
        services: &AppServices,
        session_id: &str,
        speed_mode: SpeedMode,
    ) -> Result<(), SessionError> {
        self.session_index_or_err(session_id)?;

        services
            .db()
            .sessions()
            .update_session_speed_mode(session_id, speed_mode)
            .await?;

        services.emit_app_event(AppEvent::SessionSpeedModeUpdated {
            session_id: SessionId::from(session_id),
            speed_mode,
        });

        Ok(())
    }

    /// Updates and persists the personality selected for a single session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub async fn set_session_personality(
        &mut self,
        services: &AppServices,
        session_id: &str,
        personality_id: Option<String>,
    ) -> Result<(), SessionError> {
        self.session_index_or_err(session_id)?;

        services
            .db()
            .sessions()
            .update_session_personality_id(session_id, personality_id.clone())
            .await?;

        services.emit_app_event(AppEvent::SessionPersonalityUpdated {
            personality_id,
            session_id: SessionId::from(session_id),
        });

        Ok(())
    }

    /// Updates one session model for automatic speed-mode compatibility
    /// without changing the project's default model selection.
    ///
    /// # Errors
    /// Returns an error if the session is missing or persistence fails.
    pub(crate) async fn set_session_model_for_speed_mode(
        &mut self,
        services: &AppServices,
        session_id: &str,
        session_agent: AgentSelection,
    ) -> Result<(), SessionError> {
        self.set_session_model_with_default_persistence(services, session_id, session_agent, false)
            .await
    }

    /// Applies a session model update with explicit project-default
    /// persistence behavior.
    async fn set_session_model_with_default_persistence(
        &mut self,
        services: &AppServices,
        session_id: &str,
        session_agent: AgentSelection,
        persist_last_used_model_as_default: bool,
    ) -> Result<(), SessionError> {
        let session_model = session_agent.model();
        let session_index = self.session_index_or_err(session_id)?;
        let agent_changed = self
            .session_at(session_index)
            .is_some_and(|session| session.agent != session_agent);
        let model_changed = self
            .session_at(session_index)
            .is_some_and(|session| session.agent.model() != session_model);
        let session_agent_kind = session_agent.kind().to_string();

        services
            .db()
            .sessions()
            .update_session_agent_model(session_id, &session_agent_kind, session_model.as_str())
            .await?;
        if agent_changed {
            services
                .db()
                .sessions()
                .update_session_provider_conversation_id(session_id, None)
                .await?;
            services
                .db()
                .sessions()
                .update_session_instruction_conversation_id(session_id, None)
                .await?;

            self.clear_session_worker(session_id);
        }

        if persist_last_used_model_as_default
            && let session_project_id = services
                .db()
                .sessions()
                .load_session_project_id(session_id)
                .await?
            && Self::should_persist_last_used_model_as_default(services, session_project_id).await?
            && let Some(project_id) = session_project_id
        {
            services
                .db()
                .settings()
                .upsert_project_setting(
                    project_id,
                    SettingName::DefaultSmartAgent,
                    session_agent.kind().name(),
                )
                .await?;
            services
                .db()
                .settings()
                .upsert_project_setting(
                    project_id,
                    SettingName::DefaultSmartModel,
                    session_model.as_str(),
                )
                .await?;
        }

        services.emit_app_event(AppEvent::SessionModelUpdated {
            session_id: SessionId::from(session_id),
            session_agent,
        });

        if agent_changed || model_changed {
            self.mark_history_replay_pending(session_id);
        }

        Ok(())
    }

    /// Returns whether session model switches should also persist the
    /// `DefaultSmartAgent` and `DefaultSmartModel` setting pair.
    async fn should_persist_last_used_model_as_default(
        services: &AppServices,
        project_id: Option<i64>,
    ) -> Result<bool, SessionError> {
        let Some(project_id) = project_id else {
            return Ok(false);
        };

        let should_persist = services
            .db()
            .settings()
            .get_project_setting(project_id, SettingName::LastUsedModelAsDefault)
            .await?
            .and_then(|setting_value| setting_value.parse::<bool>().ok())
            .unwrap_or(false);

        Ok(should_persist)
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.state
            .table_state
            .selected()
            .and_then(|index| self.state.sessions.get(index))
    }

    /// Returns the session snapshot for one list index, if it still exists.
    pub fn session_at(&self, session_index: usize) -> Option<&Session> {
        self.state.sessions.get(session_index)
    }

    /// Returns the session identifier for the given list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<SessionId> {
        self.state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
    }

    /// Resolves a stable session identifier to the current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.state.session_index_for_id(session_id)
    }

    /// Returns the browser-openable URL for one linked review request.
    ///
    /// # Errors
    /// Returns an error if the session is missing, has no linked review
    /// request, or the stored summary is missing a usable web URL.
    pub fn review_request_web_url(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<String, SessionError> {
        let session = self.session_or_err(session_id)?;
        let review_request = session.review_request.as_ref().ok_or_else(|| {
            SessionError::Workflow("Session has no linked review request".to_string())
        })?;

        services
            .review_request_client()
            .review_request_web_url(&review_request.summary)
            .map_err(|error| SessionError::Workflow(error.detail_message()))
    }

    /// Deletes the currently selected session and cleans related resources.
    ///
    /// After persistence and filesystem cleanup, this triggers session and
    /// project-list reloads through app refresh events.
    pub async fn delete_selected_session(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let Some(cleanup) = self
            .remove_selected_session_from_state_and_db(projects, services)
            .await
        else {
            return;
        };

        Self::cleanup_deleted_session_resources(
            services.fs_client(),
            services.git_client(),
            cleanup,
        )
        .await;
    }

    /// Deletes the selected session while deferring filesystem cleanup to a
    /// background task.
    pub async fn delete_selected_session_deferred_cleanup(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let Some(cleanup) = self
            .remove_selected_session_from_state_and_db(projects, services)
            .await
        else {
            return;
        };

        let fs_client = services.fs_client();
        let git_client = services.git_client();
        tokio::spawn(async move {
            SessionManager::cleanup_deleted_session_resources(fs_client, git_client, cleanup).await;
        });
    }

    /// Removes the selected session from app state and persistence, returning
    /// deferred cleanup instructions for git and filesystem resources.
    async fn remove_selected_session_from_state_and_db(
        &mut self,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Option<DeletedSessionCleanup> {
        let selected_index = self.state.table_state.selected()?;
        if selected_index >= self.state.sessions.len() {
            return None;
        }

        let session_id = self.state.sessions[selected_index].id.clone();
        if services
            .db()
            .sessions()
            .load_session_preparation(&session_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|preparation| preparation.state == db::SessionPreparationState::Preparing)
        {
            if let Err(error) = self.cancel_session(services, &session_id).await {
                warn!(session_id = %session_id, %error, "failed to cancel workspace preparation before deletion");
            }
            return None;
        }
        let session = self.remove_session_at(selected_index)?;
        self.state.remove_handle(&session.id);
        self.remove_session_worktree_availability(&session.id);
        self.remove_at_mention_index_for_root(&session.folder);
        self.abort_title_generation_task(&session.id);
        self.clear_history_replay_pending(&session.id);
        SessionTaskService::remove_session_update_version(
            &services.session_update_versions(),
            &session.id,
        );

        if let Err(error) = services
            .db()
            .operations()
            .request_cancel_for_session_operations(&session.id)
            .await
        {
            warn!(
                session_id = %session.id,
                error = %error,
                "failed to cancel pending session operations during deletion"
            );
        }
        self.clear_session_worker(&session.id);
        if let Err(error) = services.db().sessions().delete_session(&session.id).await {
            warn!(
                session_id = %session.id,
                error = %error,
                "failed to delete session record during session deletion"
            );
        }
        services.emit_session_and_project_refresh_events();

        let staged_draft_root = services.base_path().join(&session.id);

        Some(DeletedSessionCleanup {
            branch_name: session_branch(&session.id),
            folder: session.folder,
            has_git_branch: projects.has_git_branch(),
            session_id: session.id,
            staged_draft_root,
            working_dir: projects.working_dir().to_path_buf(),
        })
    }

    /// Deletes worktree resources for a previously removed session.
    async fn cleanup_deleted_session_resources(
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn git::GitClient>,
        cleanup: DeletedSessionCleanup,
    ) {
        let repo_root = if cleanup.has_git_branch {
            git_client.find_git_repo_root(cleanup.working_dir).await
        } else {
            None
        };

        let cleanup_errors = Self::cleanup_session_worktree_resources(
            fs_client.clone(),
            git_client,
            cleanup.folder,
            cleanup.branch_name,
            repo_root,
            cleanup.has_git_branch,
        )
        .await;
        Self::warn_cleanup_errors(&cleanup.session_id, &cleanup_errors);
        if fs_client.is_dir(cleanup.staged_draft_root.clone())
            && let Err(error) = fs_client.remove_dir_all(cleanup.staged_draft_root).await
        {
            warn!(
                session_id = %cleanup.session_id,
                error = %error,
                "failed to remove staged draft directory during session deletion"
            );
        }
        Self::cleanup_session_temp_directory(fs_client, &cleanup.session_id).await;
    }

    /// Converts one refreshed summary into persisted review-request metadata.
    pub(super) fn build_review_request(
        &self,
        summary: forge::ReviewRequestSummary,
    ) -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: unix_timestamp_from_system_time(self.state.clock.now_system_time()),
            summary,
        }
    }

    /// Persists one normalized review-request summary for a session.
    ///
    /// # Errors
    /// Returns an error if the session disappears or persistence fails.
    pub(crate) async fn store_review_request_summary(
        &mut self,
        services: &AppServices,
        session_id: &str,
        summary: forge::ReviewRequestSummary,
    ) -> Result<ReviewRequest, SessionError> {
        let session_index = self.session_index_or_err(session_id)?;
        let review_request = self.build_review_request(summary);

        self.store_review_request(services, session_index, review_request)
            .await
    }

    /// Persists one linked review request in memory and the database.
    ///
    /// # Errors
    /// Returns an error if the session disappears or persistence fails.
    pub(super) async fn store_review_request(
        &mut self,
        services: &AppServices,
        session_index: usize,
        review_request: ReviewRequest,
    ) -> Result<ReviewRequest, SessionError> {
        let session_id = self
            .state
            .sessions
            .get(session_index)
            .map(|session| session.id.clone())
            .ok_or(SessionError::NotFound)?;
        services
            .db()
            .reviews()
            .update_session_review_request(&session_id, Some(review_request.clone()))
            .await?;

        let Some(session) = self.state.sessions.get_mut(session_index) else {
            return Err(SessionError::NotFound);
        };
        session.review_request = Some(review_request.clone());

        Ok(review_request)
    }

    /// Validates and queues a follow-up prompt for an existing session.
    ///
    /// Gathers reply context, appends the prompt line to session output, builds
    /// a [`SessionCommand::Run`] with the appropriate [`AgentRequestKind`],
    /// and enqueues it on the session worker. Returns `true` only when the
    /// command reached the worker queue, so callers can defer optimistic status
    /// advances until the reply is genuinely in flight.
    async fn reply_impl(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: TurnPrompt,
        session_agent: AgentSelection,
        options: ReplyOptions,
    ) -> bool {
        let ReplyOptions {
            defer_prompt_until_enqueued,
            eligibility,
            operation_id,
            persist_prompt,
            prompt_presentation,
            requires_existing_worker,
            review_comment_thread_ids,
        } = options;
        // A saved fork prompt still needs the copied history if its worker
        // rejected acceptance after an earlier successful enqueue.
        let should_replay_history = self.should_replay_history(session_id)
            || operation_id.as_deref() == Some(format!("workspace:{session_id}").as_str());
        let (replay_transcript, is_first_message, persisted_session_id, title_to_save) = match self
            .prepare_reply_context(session_id, &prompt, should_replay_history, eligibility)
        {
            Ok(reply_context) => reply_context,
            Err(error) => {
                self.append_reply_status_error(services, session_id, &error)
                    .await;

                return false;
            }
        };

        let app_event_tx = services.event_sender();

        let Ok(handles) = self.session_handles_or_err(&persisted_session_id) else {
            return false;
        };

        let transcript = Arc::clone(&handles.transcript);
        let status_transition =
            StatusTransition::from_services(services, handles, persisted_session_id.clone());

        self.persist_initial_reply_metadata(
            services,
            &status_transition,
            &persisted_session_id,
            &prompt.text,
            title_to_save,
        )
        .await;

        if persist_prompt && !defer_prompt_until_enqueued {
            self.append_reply_prompt_line(
                services,
                &transcript,
                &app_event_tx,
                &persisted_session_id,
                &prompt,
                prompt_presentation,
            )
            .await;
        }
        let published_upstream_ref = self
            .session_or_err(&persisted_session_id)
            .ok()
            .and_then(|session| session.published_upstream_ref.clone());
        let idempotent = operation_id.is_some();

        let command = Self::build_session_command(BuildSessionCommandInput {
            is_first_message,
            operation_id,
            prompt: prompt.clone(),
            published_upstream_ref,
            replay_transcript,
            review_comment_thread_ids,
            session_agent,
        });
        let preparation_owned = command.is_preparation_prompt(&persisted_session_id);
        let enqueued = self
            .enqueue_reply_command(
                services,
                &transcript,
                &persisted_session_id,
                &prompt,
                command,
                ReplyEnqueueOptions {
                    idempotent,
                    report_failure_in_transcript: !defer_prompt_until_enqueued,
                    requires_existing_worker,
                },
            )
            .await;
        if enqueued == ReplyEnqueueOutcome::Enqueued
            && defer_prompt_until_enqueued
            && persist_prompt
            && !preparation_owned
        {
            self.append_reply_prompt_line(
                services,
                &transcript,
                &app_event_tx,
                &persisted_session_id,
                &prompt,
                prompt_presentation,
            )
            .await;
        }

        if enqueued != ReplyEnqueueOutcome::Failed && should_replay_history {
            self.clear_history_replay_pending(&persisted_session_id);
        }

        enqueued != ReplyEnqueueOutcome::Failed
    }

    /// Persists first-message metadata and starts a reply that targets a
    /// blank draft session.
    async fn persist_initial_reply_metadata(
        &self,
        services: &AppServices,
        status_transition: &StatusTransition,
        session_id: &SessionId,
        prompt: &str,
        title: Option<String>,
    ) {
        let Some(title) = title else {
            return;
        };

        self.persist_first_message_metadata(services, session_id, prompt, &title)
            .await;
        if !status_transition.apply(Status::InProgress).await {
            warn!(
                session_id = %session_id,
                "skipped reply status update because the in-memory status did not transition to in-progress"
            );
        }
    }

    /// Validates reply eligibility and gathers per-session values needed for
    /// queueing a reply command.
    ///
    /// # Errors
    /// Returns a [`SessionError::Workflow`] when session status does not allow
    /// replying.
    fn prepare_reply_context(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
        should_replay_history: bool,
        reply_eligibility: ReplyEligibility,
    ) -> Result<ReplyContext, SessionError> {
        let session_index = self.session_index_or_err(session_id)?;
        if !self.can_reply_to_session_in_stack(session_id) {
            return Err(SessionError::Workflow(
                "Stacked replies can only run when no other stack session is active".to_string(),
            ));
        }

        let session = &mut self.state.sessions[session_index];

        let is_first_message = session.status == Status::Draft && session.prompt.is_empty();
        if !reply_eligibility.allows(session.status, is_first_message) {
            return Err(SessionError::Workflow(
                "Session must be in review status".to_string(),
            ));
        }

        let mut title_to_save = None;
        if is_first_message {
            session.prompt.clone_from(&prompt.text);
            let title = prompt.text.clone();
            session.title = Some(title.clone());
            title_to_save = Some(title);
        }

        let replay_transcript = if !is_first_message
            && (should_replay_history
                || agent::transport_mode(session.agent.kind()).uses_app_server())
        {
            session
                .transcript
                .as_ref()
                .and_then(SessionTranscript::replay_text)
        } else {
            None
        };

        Ok((
            replay_transcript,
            is_first_message,
            session.id.clone(),
            title_to_save,
        ))
    }

    /// Persists first-message prompt/title metadata before queueing execution.
    ///
    /// This writes the initial prompt/title.
    ///
    /// Title generation is triggered by the turn worker while the title
    /// remains provisional.
    async fn persist_first_message_metadata(
        &self,
        services: &AppServices,
        session_id: &str,
        prompt: &str,
        title: &str,
    ) {
        if let Err(error) = services
            .db()
            .sessions()
            .update_session_provisional_title(session_id, title)
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to persist first-message session title"
            );
        }

        if let Err(error) = services
            .db()
            .sessions()
            .update_session_prompt(session_id, prompt)
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to persist first-message session prompt"
            );
        }
    }

    /// Appends the user reply marker line to session output.
    async fn append_reply_prompt_line(
        &mut self,
        services: &AppServices,
        transcript: &Arc<Mutex<SessionTranscript>>,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        prompt: &TurnPrompt,
        presentation: ReplyPromptPresentation,
    ) {
        let prompt_transcript_text = prompt.transcript_text();
        SessionTaskService::append_session_transcript_message(
            transcript,
            services.db(),
            app_event_tx,
            &services.session_update_versions(),
            session_id,
            SessionTranscriptMessageAppend {
                kind: presentation.message_kind(),
                raw_content: &prompt_transcript_text,
            },
        )
        .await;
        if presentation.is_visible() {
            let reply_line = Self::formatted_prompt_output(prompt, true);
            self.set_active_prompt_output(session_id, reply_line);
        }
    }

    /// Formats one user prompt block for persisted session output.
    ///
    /// The first line uses `USER_PROMPT_PREFIX`; continuation lines use
    /// `USER_PROMPT_CONTINUATION_PREFIX` so embedded blank lines remain inside
    /// the prompt block instead of being interpreted as prompt terminators.
    fn formatted_prompt_output(prompt: &TurnPrompt, prepend_newline: bool) -> String {
        let prompt_text = prompt.transcript_text();
        let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
        let mut formatted_lines = Vec::with_capacity(prompt_lines.len());

        for (index, prompt_line) in prompt_lines.into_iter().enumerate() {
            let prefix = if index == 0 {
                USER_PROMPT_PREFIX
            } else {
                USER_PROMPT_CONTINUATION_PREFIX
            };

            formatted_lines.push(format!("{prefix}{prompt_line}"));
        }

        let prompt_block = formatted_lines.join("\n");
        if prepend_newline {
            return format!("\n{prompt_block}\n\n");
        }

        format!("{prompt_block}\n\n")
    }

    /// Appends one newly staged prompt onto the persisted draft-session
    /// prompt text stored in `session.prompt`.
    ///
    /// Attachment placeholders are renumbered sequentially so draft sessions
    /// can keep one flat prompt string while preserving a stable attachment
    /// order across multiple staging passes.
    fn append_staged_prompt(
        existing_prompt: &str,
        prompt: &TurnPrompt,
        next_attachment_number: usize,
    ) -> String {
        let staged_prompt = Self::renumbered_prompt_text(prompt, next_attachment_number);
        if existing_prompt.is_empty() {
            return staged_prompt;
        }

        format!("{existing_prompt}\n\n{staged_prompt}")
    }

    /// Returns the staged prompt text after renumbering any attachment
    /// placeholders to their global draft-session positions.
    fn renumbered_prompt_text(prompt: &TurnPrompt, next_attachment_number: usize) -> String {
        let mut prompt_text = prompt.text.clone();

        for (offset, attachment) in prompt.attachments.iter().enumerate() {
            let placeholder = format!("[Image #{}]", next_attachment_number.saturating_add(offset));
            prompt_text = replace_first(&prompt_text, &attachment.placeholder, &placeholder);
        }

        prompt_text
    }

    /// Returns the prompt attachments rewritten to the global draft-session
    /// placeholder sequence.
    fn renumbered_attachments(
        prompt: &TurnPrompt,
        next_attachment_number: usize,
    ) -> Vec<TurnPromptAttachment> {
        prompt
            .attachments
            .iter()
            .enumerate()
            .map(|(offset, attachment)| TurnPromptAttachment {
                placeholder: format!("[Image #{}]", next_attachment_number.saturating_add(offset)),
                local_image_path: attachment.local_image_path.clone(),
            })
            .collect()
    }

    /// Builds a queued command for starting or resuming a session interaction.
    ///
    /// Creates a [`SessionCommand::Run`] with
    /// [`AgentRequestKind::SessionStart`] for first messages and
    /// [`AgentRequestKind::SessionResume`] with optional transcript replay
    /// for subsequent replies.
    fn build_session_command(input: BuildSessionCommandInput) -> SessionCommand {
        let BuildSessionCommandInput {
            is_first_message,
            operation_id,
            prompt,
            published_upstream_ref,
            replay_transcript,
            review_comment_thread_ids,
            session_agent,
        } = input;
        let operation_id = operation_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let request_kind = if is_first_message {
            AgentRequestKind::SessionStart
        } else {
            AgentRequestKind::SessionResume
        };

        SessionCommand::Run {
            operation_id,
            request_kind,
            replay_transcript,
            prompt,
            turn_metadata: TurnMetadata {
                published_upstream_ref,
                review_comment_thread_ids,
                session_agent,
            },
        }
    }

    /// Appends a reply-error notice to the session output so the user sees
    /// why the reply was rejected.
    async fn append_reply_status_error(
        &self,
        services: &AppServices,
        session_id: &str,
        error: &SessionError,
    ) {
        let status_error = TranscriptNotice::ReplyError.format(error);
        let Ok(handles) = self.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = services.event_sender();

        SessionTaskService::append_workflow_notice(
            &handles.transcript,
            services.db(),
            &app_event_tx,
            &services.session_update_versions(),
            session_id,
            &status_error,
        )
        .await;
    }

    /// Returns whether the command was newly enqueued, previously accepted,
    /// or rejected with a reply-error notice.
    async fn enqueue_reply_command(
        &mut self,
        services: &AppServices,
        transcript: &Arc<Mutex<SessionTranscript>>,
        persisted_session_id: &str,
        prompt: &TurnPrompt,
        command: SessionCommand,
        options: ReplyEnqueueOptions,
    ) -> ReplyEnqueueOutcome {
        let preparation_owned = command.is_preparation_prompt(persisted_session_id);
        let enqueue_result = if options.idempotent {
            self.enqueue_session_command_idempotently(services, persisted_session_id, command)
                .await
        } else if options.requires_existing_worker {
            let persisted_session_id = SessionId::from(persisted_session_id);
            self.worker_service_mut()
                .enqueue_existing_session_command(services, &persisted_session_id, command)
                .await
                .map(|_| true)
        } else {
            self.enqueue_session_command(services, persisted_session_id, command)
                .await
                .map(|()| true)
        };
        let newly_enqueued = match enqueue_result {
            Ok(newly_enqueued) => newly_enqueued,
            Err(error) => {
                if !preparation_owned {
                    self.cleanup_prompt_attachment_files(services, prompt).await;
                }

                if options.report_failure_in_transcript {
                    let error_line = TranscriptNotice::ReplyError.format(error);
                    let app_event_tx = services.event_sender();
                    SessionTaskService::append_workflow_notice(
                        transcript,
                        services.db(),
                        &app_event_tx,
                        &services.session_update_versions(),
                        persisted_session_id,
                        &error_line,
                    )
                    .await;
                }

                return ReplyEnqueueOutcome::Failed;
            }
        };

        if newly_enqueued {
            ReplyEnqueueOutcome::Enqueued
        } else {
            ReplyEnqueueOutcome::AlreadyAccepted
        }
    }

    /// Spawns one detached model command that generates a title from stable
    /// persisted session context plus the latest request.
    ///
    /// Each usable generated title is persisted only when no newer usable
    /// candidate or authoritative title has already been accepted. Empty
    /// responses do not invalidate older candidates. A `RefreshSessions`
    /// event is emitted after a title is applied so list-mode snapshots pick
    /// it up. Callers that can supersede draft-title generation should retain
    /// the returned task handle and abort any older in-flight task before
    /// replacing it.
    pub(super) async fn spawn_session_title_generation_task(
        input: SessionTitleGenerationTaskInput,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let SessionTitleGenerationTaskInput {
            app_event_tx,
            db,
            folder,
            latest_request,
            one_shot_client,
            requires_provisional_title,
            reasoning_level,
            session_agent,
            session_id: persisted_session_id,
            speed_mode,
            tracked_generation,
        } = input;
        let tracked_completion =
            tracked_generation.map(|generation| TitleGenerationTaskCompletion {
                generation,
                session_id: persisted_session_id.clone(),
            });

        let title_generation = match db
            .sessions()
            .begin_session_title_generation(&persisted_session_id, requires_provisional_title)
            .await
        {
            Ok(Some(title_generation)) => title_generation,
            Ok(None) => {
                Self::emit_title_generation_finished_event(
                    &app_event_tx,
                    tracked_completion.as_ref(),
                );

                return None;
            }
            Err(error) => {
                warn!(
                    session_id = %persisted_session_id,
                    error = %error,
                    "failed to claim session title generation"
                );
                Self::emit_title_generation_finished_event(
                    &app_event_tx,
                    tracked_completion.as_ref(),
                );

                return None;
            }
        };

        Some(tokio::spawn(
            Self::run_claimed_session_title_generation_task(
                ClaimedSessionTitleGenerationTaskInput {
                    app_event_tx,
                    db,
                    folder,
                    latest_request,
                    one_shot_client,
                    reasoning_level,
                    session_agent,
                    session_id: persisted_session_id,
                    speed_mode,
                    title_generation,
                    tracked_completion,
                },
            ),
        ))
    }

    /// Runs one title-generation command after its database revision has been
    /// claimed.
    async fn run_claimed_session_title_generation_task(
        input: ClaimedSessionTitleGenerationTaskInput,
    ) {
        let ClaimedSessionTitleGenerationTaskInput {
            app_event_tx,
            db,
            folder,
            latest_request,
            one_shot_client,
            reasoning_level,
            session_agent,
            session_id: persisted_session_id,
            speed_mode,
            title_generation,
            tracked_completion,
        } = input;
        let Some(title_context) =
            Self::load_session_title_generation_context(&db, &persisted_session_id, latest_request)
                .await
        else {
            Self::emit_title_generation_finished_event(&app_event_tx, tracked_completion.as_ref());

            return;
        };
        let title_generation_prompt = Self::session_title_generation_prompt(&title_context);

        let Some(title_response) = Self::run_title_generation_command(
            folder.as_path(),
            &title_generation_prompt,
            session_agent,
            reasoning_level,
            &persisted_session_id,
            speed_mode,
            one_shot_client.as_ref(),
        )
        .await
        else {
            Self::emit_title_generation_finished_event(&app_event_tx, tracked_completion.as_ref());

            return;
        };

        let Some(generated_title) = Self::parse_generated_session_title(&title_response) else {
            Self::emit_title_generation_finished_event(&app_event_tx, tracked_completion.as_ref());

            return;
        };

        if Self::is_generated_session_title_request_copy(&generated_title, &title_context) {
            Self::emit_title_generation_finished_event(&app_event_tx, tracked_completion.as_ref());

            return;
        }

        match db
            .sessions()
            .update_session_title_for_generation(
                &persisted_session_id,
                title_generation,
                &generated_title,
            )
            .await
        {
            Ok(true) => {
                if app_event_tx.send(AppEvent::RefreshSessions).is_err() {
                    warn!(
                        session_id = %persisted_session_id,
                        "failed to refresh sessions after title generation because the app event receiver is closed"
                    );
                }
            }
            Ok(false) => {}
            Err(error) => {
                warn!(
                    session_id = %persisted_session_id,
                    error = %error,
                    "failed to persist generated session title"
                );
            }
        }

        Self::emit_title_generation_finished_event(&app_event_tx, tracked_completion.as_ref());
    }

    /// Loads the stable session context used to title one claimed generation.
    async fn load_session_title_generation_context(
        db: &db::AppRepositories,
        session_id: &str,
        latest_request: String,
    ) -> Option<SessionTitleGenerationContext> {
        match db.sessions().load_session(session_id).await {
            Ok(Some(session)) => Some(SessionTitleGenerationContext {
                current_title: session.title.unwrap_or_default(),
                latest_request,
                original_request: session.prompt,
            }),
            Ok(None) => {
                warn!(
                    session_id,
                    "failed to load session title context because the session is missing"
                );

                None
            }
            Err(error) => {
                warn!(
                    session_id,
                    error = %error,
                    "failed to load session title context"
                );

                None
            }
        }
    }

    /// Emits one tracked title-generation completion event when the task was
    /// registered in the per-session task map.
    fn emit_title_generation_finished_event(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        tracked_completion: Option<&TitleGenerationTaskCompletion>,
    ) {
        let Some(tracked_completion) = tracked_completion else {
            return;
        };

        if app_event_tx
            .send(AppEvent::SessionTitleGenerationFinished {
                generation: tracked_completion.generation,
                session_id: tracked_completion.session_id.clone(),
            })
            .is_err()
        {
            warn!(
                session_id = %tracked_completion.session_id,
                generation = tracked_completion.generation,
                "failed to send session title generation completion event because the app event receiver is closed"
            );
        }
    }

    /// Executes title generation through an injected one-shot boundary.
    async fn run_title_generation_command(
        folder: &Path,
        prompt: &str,
        session_agent: AgentSelection,
        reasoning_level: ReasoningLevel,
        session_id: &str,
        speed_mode: SpeedMode,
        one_shot_client: &dyn OneShotClient,
    ) -> Option<String> {
        for attempt in 1..=SESSION_TITLE_GENERATION_MAX_ATTEMPTS {
            let result = one_shot_client
                .submit(agent::OneShotRequest {
                    provider_call_budget: None,
                    agent_kind: session_agent.kind(),
                    child_pid: None,
                    folder: folder.to_path_buf(),
                    model: session_agent.model(),
                    permission_mode: ag_agent::PermissionMode::ReadOnly,
                    prompt: prompt.to_string(),
                    request_kind: AgentRequestKind::UtilityPrompt,
                    reasoning_level,
                    speed_mode,
                })
                .await;

            match result {
                Ok(submission) => return Some(submission.response.to_answer_display_text()),
                Err(error) => warn!(
                    session_id,
                    attempt,
                    max_attempts = SESSION_TITLE_GENERATION_MAX_ATTEMPTS,
                    error = %error,
                    "session title generation request failed"
                ),
            }
        }

        None
    }

    /// Builds the title-generation instruction prompt from stable session
    /// context while retaining headroom for provider protocol envelopes.
    fn session_title_generation_prompt(context: &SessionTitleGenerationContext) -> String {
        let current_title = Self::truncate_session_title_context(
            &context.current_title,
            SESSION_TITLE_CURRENT_TITLE_MAX_BYTES,
        );
        let latest_request = Self::truncate_session_title_context(
            &context.latest_request,
            SESSION_TITLE_LATEST_REQUEST_MAX_BYTES,
        );
        let original_request = Self::truncate_session_title_context(
            &context.original_request,
            SESSION_TITLE_ORIGINAL_REQUEST_MAX_BYTES,
        );
        let template = SessionTitleGenerationPromptTemplate {
            current_title: &current_title,
            latest_request: &latest_request,
            original_request: &original_request,
        };

        template.render().unwrap_or_default()
    }

    /// Truncates one title-context field at a UTF-8 boundary within its byte
    /// budget.
    fn truncate_session_title_context(value: &str, max_bytes: usize) -> String {
        if value.len() <= max_bytes {
            return value.to_string();
        }

        let content_budget =
            max_bytes.saturating_sub(SESSION_TITLE_CONTEXT_TRUNCATION_MARKER.len());
        let mut boundary = content_budget.min(value.len());
        while !value.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }

        format!(
            "{}{}",
            value[..boundary].trim_end(),
            SESSION_TITLE_CONTEXT_TRUNCATION_MARKER
        )
    }

    /// Returns whether a candidate merely repeats persisted request text.
    fn is_generated_session_title_request_copy(
        title: &str,
        context: &SessionTitleGenerationContext,
    ) -> bool {
        [
            &context.current_title,
            &context.latest_request,
            &context.original_request,
        ]
        .into_iter()
        .filter(|request| !request.trim().is_empty())
        .any(|request| Self::is_normalized_title_copy(title, request))
    }

    /// Compares title text after removing casing, punctuation, and line-layout
    /// differences.
    fn is_normalized_title_copy(title: &str, request: &str) -> bool {
        let normalized_title = Self::normalize_title_comparison_text(title);
        if normalized_title.is_empty() {
            return false;
        }

        Self::normalize_title_comparison_text(request) == normalized_title
            || request.lines().any(|line| {
                let normalized_line = Self::normalize_title_comparison_text(line);

                !normalized_line.is_empty() && normalized_line == normalized_title
            })
    }

    /// Normalizes text for prompt-copy detection without changing persisted
    /// output.
    fn normalize_title_comparison_text(value: &str) -> String {
        value
            .split(|character: char| !character.is_alphanumeric())
            .filter(|segment| !segment.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Parses model output into a normalized one-line session title.
    ///
    /// Accepts either a plain-text title line or a protocol-wrapped response
    /// (`{"answer":"..."}`) whose first answer line contains the title.
    ///
    /// Returns [`None`] when no usable title line is present.
    fn parse_generated_session_title(content: &str) -> Option<String> {
        let content = content.trim();
        if content.is_empty() {
            return None;
        }

        if let Ok(protocol_response) = parse_agent_response_strict(content) {
            return Self::parse_generated_session_title_from_protocol_response(&protocol_response);
        }

        let first_line = Self::first_nonempty_line(content)?;

        Self::normalize_generated_session_title(first_line)
    }

    /// Extracts the first usable title candidate from protocol `answer`
    /// content.
    fn parse_generated_session_title_from_protocol_response(
        protocol_response: &AgentResponse,
    ) -> Option<String> {
        for answer in protocol_response.answers() {
            if let Some(first_line) = Self::first_nonempty_line(&answer)
                && let Some(parsed_title) = Self::normalize_generated_session_title(first_line)
            {
                return Some(parsed_title);
            }
        }

        None
    }

    /// Returns the first non-empty line from model output content.
    fn first_nonempty_line(content: &str) -> Option<&str> {
        content.lines().find_map(|line| {
            let trimmed_line = line.trim();
            if trimmed_line.is_empty() {
                return None;
            }

            Some(trimmed_line)
        })
    }

    /// Normalizes one candidate title and rejects status-like model output.
    ///
    /// Title generation runs through a general utility prompt, so providers can
    /// occasionally return first-person progress prose. Those candidates are
    /// rejected instead of overwriting the user-prompt fallback title.
    fn normalize_generated_session_title(candidate: &str) -> Option<String> {
        let mut title = candidate.trim().to_string();
        if let Some((prefix, remainder)) = title.split_once(':')
            && prefix.trim().eq_ignore_ascii_case("title")
        {
            title = remainder.trim().to_string();
        }

        title = title
            .trim_matches(|ch| matches!(ch, '"' | '\'' | '`'))
            .trim()
            .to_string();

        if title.is_empty() {
            return None;
        }

        if !Self::is_generated_session_title_candidate(&title) {
            return None;
        }

        Some(title)
    }

    /// Returns whether a normalized generated title looks like requested work
    /// rather than model progress, narration, or other non-title prose.
    fn is_generated_session_title_candidate(title: &str) -> bool {
        if title.chars().count() > GENERATED_SESSION_TITLE_MAX_CHARACTERS {
            return false;
        }

        if Self::starts_with_first_person_pronoun(title) {
            return false;
        }

        if Self::starts_with_progress_prefix(title) {
            return false;
        }

        true
    }

    /// Returns whether the title begins with a first-person pronoun shape.
    fn starts_with_first_person_pronoun(title: &str) -> bool {
        let mut characters = title.chars();
        if !matches!(characters.next(), Some('I' | 'i')) {
            return false;
        }

        matches!(characters.next(), Some(' ' | '\'' | '\u{2019}'))
    }

    /// Returns whether the title begins with a progress/status gerund.
    fn starts_with_progress_prefix(title: &str) -> bool {
        let lower_title = title.to_ascii_lowercase();

        GENERATED_SESSION_TITLE_PROGRESS_PREFIXES
            .iter()
            .any(|prefix| lower_title.starts_with(prefix))
    }

    /// Resolves project defaults unless the caller supplied a deterministic
    /// launch-settings snapshot.
    pub(crate) async fn resolve_session_creation_settings(
        &mut self,
        services: &AppServices,
        project_id: i64,
        creation_settings: Option<SessionCreationSettings>,
    ) -> Result<SessionCreationSettings, SessionError> {
        if let Some(creation_settings) = creation_settings {
            return Ok(creation_settings);
        }

        let agent = self
            .resolve_default_session_agent(services, project_id)
            .await;
        let reasoning_level = services
            .db()
            .settings()
            .load_project_reasoning_level(project_id, SettingName::DefaultSmartReasoningLevel)
            .await?;
        let response_style = services
            .db()
            .settings()
            .load_project_response_style(project_id, SettingName::DefaultResponseStyle)
            .await?;
        let speed_mode = services
            .db()
            .settings()
            .load_project_speed_mode(project_id, SettingName::DefaultSmartSpeedMode)
            .await?;
        let speed_mode = if agent.kind().supports_speed_mode() {
            speed_mode
        } else {
            SpeedMode::Normal
        };
        let agent = agent.compatible_with_speed_mode(speed_mode);
        self.default_session_model = agent.model();

        Ok(SessionCreationSettings {
            agent,
            permission_mode: PermissionMode::AutoEdit,
            personality_id: None,
            reasoning_level,
            response_style,
            role: crate::domain::session::SessionRole::Worker,
            speed_mode,
        })
    }

    /// Resolves the default agent/model selection for a new session.
    async fn resolve_default_session_agent(
        &self,
        services: &AppServices,
        project_id: i64,
    ) -> AgentSelection {
        let available_agent_kinds = services.available_agent_kinds();
        let fallback_agent_kind = available_agent_kinds
            .first()
            .copied()
            .unwrap_or(AgentKind::Antigravity);
        let fallback_selection = crate::domain::agent::resolve_agent_selection_for_model(
            self.default_session_model,
            fallback_agent_kind,
            &available_agent_kinds,
        );

        setting::load_default_smart_agent_setting(services, Some(project_id), fallback_selection)
            .await
    }

    /// Reverts filesystem and database changes after session creation failure.
    async fn rollback_failed_session_creation(
        services: &AppServices,
        folder: &Path,
        repo_root: &Path,
        session_id: &str,
        worktree_branch: &str,
        session_saved: bool,
        has_worktree: bool,
    ) {
        if session_saved {
            if let Err(error) = services.db().sessions().delete_session(session_id).await {
                warn!(
                    session_id = session_id,
                    error = %error,
                    "failed to roll back persisted session metadata"
                );
            }
            SessionTaskService::remove_session_update_version(
                &services.session_update_versions(),
                session_id,
            );
        }

        // A rejected checkout can refer to a branch that already existed;
        // only remove Git resources when this attempt has a worktree.
        if has_worktree {
            let git_client = services.git_client();
            let folder = folder.to_path_buf();
            let repo_root = repo_root.to_path_buf();
            let worktree_branch = worktree_branch.to_string();
            if let Err(error) = git_client.remove_worktree(folder).await {
                warn!(
                    session_id = session_id,
                    error = %error,
                    "failed to remove worktree while rolling back session creation"
                );
            }

            if let Err(error) = git_client.delete_branch(repo_root, worktree_branch).await {
                warn!(
                    session_id = session_id,
                    error = %error,
                    "failed to delete branch while rolling back session creation"
                );
            }
        }

        if let Err(error) = services
            .fs_client()
            .remove_dir_all(folder.to_path_buf())
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to remove session worktree directory while rolling back session creation"
            );
        }

        Self::cleanup_session_temp_directory(services.fs_client(), session_id).await;
    }

    /// Records that one session was created and warns if analytics persistence
    /// fails.
    async fn record_session_creation_activity(services: &AppServices, session_id: &str) {
        let timestamp_seconds = unix_timestamp_from_system_time(services.clock().now_system_time());
        if let Err(error) = services
            .db()
            .activity()
            .insert_session_creation_activity_at(session_id, timestamp_seconds)
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to record session creation activity"
            );
        }
    }

    /// Appends text to a specific session output stream.
    pub(crate) async fn append_output_for_session(
        &self,
        services: &AppServices,
        session_id: &str,
        output: &str,
    ) {
        let Ok((session, handles)) = self.session_and_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = services.event_sender();

        SessionTaskService::append_workflow_notice(
            &handles.transcript,
            services.db(),
            &app_event_tx,
            &services.session_update_versions(),
            &session.id,
            output,
        )
        .await;
    }

    /// Removes prompt attachment files that are no longer owned by the
    /// composer or worker.
    ///
    /// Only Agentty-managed temp files under `AGENTTY_ROOT/tmp/` are removed.
    pub(crate) async fn cleanup_prompt_attachment_files(
        &self,
        services: &AppServices,
        prompt: &TurnPrompt,
    ) {
        Self::cleanup_prompt_attachment_paths(
            services.fs_client(),
            prompt.local_image_paths().cloned().collect(),
        )
        .await;
    }

    /// Cancels a review, running, unstarted draft, or draft orchestrator
    /// session.
    ///
    /// Persisted transcript metadata remains available after the worktree
    /// checkout and session branch are removed. Draft sessions that never
    /// created a worktree only update persisted state and skip worktree
    /// cleanup. Running sessions first request operation cancellation and fire
    /// the active turn's cancellation token so provider work stops before the
    /// terminal `Canceled` status is persisted.
    ///
    /// # Errors
    /// Returns an error if the session is not found or is not cancelable.
    pub async fn cancel_session(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let status_updated = self
            .cancel_single_session(services, session_id, CancellationCapability::User)
            .await?;
        if !status_updated {
            return Ok(());
        }
        self.cancel_stacked_child_sessions(services, session_id)
            .await?;

        Ok(())
    }

    /// Cancels one managed worker through the coordinator-only capability.
    ///
    /// This keeps the ordinary user action read-only while allowing campaign
    /// cancellation to reclaim a worker using the same lifecycle cleanup.
    pub(crate) async fn cancel_managed_session(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let status_updated = self
            .cancel_single_session(services, session_id, CancellationCapability::Managed)
            .await?;
        if status_updated {
            self.cancel_stacked_child_sessions(services, session_id)
                .await?;
        }

        Ok(())
    }

    /// Cancels one session without cascading into stacked children.
    async fn cancel_single_session(
        &self,
        services: &AppServices,
        session_id: &str,
        cancellation_capability: CancellationCapability,
    ) -> Result<bool, SessionError> {
        let session = self.session_or_err(session_id)?;
        if !cancellation_capability.allows(session) {
            if cancellation_capability == CancellationCapability::StackedDescendant {
                return Ok(false);
            }

            return Err(SessionError::Workflow(
                "Session is not cancelable in its current state".to_string(),
            ));
        }

        let branch_name = session_branch(&session.id);
        let folder = session.folder.clone();
        let handles = self.session_handles_or_err(session_id)?;
        let status_transition = StatusTransition::from_services(services, handles, session_id);

        // Queued work and preparation handoffs can own execution before the
        // foreground status reflects it. Terminal cancellation stops them too.
        Self::signal_session_cancellation(services, handles, session_id).await;

        let status_updated = status_transition.apply(Status::Canceled).await;

        let preparation_owns_cleanup = services
            .db()
            .sessions()
            .cancel_session_preparation(session_id)
            .await?;
        if status_updated && !preparation_owns_cleanup {
            let has_worktree = services.fs_client().is_dir(folder.clone());
            Self::spawn_canceled_session_cleanup(
                services,
                folder,
                branch_name,
                has_worktree,
                session_id.to_string(),
            );
        }

        Ok(status_updated)
    }

    /// Cancels every loaded stacked descendant of `parent_session_id`.
    ///
    /// The cascade bypasses the ordinary user-action gate, stops active or
    /// reserved descendant branch work, and attempts every loaded descendant
    /// before reporting any failures to the parent cancellation caller.
    ///
    /// # Errors
    /// Returns a workflow error listing descendants that could not be
    /// canceled after all other descendants have been attempted.
    pub(crate) async fn cancel_stacked_child_sessions(
        &self,
        services: &AppServices,
        parent_session_id: &str,
    ) -> Result<(), SessionError> {
        let mut cancellation_failures = Vec::new();
        for child_session_id in self.stacked_descendant_session_ids(parent_session_id) {
            if let Err(error) = self
                .cancel_single_session(
                    services,
                    child_session_id.as_str(),
                    CancellationCapability::StackedDescendant,
                )
                .await
            {
                warn!(
                    parent_session_id = parent_session_id,
                    child_session_id = %child_session_id,
                    error = %error,
                    "failed to cancel stacked child session after parent cancellation"
                );
                cancellation_failures.push(format!("{child_session_id}: {error}"));
            }
        }

        if cancellation_failures.is_empty() {
            return Ok(());
        }

        Err(SessionError::Workflow(format!(
            "Failed to cancel stacked descendants: {}",
            cancellation_failures.join("; ")
        )))
    }

    /// Returns loaded descendant ids in parent-before-child order.
    fn stacked_descendant_session_ids(&self, parent_session_id: &str) -> Vec<SessionId> {
        let mut ancestor_session_ids = vec![SessionId::from(parent_session_id)];
        let mut descendant_session_ids = Vec::new();

        loop {
            let next_descendants = self
                .state
                .sessions
                .iter()
                .filter(|session| {
                    session.parent_session_id.as_ref().is_some_and(|parent_id| {
                        ancestor_session_ids.contains(parent_id)
                            && !descendant_session_ids.contains(&session.id)
                    })
                })
                .map(|session| session.id.clone())
                .collect::<Vec<_>>();
            if next_descendants.is_empty() {
                break;
            }

            ancestor_session_ids.extend(next_descendants.iter().cloned());
            descendant_session_ids.extend(next_descendants);
        }

        descendant_session_ids
    }

    /// Defers terminal cancellation cleanup so foreground key handling returns
    /// after persisted status changes instead of waiting on git and filesystem
    /// removal.
    ///
    /// The background task resolves the shared repository root only when a
    /// worktree exists, removes git worktree/branch resources, then clears the
    /// session-scoped prompt temp directory. Cleanup remains best-effort and
    /// reports failures through debug-visible warnings.
    pub(super) fn spawn_canceled_session_cleanup(
        services: &AppServices,
        folder: PathBuf,
        branch_name: String,
        has_worktree: bool,
        session_id: String,
    ) {
        let fs_client = services.fs_client();
        let git_client = services.git_client();
        let cleanup_task_handle = tokio::spawn(async move {
            if has_worktree {
                let repo_root = git_client.main_repo_root(folder.clone()).await.ok();
                let cleanup_errors = Self::cleanup_session_worktree_resources(
                    Arc::clone(&fs_client),
                    Arc::clone(&git_client),
                    folder,
                    branch_name,
                    repo_root,
                    true,
                )
                .await;
                Self::warn_cleanup_errors(&session_id, &cleanup_errors);
            }

            Self::cleanup_session_temp_directory(fs_client, &session_id).await;
        });
        services.track_cleanup_task(cleanup_task_handle);
    }

    /// Requests cancellation for unfinished operations, clears queued work,
    /// and signals any active agent turn for a terminally canceled session.
    async fn signal_session_cancellation(
        services: &AppServices,
        handles: &SessionHandles,
        session_id: &str,
    ) {
        if let Err(error) = services
            .db()
            .operations()
            .request_cancel_for_session_operations(session_id)
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to request cancellation for running session operations"
            );
        }

        if let Ok(mut queued_messages) = handles.queued_messages.lock() {
            queued_messages.clear();
        }
        handles.clear_queued_actions();

        match handles.cancel_token.lock() {
            Ok(cancel_token) => cancel_token.cancel(),
            Err(error) => {
                warn!(
                    session_id = session_id,
                    error = %error,
                    "failed to lock running session cancel token"
                );
            }
        }
    }

    /// Removes git and filesystem resources for one session worktree.
    ///
    /// This best-effort helper is shared by terminal-state cleanup and session
    /// deletion so both paths remove the linked worktree checkout, delete the
    /// session branch when the shared repository root is known, and finally
    /// remove the directory from disk. Any cleanup failures are returned as
    /// human-readable messages so callers can surface them when needed.
    #[must_use]
    pub(super) async fn cleanup_session_worktree_resources(
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn git::GitClient>,
        folder: PathBuf,
        branch_name: String,
        repo_root: Option<PathBuf>,
        remove_git_resources: bool,
    ) -> Vec<String> {
        let mut cleanup_errors = Vec::new();

        if remove_git_resources {
            if let Err(error) = git_client.remove_worktree(folder.clone()).await {
                cleanup_errors.push(format!("failed to remove worktree: {error}"));
            }

            if let Some(repo_root) = repo_root
                && let Err(error) = git_client.delete_branch(repo_root, branch_name).await
            {
                cleanup_errors.push(format!("failed to delete branch: {error}"));
            }
        }

        if let Err(error) = fs_client.remove_dir_all(folder).await {
            cleanup_errors.push(format!("failed to remove worktree directory: {error}"));
        }

        cleanup_errors
    }

    /// Emits debug-visible warnings for best-effort cleanup failures.
    fn warn_cleanup_errors(session_id: &str, cleanup_errors: &[String]) {
        for cleanup_error in cleanup_errors {
            warn!(session_id = session_id, "{cleanup_error}");
        }
    }

    /// Removes Agentty-managed prompt attachment files and prunes their
    /// now-empty image directory when possible.
    pub(crate) async fn cleanup_prompt_attachment_paths(
        fs_client: Arc<dyn FsClient>,
        attachment_paths: Vec<PathBuf>,
    ) {
        Self::cleanup_prompt_attachment_paths_in_root(
            fs_client,
            &prompt_attachment_tmp_root(),
            attachment_paths,
        )
        .await;
    }

    /// Removes Agentty-managed prompt attachment files inside one explicit tmp
    /// root and prunes their shared image directory only when it is empty.
    ///
    /// The image directory is shared per session, so other queued prompts or
    /// the active composer may still reference sibling files there. Pruning
    /// uses [`FsClient::remove_dir`] (empty-only) and silently tolerates the
    /// `DirectoryNotEmpty` and `NotFound` cases so retracting one prompt
    /// never deletes another prompt's attachments.
    async fn cleanup_prompt_attachment_paths_in_root(
        fs_client: Arc<dyn FsClient>,
        managed_tmp_root: &Path,
        attachment_paths: Vec<PathBuf>,
    ) {
        if attachment_paths.is_empty() {
            return;
        }

        let image_directory =
            managed_prompt_attachment_directory(&attachment_paths, managed_tmp_root);

        for attachment_path in attachment_paths {
            if is_managed_prompt_attachment_path(&attachment_path, managed_tmp_root)
                && let Err(error) = fs_client.remove_file(attachment_path).await
            {
                warn!(
                    error = %error,
                    "failed to remove managed prompt attachment file"
                );
            }
        }

        if let Some(image_directory) = image_directory
            && let Err(error) = fs_client.remove_dir(image_directory).await
        {
            let FsError::Io(io_error) = &error;
            if !matches!(
                io_error.kind(),
                std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
            ) {
                warn!(
                    error = %error,
                    "failed to remove managed prompt attachment directory"
                );
            }
        }
    }

    /// Removes the session-scoped temp directory used for pasted prompt
    /// images.
    async fn cleanup_session_temp_directory(fs_client: Arc<dyn FsClient>, session_id: &str) {
        if let Err(error) = fs_client
            .remove_dir_all(session_prompt_temp_directory(session_id))
            .await
        {
            warn!(
                session_id = session_id,
                error = %error,
                "failed to remove session prompt temp directory"
            );
        }
    }
}

/// Replaces only the first occurrence of `needle` in `haystack`.
///
/// If `needle` is absent, the original string is returned unchanged.
fn replace_first(haystack: &str, needle: &str, replacement: &str) -> String {
    let Some(match_index) = haystack.find(needle) else {
        return haystack.to_string();
    };

    let mut replaced = String::with_capacity(
        haystack
            .len()
            .saturating_sub(needle.len())
            .saturating_add(replacement.len()),
    );
    replaced.push_str(&haystack[..match_index]);
    replaced.push_str(replacement);
    replaced.push_str(&haystack[match_index + needle.len()..]);

    replaced
}

/// Returns the session-scoped temp directory used for pasted prompt images.
fn session_prompt_temp_directory(session_id: &str) -> PathBuf {
    agentty_home().join("tmp").join(session_id)
}

/// Returns the Agentty-owned tmp root used for pasted prompt attachments.
fn prompt_attachment_tmp_root() -> PathBuf {
    agentty_home().join("tmp")
}

/// Returns the shared managed image directory for the given attachment paths
/// when every path stays within the Agentty temp root.
fn managed_prompt_attachment_directory(
    attachment_paths: &[PathBuf],
    managed_tmp_root: &Path,
) -> Option<PathBuf> {
    let image_directory = attachment_paths.first()?.parent()?.to_path_buf();
    if !is_managed_prompt_attachment_directory(&image_directory, managed_tmp_root) {
        return None;
    }

    attachment_paths
        .iter()
        .all(|attachment_path| {
            attachment_path.parent() == Some(image_directory.as_path())
                && is_managed_prompt_attachment_path(attachment_path, managed_tmp_root)
        })
        .then_some(image_directory)
}

/// Returns whether one attachment path is owned by Agentty under the managed
/// prompt-image tmp root.
fn is_managed_prompt_attachment_path(path: &Path, managed_tmp_root: &Path) -> bool {
    path.parent().is_some_and(|parent| {
        is_managed_prompt_attachment_directory(parent, managed_tmp_root)
            && path.starts_with(managed_tmp_root)
    })
}

/// Returns whether one directory is an Agentty-managed prompt-image directory.
fn is_managed_prompt_attachment_directory(path: &Path, managed_tmp_root: &Path) -> bool {
    path.starts_with(managed_tmp_root) && path.ends_with("images")
}

#[cfg(test)]
#[path = "lifecycle_test_support_test.rs"]
mod test_support;

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
