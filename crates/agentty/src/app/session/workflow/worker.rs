//! Per-session async worker orchestration for serialized command execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ag_agent as agent;
use ag_agent::{
    AgentChannel, AgentError, AgentRequestKind, OneShotClient, TurnContinuation, TurnEvent,
    TurnRequest, TurnResult, create_agent_channel,
};
use ag_forge as forge;
use ag_git::GitClient;
use ag_protocol::AgentResponse;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::merge::{
    ExistingSessionRebaseAssistClient, RebaseAssistFuture, RebaseAssistMode, RebaseCommandInput,
};
use super::task::SessionTranscriptMessageAppend;
use super::{SessionTaskService, isolation, session_folder, turn};
use crate::app::branch_publish::{
    BranchPublishTaskContext, BranchPublishTaskSession, review_request_from_publish_result,
    run_branch_publish_action,
};
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError, unix_timestamp_from_system_time};
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::domain::agent::AgentSelection;
use crate::domain::session::{
    PublishBranchAction, QueuedMessage, ReviewRequest, SessionId, SessionStats, Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{AppRepositories, OperationRepository, SessionOperationRow};
use crate::infra::fs::FsClient;
use crate::infra::personality::PersonalityCatalogClient;

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";
const CREATE_REVIEW_REQUEST_OPERATION_KIND: &str = "create_review_request";
const REBASE_OPERATION_KIND: &str = "rebase";
const SKIPPED_CREATE_REVIEW_REQUEST_REASON: &str =
    "Review-request creation was canceled or already finished before execution";

/// Shared completion slot used by programmatic review-request callers.
///
/// The runtime retains a clone while handing the command to the worker so an
/// enqueue failure can still answer the caller instead of dropping the
/// response channel with an ambiguous runtime-unavailable error.
pub(super) type ReviewRequestResponse =
    Arc<Mutex<Option<oneshot::Sender<Result<ReviewRequest, ag_session::SessionError>>>>>;

/// Per-turn data captured at enqueue time that travels alongside the channel
/// turn but is consumed only after turn completion.
///
/// Groups per-turn state that would otherwise be threaded as individual
/// parameters through the `run_channel_turn` → `apply_turn_result` →
/// `apply_successful_turn_result` call chain. Future per-turn data (retry
/// policies, model overrides, etc.) should be added here instead of widening
/// every intermediate signature.
pub(super) struct TurnMetadata {
    /// Published-upstream reference captured when the turn was queued,
    /// consumed after turn completion by the auto-push workflow.
    pub(super) published_upstream_ref: Option<String>,
    /// Forge review-thread identifiers explicitly targeted by this turn.
    pub(super) review_comment_thread_ids: Vec<String>,
    /// Agent provider and model selected for the session when the turn was
    /// queued.
    pub(super) session_agent: AgentSelection,
}

/// Single command variant serialized per session worker.
///
/// Replaces the previous four-variant enum (`Reply`, `ReplyAppServer`,
/// `StartPrompt`, `StartPromptAppServer`) with a single provider-agnostic
/// variant. The underlying channel adapter handles transport-specific details.
pub(super) enum SessionCommand {
    /// Publishes the session branch and creates or refreshes its forge review
    /// request after earlier work on this worker has completed.
    CreateReviewRequest {
        /// Session snapshot captured when the action was accepted.
        branch_publish_session: BranchPublishTaskSession,
        /// Persisted operation identifier.
        operation_id: String,
        /// Optional user-selected remote branch name.
        remote_branch_name: Option<String>,
        /// Optional programmatic caller waiting for the resulting review
        /// request.
        response: Option<ReviewRequestResponse>,
    },
    /// Runs the session branch rebase workflow through this worker so
    /// conflict-resolution prompts reuse the active provider conversation.
    Rebase {
        /// Stored base branch used to resolve the concrete rebase target.
        base_branch: String,
        /// Persisted operation identifier.
        operation_id: String,
    },
    /// Executes one agent turn with the given request kind and prompt.
    Run {
        /// Persisted operation identifier.
        operation_id: String,
        /// Whether this is a first-message start or a follow-up resume.
        request_kind: AgentRequestKind,
        /// Replayable transcript text captured when this turn was queued.
        replay_transcript: Option<String>,
        /// Structured user prompt payload.
        prompt: TurnPrompt,
        /// Per-turn metadata consumed during and after turn execution.
        turn_metadata: TurnMetadata,
    },
}

impl SessionCommand {
    /// Identifies the stable handoff whose saved payload belongs to workspace
    /// preparation until execution starts.
    pub(super) fn is_preparation_prompt(&self, session_id: &str) -> bool {
        self.operation_id() == format!("workspace:{session_id}")
    }

    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::CreateReviewRequest { operation_id, .. }
            | Self::Rebase { operation_id, .. }
            | Self::Run { operation_id, .. } => operation_id,
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::CreateReviewRequest { .. } => CREATE_REVIEW_REQUEST_OPERATION_KIND,
            Self::Rebase { .. } => REBASE_OPERATION_KIND,
            Self::Run {
                request_kind: AgentRequestKind::SessionStart,
                ..
            } => "start_prompt",
            Self::Run {
                request_kind: AgentRequestKind::SessionResume,
                ..
            } => "reply",
            Self::Run {
                request_kind: AgentRequestKind::FocusedReview,
                ..
            } => "focused_review",
            Self::Run {
                request_kind: AgentRequestKind::UtilityPrompt,
                ..
            } => "utility_prompt",
            Self::Run {
                request_kind: AgentRequestKind::AccountRead,
                ..
            } => "account_read",
        }
    }
}

/// Worker command paired with its shared queue order when it was submitted
/// behind active work.
struct ScheduledSessionCommand {
    command: SessionCommand,
    /// Reserves stack branch work until this command is dropped or finishes.
    preparation_reservation: Option<Arc<()>>,
    queued_order: Option<u64>,
    /// First turns wait until the foreground has persisted their operation
    /// and published their initial metadata.
    ready_rx: Option<oneshot::Receiver<()>>,
}

impl ScheduledSessionCommand {
    /// Returns whether this command can make a questioned session runnable.
    fn can_run_while_question(&self) -> bool {
        self.queued_order.is_none() || matches!(self.command, SessionCommand::Run { .. })
    }

    /// Wraps a command that is ready to run without joining the visible queue.
    fn immediate(command: SessionCommand) -> Self {
        Self {
            command,
            preparation_reservation: None,
            queued_order: None,
            ready_rx: None,
        }
    }

    /// Wraps a command queued at the supplied shared submission order.
    fn queued(command: SessionCommand, queued_order: u64) -> Self {
        Self {
            command,
            preparation_reservation: None,
            queued_order: Some(queued_order),
            ready_rx: None,
        }
    }
}

/// Sender and shared ordering source owned by one active session worker.
#[derive(Clone)]
struct SessionWorkerHandle {
    queued_work_sequence: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<ScheduledSessionCommand>,
    wakeup: Arc<Notify>,
}

impl SessionWorkerHandle {
    /// Reserves the next submission order shared with queued chat messages.
    fn next_queued_work_order(&self) -> u64 {
        self.queued_work_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Wakes the worker so buffered work is reconsidered after an external
    /// status transition.
    fn wake(&self) {
        self.wakeup.notify_one();
    }
}

/// Next unit selected from the combined action and chat queues.
enum ScheduledSessionWork {
    Command(Box<ScheduledSessionCommand>),
    Message(QueuedMessage),
}

/// Returns whether the session has a queued or running rebase operation.
pub(super) fn has_unfinished_rebase_operation(
    operations: &[SessionOperationRow],
    session_id: &str,
) -> bool {
    operations.iter().any(|operation| {
        operation.session_id == session_id && operation.kind == REBASE_OPERATION_KIND
    })
}

/// Returns whether the session has a queued or running branch operation that
/// must run before automatic post-turn publishing.
pub(super) fn has_unfinished_branch_operation(
    operations: &[SessionOperationRow],
    session_id: &str,
) -> bool {
    operations.iter().any(|operation| {
        operation.session_id == session_id
            && matches!(
                operation.kind.as_str(),
                CREATE_REVIEW_REQUEST_OPERATION_KIND | REBASE_OPERATION_KIND
            )
    })
}

/// Shared state threaded through all worker turn executions.
pub(super) struct SessionWorkerContext {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Serializes post-turn publish ownership with queued branch operations.
    pub(super) branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Per-turn cancellation token shared with the UI through
    /// [`SessionHandles`]. The worker swaps in a fresh token at the start
    /// of each turn; the UI calls `cancel()` on the current token to
    /// interrupt a running turn.
    pub(super) cancel_token: Arc<Mutex<CancellationToken>>,
    /// Provider-agnostic agent channel for this session's worker.
    pub(super) channel: Arc<dyn AgentChannel>,
    /// Runtime accounting root; only CLI transports may use it for signaling.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) db: AppRepositories,
    pub(super) folder: PathBuf,
    pub(super) fs_client: Arc<dyn FsClient>,
    pub(super) git_client: Arc<dyn GitClient>,
    /// Workspace-only personality discovery used immediately before turns.
    pub(super) personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    /// In-memory queue of prompts staged while the session is `InProgress`.
    ///
    /// Shared with [`SessionHandles::queued_messages`]. The worker drains
    /// this queue between turns; the lifecycle pushes new entries when a
    /// user submits a chat message during a running turn.
    pub(super) queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Agent provider and model selected for this session.
    pub(super) session_agent: AgentSelection,
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    pub(super) status: Arc<Mutex<Status>>,
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

impl SessionWorkerContext {
    /// Returns the submission order of the next queued chat prompt.
    fn next_queued_message_order(&self) -> Option<u64> {
        self.queued_messages
            .lock()
            .ok()
            .and_then(|guard| guard.front().map(QueuedMessage::order))
    }

    /// Pops the next queued chat message for dispatch as a follow-up turn.
    fn pop_queued_message(&self) -> Option<QueuedMessage> {
        // Sync critical section (single pop, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        self.queued_messages
            .lock()
            .ok()
            .and_then(|mut guard| guard.pop_front())
    }

    /// Removes every queued prompt without dispatching it.
    fn clear_queued_messages(&self) {
        // Sync critical section (single clear, no `.await`);
        // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
        // Selection".
        if let Ok(mut guard) = self.queued_messages.lock() {
            guard.clear();
        }
    }

    /// Loads the latest published upstream reference before running a queued
    /// follow-up turn.
    ///
    /// Queued prompts are created while another turn is still running, so
    /// their auto-push metadata is resolved at drain time from persistence
    /// instead of being captured when the user submits the queued prompt.
    async fn load_published_upstream_ref(&self) -> Option<String> {
        self.db
            .sessions()
            .load_session_published_upstream_ref(&self.session_id)
            .await
            .ok()
            .flatten()
    }

    /// Returns the current shared session status.
    fn current_status(&self) -> Status {
        // Sync critical section (single read, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        self.status.lock().map_or(Status::Review, |guard| *guard)
    }
}

/// Existing-session rebase assistance backed by the worker's active channel.
#[derive(Clone)]
struct SessionWorkerRebaseAssistClient {
    /// Reducer event sender used for transient progress and output updates.
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Per-turn cancellation token shared with the UI.
    cancel_token: Arc<Mutex<CancellationToken>>,
    /// Provider channel already associated with this session.
    channel: Arc<dyn AgentChannel>,
    /// Runtime accounting root; app-server cancellation uses channel shutdown.
    child_pid: Arc<Mutex<Option<u32>>>,
    /// Repository bundle used for conversation and usage persistence.
    db: AppRepositories,
    /// Session worktree folder where the utility prompt runs.
    folder: PathBuf,
    /// Main repository checkout that must remain read-only during assist
    /// turns, or `None` when the shared repository is bare and has no main
    /// working checkout.
    main_checkout_root: Option<PathBuf>,
    /// Agent provider and model used for the rebase-assist utility prompt.
    session_agent: AgentSelection,
    /// Session identifier whose provider conversation is reused.
    session_id: SessionId,
    /// Per-app session update versions for targeted refresh events.
    session_update_versions: SessionUpdateVersionMap,
    /// Shared typed transcript snapshot mirrored to the render layer.
    transcript: Arc<Mutex<SessionTranscript>>,
}

impl SessionWorkerRebaseAssistClient {
    /// Clones the worker fields needed to run a rebase-assist utility turn.
    fn from_context(context: &SessionWorkerContext, main_checkout_root: Option<PathBuf>) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            cancel_token: Arc::clone(&context.cancel_token),
            channel: Arc::clone(&context.channel),
            child_pid: Arc::clone(&context.child_pid),
            db: context.db.clone(),
            folder: context.folder.clone(),
            main_checkout_root,
            session_update_versions: context.session_update_versions.clone(),
            session_id: context.session_id.clone(),
            session_agent: context.session_agent,
            transcript: Arc::clone(&context.transcript),
        }
    }

    /// Runs one utility prompt through the current session channel.
    ///
    /// # Errors
    /// Returns an error when the provider turn fails or conversation metadata
    /// cannot be persisted.
    async fn run_assist_turn(&self, prompt: String) -> Result<(), SessionError> {
        let turn_cancel_token = self.fresh_turn_cancel_token()?;
        let reasoning_level = turn::load_session_reasoning_level(&self.db, &self.session_id).await;
        let speed_mode = turn::load_session_speed_mode(&self.db, &self.session_id).await;
        let provider_conversation_id = self
            .db
            .sessions()
            .get_session_provider_conversation_id(&self.session_id)
            .await
            .ok()
            .flatten();
        let persisted_instruction_conversation_id = self
            .db
            .sessions()
            .get_session_instruction_conversation_id(&self.session_id)
            .await
            .ok()
            .flatten();
        let req = TurnRequest {
            continuation: TurnContinuation::provider(
                Some(turn::live_transcript_source(&self.transcript)),
                persisted_instruction_conversation_id,
                provider_conversation_id,
                None,
            ),
            folder: self.folder.clone(),
            main_checkout_root: self.main_checkout_root.clone(),
            model: self.session_agent.model().provider_model_str().to_string(),
            permission_mode: agent::PermissionMode::AutoEdit,
            personality: ag_agent::PersonalityPrompt::default(),
            prompt: TurnPrompt::from_agent_data(prompt),
            reasoning_level,
            request_kind: AgentRequestKind::UtilityPrompt,
            response_style: agent::ResponseStyle::default(),
            speed_mode,
        };
        let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let consumer = tokio::spawn(turn::consume_turn_events(
            event_rx,
            self.app_event_tx.clone(),
            self.session_id.clone(),
            Arc::clone(&self.child_pid),
        ));

        let turn_result = self
            .run_turn_with_cancellation(turn_cancel_token, req, event_tx)
            .await;
        let _ = consumer.await;
        let turn_result = turn_result.map_err(turn::session_error_from_agent_error)?;

        self.append_assist_answer(&turn_result.assistant_message)
            .await;
        self.persist_assist_turn_metadata(&turn_result).await?;

        Ok(())
    }

    /// Replaces the shared cancellation token for one rebase-assist turn.
    fn fresh_turn_cancel_token(&self) -> Result<CancellationToken, SessionError> {
        // Sync critical section (assignment + clone, no `.await`);
        // `std::sync::Mutex` is the correct choice per CLAUDE.md
        // §"Mutex Selection".
        let mut guard = self
            .cancel_token
            .lock()
            .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))?;
        *guard = CancellationToken::new();

        Ok(guard.clone())
    }

    /// Runs the provider turn while honoring the shared cancellation token.
    async fn run_turn_with_cancellation(
        &self,
        cancel_token: CancellationToken,
        req: TurnRequest,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
    ) -> Result<TurnResult, AgentError> {
        if cancel_token.is_cancelled() {
            turn::terminate_child_process(&self.child_pid, self.session_agent.kind());
            let _ = self
                .channel
                .shutdown_session(self.session_id.to_string())
                .await;

            return Err(AgentError::InterruptedByUser(
                "[Stopped] Session interrupted by user.".to_string(),
            ));
        }

        let turn_future = self
            .channel
            .run_turn(self.session_id.to_string(), req, event_tx);
        tokio::pin!(turn_future);

        tokio::select! {
            result = &mut turn_future => result,
            () = cancel_token.cancelled() => {
                turn::terminate_child_process(&self.child_pid, self.session_agent.kind());
                let _ = self.channel.shutdown_session(self.session_id.to_string()).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), &mut turn_future).await;

                Err(AgentError::InterruptedByUser(
                    "[Stopped] Session interrupted by user.".to_string(),
                ))
            }
        }
    }

    /// Appends the utility prompt answer to the session transcript.
    async fn append_assist_answer(&self, assistant_message: &AgentResponse) {
        let answer_text = assistant_message.to_answer_display_text();
        if answer_text.trim().is_empty() {
            return;
        }

        SessionTaskService::append_session_transcript_message(
            &self.transcript,
            &self.db,
            &self.app_event_tx,
            &self.session_update_versions,
            &self.session_id,
            SessionTranscriptMessageAppend {
                kind: SessionMessageKind::AssistantAnswer,
                raw_content: &answer_text,
            },
        )
        .await;
    }

    /// Persists token usage and updated provider conversation identifiers.
    ///
    /// # Errors
    /// Returns an error when conversation identifier persistence fails.
    async fn persist_assist_turn_metadata(
        &self,
        turn_result: &TurnResult,
    ) -> Result<(), SessionError> {
        let token_usage_delta = SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: agent::SessionDiffState::Unknown,
            input_tokens: turn_result.input_tokens,
            output_tokens: turn_result.output_tokens,
        };
        if let Err(error) = self
            .db
            .sessions()
            .update_session_stats(&self.session_id, &token_usage_delta)
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                error = %error,
                "failed to persist session stats after rebase-assist turn"
            );
        }
        if let Err(error) = self
            .db
            .usage()
            .upsert_session_usage(
                &self.session_id,
                self.session_agent.model().as_str(),
                &token_usage_delta,
            )
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                model = %self.session_agent.model().as_str(),
                error = %error,
                "failed to persist session usage after rebase-assist turn"
            );
        }
        let Some(provider_conversation_id) = turn_result.provider_conversation_id.clone() else {
            return Ok(());
        };

        self.db
            .sessions()
            .update_session_provider_conversation_id(
                &self.session_id,
                Some(provider_conversation_id.clone()),
            )
            .await?;
        if agent::transport_mode(self.session_agent.kind()).uses_app_server() {
            self.db
                .sessions()
                .update_session_instruction_conversation_id(
                    &self.session_id,
                    agent::normalize_instruction_conversation_id(Some(&provider_conversation_id)),
                )
                .await?;
        }

        Ok(())
    }
}

impl ExistingSessionRebaseAssistClient for SessionWorkerRebaseAssistClient {
    fn resolve_rebase_conflicts(
        &self,
        prompt: String,
    ) -> RebaseAssistFuture<Result<(), SessionError>> {
        let assist_client = self.clone();

        Box::pin(async move { assist_client.run_assist_turn(prompt).await })
    }
}

/// Runtime snapshot required to create or reuse one session worker.
pub(super) struct SessionWorkerRuntime {
    branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
    cancel_token: Arc<Mutex<CancellationToken>>,
    child_pid: Arc<Mutex<Option<u32>>>,
    folder: PathBuf,
    personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    queued_work_sequence: Arc<AtomicU64>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Agent provider and model selected for this session.
    session_agent: AgentSelection,
    session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    session_update_versions: SessionUpdateVersionMap,
    status: Arc<Mutex<Status>>,
    transcript: Arc<Mutex<SessionTranscript>>,
}

/// Owns per-session worker queue senders and test channel overrides.
pub(crate) struct SessionWorkerService {
    /// Channels pre-registered for specific session workers in tests.
    ///
    /// Tests populate this map before enqueueing a command so that
    /// `ensure_session_worker` uses the injected channel instead of the
    /// default factory, enabling deterministic command execution without
    /// spawning real provider processes.
    pub(in crate::app::session) test_agent_channels: HashMap<SessionId, Arc<dyn AgentChannel>>,
    preparation_reservations: HashMap<SessionId, Weak<()>>,
    workers: HashMap<SessionId, SessionWorkerHandle>,
}

impl SessionWorkerService {
    /// Creates an empty worker service with no active session workers.
    pub(in crate::app::session) fn new() -> Self {
        Self {
            preparation_reservations: HashMap::new(),
            test_agent_channels: HashMap::new(),
            workers: HashMap::new(),
        }
    }

    /// Returns whether a saved first turn still owns queued or running work.
    pub(super) fn has_preparation_reservation(&self, session_id: &str) -> bool {
        self.preparation_reservations
            .get(session_id)
            .is_some_and(|reservation| reservation.strong_count() > 0)
    }

    /// Claims branch work during the serialized foreground enqueue. The
    /// command owns the claim, so failed delivery, an abandoned gate, and
    /// worker rejection all release it without consuming the saved prompt.
    fn reserve_preparation_command(
        &mut self,
        session_id: &SessionId,
        command: &mut ScheduledSessionCommand,
    ) {
        if !command.command.is_preparation_prompt(session_id) {
            return;
        }
        self.preparation_reservations
            .retain(|_, reservation| reservation.strong_count() > 0);
        let reservation = self
            .preparation_reservations
            .entry(session_id.clone())
            .or_default();
        let claim = reservation.upgrade().unwrap_or_else(|| Arc::new(()));
        *reservation = Arc::downgrade(&claim);
        command.preparation_reservation = Some(claim);
    }

    /// Marks unfinished operations from previous process runs as failed and
    /// closes any open active-work timing window at `timestamp_seconds`.
    ///
    /// # Errors
    /// Returns an error when loading operations, cleaning interrupted rebases,
    /// reconciling session status, or recording interrupted operations fails.
    pub(super) async fn fail_unfinished_operations_from_previous_run_at(
        db: &AppRepositories,
        base_path: &Path,
        git_client: Arc<dyn GitClient>,
        timestamp_seconds: i64,
    ) -> Result<(), SessionError> {
        let unfinished_operations = db.operations().load_unfinished_session_operations().await?;
        Self::abort_rebase_operations_from_previous_run(
            base_path,
            git_client.as_ref(),
            &unfinished_operations,
        )
        .await?;

        let interrupted_session_ids: HashSet<&str> = unfinished_operations
            .iter()
            .map(|operation| operation.session_id.as_str())
            .collect();

        for session_id in interrupted_session_ids {
            let unstarted_first_prompt = unfinished_operations
                .iter()
                .filter(|operation| operation.session_id == session_id)
                .all(|operation| {
                    operation.id == format!("workspace:{session_id}")
                        && operation.kind == "start_prompt"
                        && operation.started_at.is_none()
                })
                && db
                    .sessions()
                    .load_session_preparation(session_id)
                    .await?
                    .is_some_and(|preparation| preparation.prompt.is_some());
            let recovered_status = if unstarted_first_prompt {
                Status::Draft
            } else {
                Status::Review
            };
            db.sessions()
                .update_session_status_with_timing_at(
                    session_id,
                    &recovered_status.to_string(),
                    timestamp_seconds,
                )
                .await?;
        }

        db.operations()
            .fail_unfinished_session_operations(RESTART_FAILURE_REASON)
            .await?;

        Ok(())
    }

    /// Aborts stale git rebase state left by interrupted worker operations.
    ///
    /// Only worker-backed rebase operations are handled here because merge
    /// tasks are not yet persisted in `session_operation`. Missing worktrees
    /// need no Git cleanup; their operations still undergo durable recovery.
    ///
    /// # Errors
    /// Returns an error when Git cannot inspect or abort interrupted rebase
    /// state.
    async fn abort_rebase_operations_from_previous_run(
        base_path: &Path,
        git_client: &dyn GitClient,
        unfinished_operations: &[SessionOperationRow],
    ) -> Result<(), SessionError> {
        let mut rebase_session_ids = unfinished_operations
            .iter()
            .filter(|operation| operation.kind == REBASE_OPERATION_KIND)
            .map(|operation| operation.session_id.as_str())
            .collect::<Vec<_>>();
        rebase_session_ids.sort_unstable();
        rebase_session_ids.dedup();

        for session_id in rebase_session_ids {
            let folder = session_folder(base_path, session_id);
            let is_rebase_in_progress = match git_client.is_rebase_in_progress(folder.clone()).await
            {
                Ok(in_progress) => in_progress,
                Err(ag_git::GitError::RepositoryUnavailable { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
            if is_rebase_in_progress {
                git_client.abort_rebase(folder).await?;
            }
        }

        Ok(())
    }

    /// Persists and enqueues a command on the per-session worker queue.
    ///
    /// # Errors
    /// Returns an error if operation persistence fails or no worker is
    /// available.
    pub(super) async fn enqueue_session_command(
        &mut self,
        services: &AppServices,
        runtime: SessionWorkerRuntime,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        let session_id = runtime.session_id.clone();
        let worker = self.ensure_session_worker(services, &runtime);

        self.persist_and_send_command(services, &session_id, worker, command)
            .await
    }

    /// Claims and enqueues one command under its stable operation identifier.
    ///
    /// Returns `true` when this call enqueued the command and `false` when an
    /// earlier attempt already durably accepted it.
    ///
    /// # Errors
    /// Returns an error if operation persistence fails or no worker is
    /// available.
    pub(super) async fn enqueue_session_command_idempotently(
        &mut self,
        services: &AppServices,
        runtime: SessionWorkerRuntime,
        command: SessionCommand,
    ) -> Result<bool, SessionError> {
        let session_id = runtime.session_id.clone();
        let operation_id = command.operation_id().to_string();
        let claimed = services
            .db()
            .operations()
            .claim_session_operation(&operation_id, &session_id, command.kind())
            .await?;
        if !claimed {
            return Ok(false);
        }

        let worker = self.ensure_session_worker(services, &runtime);
        let mut scheduled_command = ScheduledSessionCommand::immediate(command);
        self.reserve_preparation_command(&session_id, &mut scheduled_command);
        self.send_persisted_command(
            services.db().operations(),
            &session_id,
            worker.sender,
            scheduled_command,
        )
        .await?;

        Ok(true)
    }

    /// Persists and enqueues a command only when the session already owns a
    /// worker sender.
    ///
    /// Running-session actions use this path so a stale `InProgress` status
    /// can never create a second worker that executes concurrently with the
    /// original turn.
    ///
    /// # Errors
    /// Returns an error without persisting the operation when the active
    /// worker sender is unavailable, or when persistence or delivery fails.
    pub(super) async fn enqueue_existing_session_command(
        &mut self,
        services: &AppServices,
        session_id: &SessionId,
        command: SessionCommand,
    ) -> Result<u64, SessionError> {
        let worker = self.workers.get(session_id).cloned().ok_or_else(|| {
            SessionError::Workflow(
                "Cannot queue session action because the active session worker is unavailable"
                    .to_string(),
            )
        })?;
        let operation_id = command.operation_id().to_string();
        services
            .db()
            .operations()
            .insert_session_operation(&operation_id, session_id, command.kind())
            .await?;
        let queued_order = worker.next_queued_work_order();

        self.send_persisted_command(
            services.db().operations(),
            session_id,
            worker.sender,
            ScheduledSessionCommand::queued(command, queued_order),
        )
        .await?;

        Ok(queued_order)
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.workers.remove(session_id);
    }

    /// Wakes an existing session worker after an external state transition.
    pub(super) fn wake_session_worker(&self, session_id: &str) {
        if let Some(worker) = self.workers.get(session_id) {
            worker.wake();
        }
    }

    /// Queues a first turn behind a foreground completion gate, then persists
    /// its operation. Dropping the returned sender skips the turn; sending
    /// `()` releases it after the foreground metadata is ready.
    ///
    /// # Errors
    /// Returns an error before accepting the turn when delivery or operation
    /// persistence fails. Failed delivery leaves no operation to recover.
    async fn enqueue_gated_session_command(
        &mut self,
        services: &AppServices,
        runtime: SessionWorkerRuntime,
        command: SessionCommand,
    ) -> Result<oneshot::Sender<()>, SessionError> {
        let operation_id = command.operation_id().to_string();
        let operation_kind = command.kind();
        let worker = self.ensure_session_worker(services, &runtime);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut scheduled_command = ScheduledSessionCommand::immediate(command);
        scheduled_command.ready_rx = Some(ready_rx);
        self.reserve_preparation_command(&runtime.session_id, &mut scheduled_command);
        if worker.sender.send(scheduled_command).is_err() {
            self.workers.remove(&runtime.session_id);

            return Err(SessionError::Workflow(
                "Session worker is not available".to_string(),
            ));
        }
        services
            .db()
            .operations()
            .insert_session_operation(&operation_id, &runtime.session_id, operation_kind)
            .await?;

        Ok(ready_tx)
    }

    /// Returns an existing session worker sender or creates one lazily.
    fn ensure_session_worker(
        &mut self,
        services: &AppServices,
        runtime: &SessionWorkerRuntime,
    ) -> SessionWorkerHandle {
        if let Some(worker) = self.workers.get(&runtime.session_id) {
            return worker.clone();
        }

        // When a pre-registered channel exists, reuse it; otherwise fall back
        // to the production channel factory.
        let channel = self
            .test_agent_channels
            .remove(&runtime.session_id)
            .unwrap_or_else(|| {
                create_agent_channel(
                    runtime.session_agent.kind(),
                    services.app_server_client_override(),
                )
            });

        let context = SessionWorkerContext {
            app_event_tx: services.event_sender(),
            branch_operation_lock: Arc::clone(&runtime.branch_operation_lock),
            cancel_token: Arc::clone(&runtime.cancel_token),
            channel,
            child_pid: Arc::clone(&runtime.child_pid),
            clock: services.clock(),
            db: services.db().clone(),
            folder: runtime.folder.clone(),
            fs_client: services.fs_client(),
            git_client: services.git_client(),
            personality_catalog_client: Arc::clone(&runtime.personality_catalog_client),
            queued_messages: Arc::clone(&runtime.queued_messages),
            review_request_client: Arc::clone(&runtime.review_request_client),
            session_update_versions: Arc::clone(&runtime.session_update_versions),
            session_id: runtime.session_id.clone(),
            session_agent: runtime.session_agent,
            status: Arc::clone(&runtime.status),
            transcript: Arc::clone(&runtime.transcript),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let wakeup = Arc::new(Notify::new());
        let worker = SessionWorkerHandle {
            queued_work_sequence: Arc::clone(&runtime.queued_work_sequence),
            sender,
            wakeup: Arc::clone(&wakeup),
        };
        self.workers
            .insert(runtime.session_id.clone(), worker.clone());
        Self::spawn_session_worker(context, services.one_shot_client(), wakeup, receiver);

        worker
    }

    /// Persists one operation and sends its command to the selected worker.
    async fn persist_and_send_command(
        &mut self,
        services: &AppServices,
        session_id: &SessionId,
        worker: SessionWorkerHandle,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        let operation_id = command.operation_id().to_string();
        services
            .db()
            .operations()
            .insert_session_operation(&operation_id, session_id, command.kind())
            .await?;

        self.send_persisted_command(
            services.db().operations(),
            session_id,
            worker.sender,
            ScheduledSessionCommand::immediate(command),
        )
        .await
    }

    /// Sends one command whose operation row has already been persisted.
    async fn send_persisted_command(
        &mut self,
        operations: &dyn OperationRepository,
        session_id: &SessionId,
        sender: mpsc::UnboundedSender<ScheduledSessionCommand>,
        scheduled_command: ScheduledSessionCommand,
    ) -> Result<(), SessionError> {
        let operation_id = scheduled_command.command.operation_id().to_string();
        if sender.send(scheduled_command).is_err() {
            self.workers.remove(session_id);
            // Best-effort: operation tracking metadata is non-critical.
            let _ = operations
                .mark_session_operation_failed(&operation_id, "Session worker is not available")
                .await;

            return Err(SessionError::Workflow(
                "Session worker is not available".to_string(),
            ));
        }

        Ok(())
    }

    /// Spawns the background loop that executes queued session commands.
    ///
    /// Queued workflow actions and chat messages share one submission order,
    /// so the next displayed row is always the next work executed. Scheduling
    /// pauses while the session is in `Question` state, except for an
    /// immediate answer command that makes the session runnable again. A turn
    /// stopped by the user (`Ctrl+C`) clears queued chat so canceled work does
    /// not silently leak into the next session activity.
    fn spawn_session_worker(
        context: SessionWorkerContext,
        one_shot_client: Arc<dyn OneShotClient>,
        wakeup: Arc<Notify>,
        mut receiver: mpsc::UnboundedReceiver<ScheduledSessionCommand>,
    ) {
        tokio::spawn(async move {
            let mut pending_commands = VecDeque::new();
            loop {
                while let Ok(command) = receiver.try_recv() {
                    pending_commands.push_back(command);
                }

                let Some(work) = Self::next_scheduled_work(&context, &mut pending_commands) else {
                    tokio::select! {
                        command = receiver.recv() => {
                            let Some(command) = command else {
                                break;
                            };
                            pending_commands.push_back(command);
                        }
                        () = wakeup.notified() => {}
                    }

                    continue;
                };
                let result = match work {
                    ScheduledSessionWork::Command(mut command) => {
                        let _reservation = command.preparation_reservation.take();
                        if let Some(ready_rx) = command.ready_rx.take()
                            && ready_rx.await.is_err()
                        {
                            continue;
                        }
                        Self::process_session_command(&context, &one_shot_client, command.command)
                            .await
                    }
                    ScheduledSessionWork::Message(message) => {
                        Self::process_queued_message(&context, &one_shot_client, message).await
                    }
                };
                Self::clear_queued_messages_after_stop(&context, result.as_ref());
            }

            // Best-effort: session transport may already be torn down.
            let _ = context
                .channel
                .shutdown_session(context.session_id.to_string())
                .await;
            // Sync critical section (single assignment, no `.await`);
            // `std::sync::Mutex` is the correct choice per CLAUDE.md
            // §"Mutex Selection".
            if let Ok(mut guard) = context.child_pid.lock() {
                *guard = None;
            }
        });
    }

    /// Selects the oldest runnable work across workflow and chat queues.
    fn next_scheduled_work(
        context: &SessionWorkerContext,
        pending_commands: &mut VecDeque<ScheduledSessionCommand>,
    ) -> Option<ScheduledSessionWork> {
        if matches!(context.current_status(), Status::Question) {
            let runnable_index = pending_commands
                .iter()
                .position(ScheduledSessionCommand::can_run_while_question)?;

            return pending_commands
                .remove(runnable_index)
                .map(Box::new)
                .map(ScheduledSessionWork::Command);
        }
        if pending_commands
            .front()
            .is_some_and(|command| command.queued_order.is_none())
        {
            return pending_commands
                .pop_front()
                .map(Box::new)
                .map(ScheduledSessionWork::Command);
        }

        let command_order = pending_commands
            .front()
            .and_then(|command| command.queued_order);
        let message_order = context.next_queued_message_order();
        if command_order.is_some_and(|command_order| {
            message_order.is_none_or(|message_order| command_order <= message_order)
        }) {
            return pending_commands
                .pop_front()
                .map(Box::new)
                .map(ScheduledSessionWork::Command);
        }

        context
            .pop_queued_message()
            .map(ScheduledSessionWork::Message)
    }

    /// Clears pending chat messages when the work just stopped by user action.
    fn clear_queued_messages_after_stop(
        context: &SessionWorkerContext,
        result: Option<&Result<(), SessionError>>,
    ) {
        if matches!(result, Some(Err(SessionError::StoppedByUser(_)))) {
            context.clear_queued_messages();
            Self::emit_queue_session_updated(context);
        }
    }

    /// Executes one queued session command including its operation
    /// bookkeeping. Returns `None` when the command was skipped before
    /// execution (already finished or cancelled) and `Some(result)` when the
    /// turn ran.
    async fn process_session_command(
        context: &SessionWorkerContext,
        one_shot_client: &Arc<dyn OneShotClient>,
        command: SessionCommand,
    ) -> Option<Result<(), SessionError>> {
        let operation_id = command.operation_id().to_string();
        let should_skip = if Self::should_skip_worker_command(context, &operation_id).await {
            true
        } else if command.is_preparation_prompt(&context.session_id)
            && let SessionCommand::Run { prompt, .. } = &command
        {
            match Self::begin_preparation_prompt(context, &operation_id, &prompt.transcript_text())
                .await
            {
                Ok(started) => {
                    !started || Self::should_skip_worker_command(context, &operation_id).await
                }
                Err(error) => return Some(Err(error)),
            }
        } else {
            // Best-effort: operation tracking metadata is non-critical.
            let _ = context
                .db
                .operations()
                .mark_session_operation_running(&operation_id)
                .await;

            Self::should_skip_worker_command(context, &operation_id).await
        };
        if should_skip {
            Self::complete_skipped_session_command(context, &command);

            return None;
        }

        if command.is_preparation_prompt(&context.session_id)
            && let SessionCommand::Run {
                request_kind,
                prompt,
                ..
            } = &command
        {
            Self::append_preparation_prompt(context, request_kind, prompt);
        }

        if matches!(
            command,
            SessionCommand::Run {
                request_kind: AgentRequestKind::SessionStart | AgentRequestKind::SessionResume,
                ..
            }
        ) {
            let _ = context.app_event_tx.send(AppEvent::SessionTurnStarted {
                session_id: context.session_id.clone(),
            });
        }

        let result = Self::execute_session_command(context, one_shot_client, command).await;
        match &result {
            Ok(()) => {
                // Best-effort: operation tracking metadata is non-critical.
                let _ = context
                    .db
                    .operations()
                    .mark_session_operation_done(&operation_id)
                    .await;
            }
            Err(error) => {
                // Best-effort: operation tracking metadata is non-critical.
                let _ = context
                    .db
                    .operations()
                    .mark_session_operation_failed(&operation_id, &error.to_string())
                    .await;
            }
        }

        Some(result)
    }

    /// Fails closed if the durable start marker cannot take ownership of the
    /// saved payload, leaving both prompt and images available for retry.
    async fn begin_preparation_prompt(
        context: &SessionWorkerContext,
        operation_id: &str,
        transcript_text: &str,
    ) -> Result<bool, SessionError> {
        match context
            .db
            .sessions()
            .begin_preparation_prompt_operation(&context.session_id, transcript_text)
            .await
        {
            Ok(started) => {
                if started {
                    let _ = context.app_event_tx.send(AppEvent::RefreshSessions);
                }

                Ok(started)
            }
            Err(error) => {
                let message = error.to_string();
                let _ = context
                    .db
                    .operations()
                    .mark_session_operation_failed(operation_id, &message)
                    .await;
                let _ = context
                    .db
                    .sessions()
                    .update_session_preparation(
                        &context.session_id,
                        crate::infra::db::SessionPreparationState::Failed,
                        Some(&message),
                    )
                    .await;
                let _ = context.app_event_tx.send(AppEvent::RefreshSessions);

                Err(error.into())
            }
        }
    }

    /// Publishes the already-persisted prompt to the live transcript without
    /// writing a second durable row. Initial turns tolerate legacy messages;
    /// fork replies may legitimately repeat earlier user text.
    fn append_preparation_prompt(
        context: &SessionWorkerContext,
        request_kind: &AgentRequestKind,
        prompt: &TurnPrompt,
    ) {
        let text = ag_session::stored_message_content(
            SessionMessageKind::UserPrompt,
            &prompt.transcript_text(),
        );
        let recorded = matches!(request_kind, AgentRequestKind::SessionStart)
            && context.transcript.lock().is_ok_and(|transcript| {
                transcript.messages().iter().any(|message| {
                    message.kind == SessionMessageKind::UserPrompt && message.content == text
                })
            });
        if !recorded && let Ok(mut transcript) = context.transcript.lock() {
            transcript.append_message(SessionMessageKind::UserPrompt, &text);
        }
        SessionTaskService::emit_session_updated(
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
        );
    }

    /// Resolves external observers when a queued command is canceled or
    /// otherwise finishes before worker execution begins.
    fn complete_skipped_session_command(context: &SessionWorkerContext, command: &SessionCommand) {
        if matches!(command, SessionCommand::CreateReviewRequest { .. }) {
            let _ = context
                .app_event_tx
                .send(AppEvent::BranchPublishActionResolved {
                    session_id: context.session_id.clone(),
                });
        }

        if matches!(command, SessionCommand::Rebase { .. }) {
            let _ = context
                .app_event_tx
                .send(AppEvent::SessionQueuedSyncResolved {
                    session_id: context.session_id.clone(),
                });
        }

        if let SessionCommand::CreateReviewRequest {
            response: Some(response),
            ..
        } = command
            && let Ok(mut response) = response.lock()
            && let Some(response_tx) = response.take()
        {
            let _ = response_tx.send(Err(ag_session::SessionError::Operation(
                SKIPPED_CREATE_REVIEW_REQUEST_REASON.to_string(),
            )));
        }
    }

    /// Dispatches one queued chat message as a follow-up `SessionResume` turn.
    ///
    /// The turn is persisted as its own `reply` operation with a fresh
    /// identifier so cancellation, retry, and operation tracking behave the
    /// same as a normal reply.
    async fn process_queued_message(
        context: &SessionWorkerContext,
        one_shot_client: &Arc<dyn OneShotClient>,
        message: QueuedMessage,
    ) -> Option<Result<(), SessionError>> {
        let prompt = message.into_prompt();

        // Mirror the queue change into render snapshots so the inline queued
        // row disappears as soon as the follow-up turn starts. The targeted
        // event re-syncs only this session from handles.
        Self::emit_queue_session_updated(context);

        let operation_id = Uuid::new_v4().to_string();
        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .operations()
            .insert_session_operation(&operation_id, &context.session_id, "reply")
            .await;
        let published_upstream_ref = context.load_published_upstream_ref().await;
        append_drained_prompt_to_transcript(context, &prompt).await;
        let command = SessionCommand::Run {
            operation_id,
            request_kind: AgentRequestKind::SessionResume,
            replay_transcript: None,
            prompt,
            turn_metadata: TurnMetadata {
                published_upstream_ref,
                review_comment_thread_ids: Vec::new(),
                session_agent: context.session_agent,
            },
        };

        Self::process_session_command(context, one_shot_client, command).await
    }

    /// Emits a targeted [`AppEvent::SessionUpdated`] for the worker's session
    /// after the in-memory queue mutates so the reducer re-syncs the snapshot
    /// from the handles without paying for a full `RefreshSessions` reload.
    fn emit_queue_session_updated(context: &SessionWorkerContext) {
        let version = SessionTaskService::next_session_update_version(
            &context.session_update_versions,
            context.session_id.as_str(),
        );
        let _ = context.app_event_tx.send(AppEvent::SessionUpdated {
            session_id: context.session_id.clone(),
            version,
        });
    }

    /// Executes the queued command through the session's agent channel.
    async fn execute_session_command(
        context: &SessionWorkerContext,
        one_shot_client: &Arc<dyn OneShotClient>,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        match command {
            SessionCommand::CreateReviewRequest {
                branch_publish_session,
                remote_branch_name,
                response,
                ..
            } => {
                Self::run_create_review_request_command(
                    context,
                    branch_publish_session,
                    remote_branch_name,
                    response,
                )
                .await
            }
            SessionCommand::Rebase { base_branch, .. } => {
                Self::run_rebase_command(context, Arc::clone(one_shot_client), base_branch).await
            }
            SessionCommand::Run {
                request_kind,
                replay_transcript,
                prompt,
                turn_metadata,
                ..
            } => {
                turn::run_channel_turn(
                    context,
                    Arc::clone(one_shot_client),
                    turn_metadata,
                    request_kind,
                    replay_transcript,
                    prompt,
                )
                .await
            }
        }
    }

    /// Publishes one review request inside the serialized session worker.
    ///
    /// The live status is applied at execution time so an action accepted
    /// during `InProgress` or `Rebasing` observes the completed work's
    /// review-ready state instead of the enqueue-time snapshot.
    async fn run_create_review_request_command(
        context: &SessionWorkerContext,
        mut branch_publish_session: BranchPublishTaskSession,
        remote_branch_name: Option<String>,
        response: Option<ReviewRequestResponse>,
    ) -> Result<(), SessionError> {
        branch_publish_session.status = context.current_status();
        let _ = context
            .app_event_tx
            .send(AppEvent::BranchPublishActionStarted {
                session_id: context.session_id.clone(),
            });
        let result = run_branch_publish_action(
            PublishBranchAction::PublishPullRequest,
            BranchPublishTaskContext {
                branch_operation_lock: Arc::clone(&context.branch_operation_lock),
                session: branch_publish_session,
            },
            context.db.clone(),
            Arc::clone(&context.clock),
            Arc::clone(&context.git_client),
            Arc::clone(&context.review_request_client),
            remote_branch_name,
        )
        .await;
        let response_result = review_request_from_publish_result(&result)
            .map_err(ag_session::SessionError::Operation);
        let command_result = review_request_from_publish_result(&result)
            .map(|_| ())
            .map_err(SessionError::Workflow);
        let _ = context
            .app_event_tx
            .send(AppEvent::BranchPublishActionCompleted {
                result: Box::new(result),
                session_id: context.session_id.clone(),
            });
        if let Some(response) = response
            && let Ok(mut response) = response.lock()
            && let Some(response_tx) = response.take()
        {
            let _ = response_tx.send(response_result);
        }

        command_result
    }

    /// Runs the session rebase command inside this worker's serialized queue.
    ///
    /// The rebase task applies the `Rebasing` status at execution time so
    /// sync requested during an active turn can remain visibly queued until
    /// the worker reaches this command.
    ///
    /// # Errors
    /// Returns an error when the rebase workflow fails after appending the
    /// user-visible rebase outcome.
    async fn run_rebase_command(
        context: &SessionWorkerContext,
        one_shot_client: Arc<dyn OneShotClient>,
        base_branch: String,
    ) -> Result<(), SessionError> {
        let validation = match isolation::validate_session_worktree(
            context.fs_client.as_ref(),
            context.git_client.as_ref(),
            &context.folder,
            &context.session_id,
        )
        .await
        {
            Ok(validation) => validation,
            Err(error) => {
                Self::record_rebase_validation_failure(context, &error).await;

                return Err(error);
            }
        };
        let assist_client = Arc::new(SessionWorkerRebaseAssistClient::from_context(
            context,
            validation.main_checkout,
        ));
        SessionManager::run_rebase_command(RebaseCommandInput {
            app_event_tx: context.app_event_tx.clone(),
            assist_mode: RebaseAssistMode::ExistingSession(assist_client),
            base_branch,
            branch_operation_lock: Arc::clone(&context.branch_operation_lock),
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            fs_client: Arc::clone(&context.fs_client),
            git_client: Arc::clone(&context.git_client),
            id: context.session_id.clone(),
            one_shot_client,
            review_request_client: Arc::clone(&context.review_request_client),
            session_agent: context.session_agent,
            session_update_versions: context.session_update_versions.clone(),
            status: Arc::clone(&context.status),
            transcript: Arc::clone(&context.transcript),
        })
        .await
    }

    /// Persists one pre-rebase validation failure before resolving its queue
    /// row.
    async fn record_rebase_validation_failure(
        context: &SessionWorkerContext,
        error: &SessionError,
    ) {
        let notice = TranscriptNotice::RebaseError.format(error);
        SessionTaskService::append_workflow_notice(
            &context.transcript,
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            &notice,
        )
        .await;
        let _ = context
            .app_event_tx
            .send(AppEvent::SessionQueuedSyncResolved {
                session_id: context.session_id.clone(),
            });
    }

    /// Returns whether a queued command should be skipped before execution.
    async fn should_skip_worker_command(
        context: &SessionWorkerContext,
        operation_id: &str,
    ) -> bool {
        let operation_is_unfinished = context
            .db
            .operations()
            .is_session_operation_unfinished(operation_id)
            .await
            .unwrap_or(false);
        if !operation_is_unfinished {
            return true;
        }

        let is_cancel_requested = context
            .db
            .operations()
            .is_cancel_requested_for_operation(operation_id)
            .await
            .unwrap_or(false);
        if !is_cancel_requested {
            return false;
        }

        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .operations()
            .mark_session_operation_canceled(operation_id, CANCEL_BEFORE_EXECUTION_REASON)
            .await;

        true
    }
}

impl SessionManager {
    /// Marks unfinished operations from previous process runs as failed.
    ///
    /// # Errors
    /// Returns an error when startup recovery cannot finish, leaving the
    /// unfinished operations available for a later retry.
    pub(crate) async fn fail_unfinished_operations_from_previous_run(
        db: AppRepositories,
        base_path: PathBuf,
        git_client: Arc<dyn GitClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<(), SessionError> {
        let timestamp_seconds = unix_timestamp_from_system_time(clock.now_system_time());

        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_path.as_path(),
            git_client,
            timestamp_seconds,
        )
        .await
    }

    /// Persists and enqueues a command on the per-session worker queue.
    ///
    /// # Errors
    /// Returns an error if operation persistence fails or no worker is
    /// available.
    pub(super) async fn enqueue_session_command(
        &mut self,
        services: &AppServices,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;

        self.worker_service_mut()
            .enqueue_session_command(services, runtime, command)
            .await
    }

    /// Queues and persists a first turn without letting the worker run until
    /// the caller publishes the initial metadata.
    ///
    /// # Errors
    /// Returns an error when runtime lookup, worker delivery, or operation
    /// persistence fails.
    pub(super) async fn enqueue_gated_session_command(
        &mut self,
        services: &AppServices,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<oneshot::Sender<()>, SessionError> {
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;

        self.worker_service_mut()
            .enqueue_gated_session_command(services, runtime, command)
            .await
    }

    /// Claims and enqueues a command by its stable operation identifier.
    ///
    /// # Errors
    /// Returns an error if the session runtime cannot be built, operation
    /// persistence fails, or no worker is available.
    pub(super) async fn enqueue_session_command_idempotently(
        &mut self,
        services: &AppServices,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<bool, SessionError> {
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;

        self.worker_service_mut()
            .enqueue_session_command_idempotently(services, runtime, command)
            .await
    }

    /// Persists and queues review-request creation on one session worker.
    ///
    /// Active turn and rebase sessions must already own a worker so stale
    /// status cannot create a concurrent executor. Review-ready sessions
    /// lazily create a worker and execute the action immediately.
    ///
    /// # Errors
    /// Returns an error when operation persistence or worker delivery fails.
    pub(crate) async fn enqueue_review_request_creation(
        &mut self,
        services: &AppServices,
        branch_publish_session: BranchPublishTaskSession,
        remote_branch_name: Option<String>,
        response_tx: Option<oneshot::Sender<Result<ReviewRequest, ag_session::SessionError>>>,
    ) -> Result<Option<u64>, SessionError> {
        let session_id = branch_publish_session.id.clone();
        let status = branch_publish_session.status;
        let response = response_tx.map(|response_tx| Arc::new(Mutex::new(Some(response_tx))));
        let command = SessionCommand::CreateReviewRequest {
            branch_publish_session,
            operation_id: Uuid::new_v4().to_string(),
            remote_branch_name,
            response: response.clone(),
        };
        let result = if matches!(status, Status::InProgress | Status::Rebasing) {
            self.worker_service_mut()
                .enqueue_existing_session_command(services, &session_id, command)
                .await
                .map(Some)
        } else {
            self.enqueue_session_command(services, &session_id, command)
                .await
                .map(|()| None)
        };
        if let Err(error) = &result
            && let Some(response) = response
            && let Ok(mut response) = response.lock()
            && let Some(response_tx) = response.take()
        {
            let _ = response_tx.send(Err(ag_session::SessionError::Operation(error.to_string())));
        }

        result
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().clear_session_worker(session_id);
    }

    /// Wakes an existing worker so it re-evaluates buffered work against the
    /// current session status.
    pub(crate) fn wake_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().wake_session_worker(session_id);
    }

    /// Drops worker queues for touched sessions that reached terminal status.
    ///
    /// Terminal sessions (`Done`, `Canceled`) no longer execute turns, so
    /// dropping their worker sender lets the worker task exit and shut down any
    /// provider runtime process associated with that session.
    pub(crate) fn clear_terminal_session_workers(
        &mut self,
        updated_session_ids: &HashSet<SessionId>,
    ) {
        let terminal_session_ids = updated_session_ids
            .iter()
            .filter(|session_id| {
                self.state
                    .handle(session_id)
                    .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                    .is_some_and(|status| matches!(status, Status::Done | Status::Canceled))
            })
            .cloned()
            .collect::<Vec<_>>();

        for session_id in terminal_session_ids {
            self.clear_session_worker(&session_id);
        }
    }

    /// Builds worker-runtime data for one session.
    ///
    /// # Errors
    /// Returns an error when the session or runtime handles are missing.
    fn session_worker_runtime_or_err(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<SessionWorkerRuntime, SessionError> {
        let (session, handles) = self.session_and_handles_or_err(session_id)?;

        Ok(SessionWorkerRuntime {
            branch_operation_lock: Arc::clone(&handles.branch_operation_lock),
            cancel_token: Arc::clone(&handles.cancel_token),
            child_pid: Arc::clone(&handles.child_pid),
            folder: session.folder.clone(),
            personality_catalog_client: services.personality_catalog_client(),
            queued_messages: Arc::clone(&handles.queued_messages),
            queued_work_sequence: Arc::clone(&handles.queued_work_sequence),
            review_request_client: services.review_request_client(),
            session_update_versions: services.session_update_versions(),
            session_id: session.id.clone(),
            session_agent: session.agent,
            status: Arc::clone(&handles.status),
            transcript: Arc::clone(&handles.transcript),
        })
    }
}

/// Appends one drained queued prompt to the typed session transcript so it
/// renders alongside the normal reply prompt line once the queued turn starts
/// running.
async fn append_drained_prompt_to_transcript(context: &SessionWorkerContext, prompt: &TurnPrompt) {
    let prompt_transcript_text = prompt.transcript_text();

    SessionTaskService::append_session_transcript_message(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        SessionTranscriptMessageAppend {
            kind: SessionMessageKind::UserPrompt,
            raw_content: &prompt_transcript_text,
        },
    )
    .await;
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
