//! Per-session async worker orchestration for serialized command execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_forge as forge;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::merge::{
    ExistingSessionRebaseAssistClient, RebaseAssistFuture, RebaseAssistMode, RebaseCommandInput,
};
use super::task::SessionOutputMessageAppend;
use super::{SessionTaskService, session_folder, turn};
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError, unix_timestamp_from_system_time};
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::domain::agent::AgentSelection;
use crate::domain::session::{SessionId, SessionStats, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::infra::channel::{
    AgentChannel, AgentError, AgentRequestKind, TurnEvent, TurnPrompt, TurnRequest, TurnResult,
    create_agent_channel,
};
use crate::infra::db::{AppRepositories, SessionOperationRow};
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;
use crate::infra::{agent, process};

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";

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
        /// Structured user prompt payload.
        prompt: TurnPrompt,
        /// Per-turn metadata consumed during and after turn execution.
        turn_metadata: TurnMetadata,
    },
}

impl SessionCommand {
    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::Rebase { operation_id, .. } | Self::Run { operation_id, .. } => operation_id,
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::Rebase { .. } => "rebase",
            Self::Run {
                request_kind: AgentRequestKind::SessionStart,
                ..
            } => "start_prompt",
            Self::Run {
                request_kind: AgentRequestKind::SessionResume { .. },
                ..
            } => "reply",
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

/// Shared state threaded through all worker turn executions.
pub(super) struct SessionWorkerContext {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Per-turn cancellation token shared with the UI through
    /// [`SessionHandles`]. The worker swaps in a fresh token at the start
    /// of each turn; the UI calls `cancel()` on the current token to
    /// interrupt a running turn.
    pub(super) cancel_token: Arc<Mutex<CancellationToken>>,
    /// Provider-agnostic agent channel for this session's worker.
    pub(super) channel: Arc<dyn AgentChannel>,
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) db: AppRepositories,
    pub(super) folder: PathBuf,
    pub(super) fs_client: Arc<dyn FsClient>,
    pub(super) git_client: Arc<dyn GitClient>,
    pub(super) output: Arc<Mutex<String>>,
    /// In-memory queue of prompts staged while the session is `InProgress`.
    ///
    /// Shared with [`SessionHandles::queued_messages`]. The worker drains
    /// this queue between turns; the lifecycle pushes new entries when a
    /// user submits a chat message during a running turn.
    pub(super) queued_messages: Arc<Mutex<VecDeque<TurnPrompt>>>,
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    pub(super) session_id: SessionId,
    /// Agent provider and model selected for this session.
    pub(super) session_agent: AgentSelection,
    pub(super) status: Arc<Mutex<Status>>,
}

impl SessionWorkerContext {
    /// Pops the next queued prompt for dispatch as a follow-up turn.
    fn pop_queued_prompt(&self) -> Option<TurnPrompt> {
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
    /// Shared child-process PID slot used by cancellation.
    child_pid: Arc<Mutex<Option<u32>>>,
    /// Repository bundle used for conversation and usage persistence.
    db: AppRepositories,
    /// Session worktree folder where the utility prompt runs.
    folder: PathBuf,
    /// Shared transcript buffer for assist summaries.
    output: Arc<Mutex<String>>,
    /// Per-app session update versions for targeted refresh events.
    session_update_versions: SessionUpdateVersionMap,
    /// Session identifier whose provider conversation is reused.
    session_id: SessionId,
    /// Agent provider and model used for the rebase-assist utility prompt.
    session_agent: AgentSelection,
}

impl SessionWorkerRebaseAssistClient {
    /// Clones the worker fields needed to run a rebase-assist utility turn.
    fn from_context(context: &SessionWorkerContext) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            cancel_token: Arc::clone(&context.cancel_token),
            channel: Arc::clone(&context.channel),
            child_pid: Arc::clone(&context.child_pid),
            db: context.db.clone(),
            folder: context.folder.clone(),
            output: Arc::clone(&context.output),
            session_update_versions: context.session_update_versions.clone(),
            session_id: context.session_id.clone(),
            session_agent: context.session_agent,
        }
    }

    /// Runs one utility prompt through the current session channel.
    ///
    /// # Errors
    /// Returns an error when the provider turn fails or conversation metadata
    /// cannot be persisted.
    async fn run_assist_turn(&self, prompt: String) -> Result<(), SessionError> {
        let turn_cancel_token = self.fresh_turn_cancel_token()?;
        let session_project_id = turn::load_session_project_id(&self.db, &self.session_id).await;
        let reasoning_level =
            turn::load_session_reasoning_level(&self.db, &self.session_id, session_project_id)
                .await;
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
            folder: self.folder.clone(),
            live_session_output: Some(Arc::clone(&self.output)),
            model: self.session_agent.model().provider_model_str().to_string(),
            request_kind: AgentRequestKind::UtilityPrompt,
            prompt: TurnPrompt::from_agent_data(prompt),
            provider_conversation_id,
            persisted_instruction_conversation_id,
            reasoning_level,
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
        // Sync critical section (assignment + clone, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
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
            self.terminate_child_process();
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
                self.terminate_child_process();
                let _ = self.channel.shutdown_session(self.session_id.to_string()).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), &mut turn_future).await;

                Err(AgentError::InterruptedByUser(
                    "[Stopped] Session interrupted by user.".to_string(),
                ))
            }
        }
    }

    /// Sends `SIGTERM` to the active child process tracked by this assist turn.
    fn terminate_child_process(&self) {
        // Sync critical section (take PID, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        let active_pid = self
            .child_pid
            .lock()
            .ok()
            .and_then(|mut child_pid| child_pid.take());

        if let Some(pid) = active_pid {
            process::send_terminate_signal(pid);
        }
    }

    /// Appends the utility prompt answer to the session transcript.
    async fn append_assist_answer(&self, assistant_message: &agent::AgentResponse) {
        let answer_text = assistant_message.to_answer_display_text();
        if answer_text.trim().is_empty() {
            return;
        }

        SessionTaskService::append_session_output_message(
            &self.output,
            &self.db,
            &self.app_event_tx,
            &self.session_update_versions,
            &self.session_id,
            SessionOutputMessageAppend {
                formatted_message: &answer_text,
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
    cancel_token: Arc<Mutex<CancellationToken>>,
    child_pid: Arc<Mutex<Option<u32>>>,
    folder: PathBuf,
    output: Arc<Mutex<String>>,
    queued_messages: Arc<Mutex<VecDeque<TurnPrompt>>>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Per-app session update versions shared with the main runtime.
    session_update_versions: SessionUpdateVersionMap,
    /// Agent provider and model selected for this session.
    session_agent: AgentSelection,
    session_id: SessionId,
    status: Arc<Mutex<Status>>,
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
    workers: HashMap<SessionId, mpsc::UnboundedSender<SessionCommand>>,
}

impl SessionWorkerService {
    /// Creates an empty worker service with no active session workers.
    pub(in crate::app::session) fn new() -> Self {
        Self {
            test_agent_channels: HashMap::new(),
            workers: HashMap::new(),
        }
    }

    /// Marks unfinished operations from previous process runs as failed and
    /// closes any open active-work timing window at `timestamp_seconds`.
    pub(super) async fn fail_unfinished_operations_from_previous_run_at(
        db: &AppRepositories,
        base_path: &Path,
        git_client: Arc<dyn GitClient>,
        timestamp_seconds: i64,
    ) {
        let unfinished_operations = db
            .operations()
            .load_unfinished_session_operations()
            .await
            .unwrap_or_default();
        Self::abort_rebase_operations_from_previous_run(
            base_path,
            git_client.as_ref(),
            &unfinished_operations,
        )
        .await;

        let interrupted_session_ids: HashSet<String> = unfinished_operations
            .into_iter()
            .map(|operation| operation.session_id)
            .collect();

        for session_id in interrupted_session_ids {
            // Best-effort: status persistence failure is non-critical.
            let _ = db
                .sessions()
                .update_session_status_with_timing_at(
                    &session_id,
                    &Status::Review.to_string(),
                    timestamp_seconds,
                )
                .await;
        }

        // Best-effort: operation tracking metadata is non-critical.
        let _ = db
            .operations()
            .fail_unfinished_session_operations(RESTART_FAILURE_REASON)
            .await;
    }

    /// Aborts stale git rebase state left by interrupted worker operations.
    ///
    /// Only worker-backed rebase operations are handled here because merge
    /// tasks are not yet persisted in `session_operation`.
    async fn abort_rebase_operations_from_previous_run(
        base_path: &Path,
        git_client: &dyn GitClient,
        unfinished_operations: &[SessionOperationRow],
    ) {
        let mut rebase_session_ids = unfinished_operations
            .iter()
            .filter(|operation| operation.kind == "rebase")
            .map(|operation| operation.session_id.as_str())
            .collect::<Vec<_>>();
        rebase_session_ids.sort_unstable();
        rebase_session_ids.dedup();

        for session_id in rebase_session_ids {
            let folder = session_folder(base_path, session_id);
            let Ok(is_rebase_in_progress) = git_client.is_rebase_in_progress(folder.clone()).await
            else {
                continue;
            };
            if is_rebase_in_progress {
                let _ = git_client.abort_rebase(folder).await;
            }
        }
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
        let operation_id = command.operation_id().to_string();
        let session_id = runtime.session_id.clone();
        services
            .db()
            .operations()
            .insert_session_operation(&operation_id, &session_id, command.kind())
            .await?;

        let sender = self.ensure_session_worker(services, &runtime);
        if sender.send(command).is_err() {
            // Best-effort: operation tracking metadata is non-critical.
            let _ = services
                .db()
                .operations()
                .mark_session_operation_failed(&operation_id, "Session worker is not available")
                .await;

            return Err(SessionError::Workflow(
                "Session worker is not available".to_string(),
            ));
        }

        Ok(())
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.workers.remove(session_id);
    }

    /// Drops worker queues for sessions no longer present in the active list.
    pub(super) fn retain_active_workers(&mut self, active_session_ids: &HashSet<SessionId>) {
        self.workers
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Returns an existing session worker sender or creates one lazily.
    fn ensure_session_worker(
        &mut self,
        services: &AppServices,
        runtime: &SessionWorkerRuntime,
    ) -> mpsc::UnboundedSender<SessionCommand> {
        if let Some(sender) = self.workers.get(&runtime.session_id) {
            return sender.clone();
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
            cancel_token: Arc::clone(&runtime.cancel_token),
            channel,
            child_pid: Arc::clone(&runtime.child_pid),
            clock: services.clock(),
            db: services.db().clone(),
            folder: runtime.folder.clone(),
            fs_client: services.fs_client(),
            git_client: services.git_client(),
            output: Arc::clone(&runtime.output),
            queued_messages: Arc::clone(&runtime.queued_messages),
            review_request_client: Arc::clone(&runtime.review_request_client),
            session_update_versions: Arc::clone(&runtime.session_update_versions),
            session_id: runtime.session_id.clone(),
            session_agent: runtime.session_agent,
            status: Arc::clone(&runtime.status),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        self.workers
            .insert(runtime.session_id.clone(), sender.clone());
        Self::spawn_session_worker(context, receiver);

        sender
    }

    /// Spawns the background loop that executes queued session commands.
    ///
    /// After each command completes the worker drains
    /// [`SessionWorkerContext::queued_messages`] inline so user prompts
    /// submitted while a turn was running dispatch as follow-up turns
    /// without bouncing the session through `Review` between them. Drainage
    /// pauses while the session is in `Question` state and resumes once
    /// status returns to a runnable state. A turn stopped by the user
    /// (`Ctrl+C`) clears the queue so canceled work does not silently leak
    /// into the next session activity.
    fn spawn_session_worker(
        context: SessionWorkerContext,
        mut receiver: mpsc::UnboundedReceiver<SessionCommand>,
    ) {
        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                let result = Self::process_session_command(&context, command).await;
                if matches!(result, Some(Err(SessionError::StoppedByUser(_)))) {
                    context.clear_queued_messages();
                    Self::emit_queue_session_updated(&context);
                    continue;
                }

                Self::drain_queued_messages(&context).await;
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

    /// Executes one queued session command including its operation
    /// bookkeeping. Returns `None` when the command was skipped before
    /// execution (already finished or cancelled) and `Some(result)` when the
    /// turn ran.
    async fn process_session_command(
        context: &SessionWorkerContext,
        command: SessionCommand,
    ) -> Option<Result<(), SessionError>> {
        let operation_id = command.operation_id().to_string();
        if Self::should_skip_worker_command(context, &operation_id).await {
            return None;
        }
        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .operations()
            .mark_session_operation_running(&operation_id)
            .await;
        if Self::should_skip_worker_command(context, &operation_id).await {
            return None;
        }

        let result = Self::execute_session_command(context, command).await;
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

    /// Pops queued prompts and dispatches them as follow-up `SessionResume`
    /// turns until the queue is empty or the session enters `Question`
    /// state.
    ///
    /// Each drained turn is persisted as its own `reply` operation with a
    /// fresh identifier so cancellation, retry, and operation tracking
    /// behave the same as a normal reply. The drain stops on the first
    /// user-stopped turn and clears the remaining queue so `Ctrl+C` cancels
    /// the queued work cleanly.
    async fn drain_queued_messages(context: &SessionWorkerContext) {
        loop {
            if matches!(context.current_status(), Status::Question) {
                return;
            }
            let Some(prompt) = context.pop_queued_prompt() else {
                return;
            };

            // Mirror the queue change into render snapshots so the inline
            // "queued" rows disappear as soon as drainage starts the
            // follow-up turn. The targeted `SessionUpdated` event re-syncs
            // only this session's snapshot from handles instead of paying for
            // a full DB-backed `RefreshSessions` reload.
            Self::emit_queue_session_updated(context);

            let operation_id = Uuid::new_v4().to_string();
            // Best-effort: operation tracking metadata is non-critical.
            let _ = context
                .db
                .operations()
                .insert_session_operation(&operation_id, &context.session_id, "reply")
                .await;
            let published_upstream_ref = context.load_published_upstream_ref().await;
            append_drained_prompt_to_output(context, &prompt).await;
            let command = SessionCommand::Run {
                operation_id,
                request_kind: AgentRequestKind::SessionResume {
                    session_output: None,
                },
                prompt,
                turn_metadata: TurnMetadata {
                    published_upstream_ref,
                    session_agent: context.session_agent,
                },
            };
            let result = Self::process_session_command(context, command).await;
            if matches!(result, Some(Err(SessionError::StoppedByUser(_)))) {
                context.clear_queued_messages();
                Self::emit_queue_session_updated(context);

                return;
            }
        }
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
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        match command {
            SessionCommand::Rebase { base_branch, .. } => {
                Self::run_rebase_command(context, base_branch).await
            }
            SessionCommand::Run {
                request_kind,
                prompt,
                turn_metadata,
                ..
            } => turn::run_channel_turn(context, turn_metadata, request_kind, prompt).await,
        }
    }

    /// Runs the session rebase command inside this worker's serialized queue.
    ///
    /// # Errors
    /// Returns an error when the rebase workflow fails after appending the
    /// user-visible rebase outcome.
    async fn run_rebase_command(
        context: &SessionWorkerContext,
        base_branch: String,
    ) -> Result<(), SessionError> {
        let assist_client = Arc::new(SessionWorkerRebaseAssistClient::from_context(context));
        SessionManager::run_rebase_command(RebaseCommandInput {
            app_event_tx: context.app_event_tx.clone(),
            assist_mode: RebaseAssistMode::ExistingSession(assist_client),
            base_branch,
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            fs_client: Arc::clone(&context.fs_client),
            git_client: Arc::clone(&context.git_client),
            id: context.session_id.clone(),
            output: Arc::clone(&context.output),
            session_agent: context.session_agent,
            session_update_versions: context.session_update_versions.clone(),
            status: Arc::clone(&context.status),
        })
        .await
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
    pub(crate) async fn fail_unfinished_operations_from_previous_run(
        db: AppRepositories,
        base_path: PathBuf,
        git_client: Arc<dyn GitClient>,
        clock: Arc<dyn Clock>,
    ) {
        let timestamp_seconds = unix_timestamp_from_system_time(clock.now_system_time());

        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_path.as_path(),
            git_client,
            timestamp_seconds,
        )
        .await;
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

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().clear_session_worker(session_id);
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
            .filter_map(|session_id| {
                self.sessions()
                    .iter()
                    .find(|session| session.id == *session_id)
                    .and_then(|session| {
                        matches!(session.status, Status::Done | Status::Canceled)
                            .then(|| session.id.clone())
                    })
            })
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
            cancel_token: Arc::clone(&handles.cancel_token),
            child_pid: Arc::clone(&handles.child_pid),
            folder: session.folder.clone(),
            output: Arc::clone(&handles.output),
            queued_messages: Arc::clone(&handles.queued_messages),
            review_request_client: services.review_request_client(),
            session_update_versions: services.session_update_versions(),
            session_id: session.id.clone(),
            session_agent: session.agent,
            status: Arc::clone(&handles.status),
        })
    }
}

/// Appends one drained queued prompt to the session transcript and shared
/// output buffer so it renders alongside the normal reply prompt line once
/// the queued turn starts running.
///
/// Mirrors the formatting used by [`SessionManager::formatted_prompt_output`]
/// from the live reply path: the first line uses `USER_PROMPT_PREFIX` and
/// continuation lines use `USER_PROMPT_CONTINUATION_PREFIX` so blank lines
/// inside the prompt do not look like a transcript boundary.
async fn append_drained_prompt_to_output(context: &SessionWorkerContext, prompt: &TurnPrompt) {
    const USER_PROMPT_PREFIX: &str = " › ";
    const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";

    let prompt_text = prompt.transcript_text();
    let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
    let mut formatted_lines = Vec::with_capacity(prompt_lines.len());
    for (line_index, prompt_line) in prompt_lines.into_iter().enumerate() {
        let prefix = if line_index == 0 {
            USER_PROMPT_PREFIX
        } else {
            USER_PROMPT_CONTINUATION_PREFIX
        };

        formatted_lines.push(format!("{prefix}{prompt_line}"));
    }
    let prompt_block = formatted_lines.join("\n");
    let message = format!("\n{prompt_block}\n\n");
    let prompt_transcript_text = prompt.transcript_text();

    SessionTaskService::append_session_output_message(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        SessionOutputMessageAppend {
            formatted_message: &message,
            kind: SessionMessageKind::UserPrompt,
            raw_content: &prompt_transcript_text,
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mockall::Sequence;
    use serde_json;
    use tempfile::tempdir;

    use super::super::post_turn::{
        PostTurnContext, apply_turn_result, build_assistant_transcript_output,
        persisted_session_summary_payload, status_update_after_turn_result,
    };
    use super::super::turn::{
        consume_turn_events, run_channel_turn, run_turn_with_cancellation, terminate_child_process,
    };
    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
    use crate::domain::question::QuestionItem;
    use crate::domain::session::{PublishedBranchSyncStatus, ReviewRequest, ReviewRequestState};
    use crate::infra::agent::AgentResponse;
    use crate::infra::agent::protocol::AgentResponseSummary;
    use crate::infra::channel::MockAgentChannel;
    use crate::infra::db::AppRepositories;
    use crate::infra::fs;
    use crate::infra::git::{MockGitClient, RebaseStepResult};

    /// Builds one filesystem mock that treats every probed path as an
    /// existing directory.
    fn mock_fs_client_with_existing_directories() -> fs::MockFsClient {
        let mut fs_client = fs::MockFsClient::new();
        fs_client.expect_is_dir().times(0..).returning(|_| true);
        fs_client
            .expect_canonicalize()
            .times(0..)
            .returning(|path| Box::pin(async move { Ok(path) }));

        fs_client
    }

    /// Inserts one in-progress Antigravity-backed session for worker-flow
    /// tests.
    async fn insert_in_progress_test_session(db: &AppRepositories) -> i64 {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        project_id
    }

    /// Applies one turn result through the narrowed post-turn dependency set
    /// cloned from a full worker context.
    async fn apply_worker_turn_result(
        context: &SessionWorkerContext,
        turn_metadata: TurnMetadata,
        turn_result: Result<TurnResult, AgentError>,
    ) -> Result<Status, SessionError> {
        let post_turn_context = PostTurnContext::from_worker(context);

        apply_turn_result(&post_turn_context, turn_metadata, turn_result).await
    }

    #[test]
    fn test_status_update_after_turn_result_skips_stopped_by_user() {
        // Arrange
        let result = Err(SessionError::StoppedByUser(
            "[Stopped] Session interrupted by user.".to_string(),
        ));

        // Act
        let status_update = status_update_after_turn_result(&result);

        // Assert
        assert_eq!(status_update, None);
    }

    #[test]
    fn test_status_update_after_turn_result_falls_back_to_review_for_errors() {
        // Arrange
        let result = Err(SessionError::Workflow("backend failed".to_string()));

        // Act
        let status_update = status_update_after_turn_result(&result);

        // Assert
        assert_eq!(status_update, Some(Status::Review));
    }

    #[test]
    /// Ensures session command request kinds map to stable persisted
    /// operation labels.
    fn test_session_command_kind_values() {
        // Arrange
        let start_command = SessionCommand::Run {
            operation_id: "op-start".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet46,
                ),
            },
        };
        let resume_command = SessionCommand::Run {
            operation_id: "op-resume".to_string(),
            request_kind: AgentRequestKind::SessionResume {
                session_output: None,
            },
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet46,
                ),
            },
        };
        let account_read_command = SessionCommand::Run {
            operation_id: "op-account-read".to_string(),
            request_kind: AgentRequestKind::AccountRead,
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet46,
                ),
            },
        };

        // Act
        let start_kind = start_command.kind();
        let resume_kind = resume_command.kind();
        let account_read_kind = account_read_command.kind();

        // Assert
        assert_eq!(start_kind, "start_prompt");
        assert_eq!(resume_kind, "reply");
        assert_eq!(account_read_kind, "account_read");
    }

    #[test]
    fn test_agent_response_questions_returns_only_question_messages() {
        // Arrange
        let agent_response = AgentResponse {
            answer: "Implemented the feature.".to_string(),
            questions: vec![
                QuestionItem::new("Need a target branch?"),
                QuestionItem::new("Need migration notes?"),
            ],
            summary: None,
        };

        // Act
        let items = agent_response.question_items();

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "Need a target branch?");
        assert_eq!(items[1].text, "Need migration notes?");
    }

    #[test]
    fn test_agent_response_questions_preserves_ordered_list_as_single_question_text() {
        // Arrange
        let numbered_questions =
            "1) Is this repository intentionally incomplete (docs-only), or should it include the \
             referenced dotfiles tree (for\nexample `.config/` and `lua/`)?\n2) Should I propose \
             and apply a docs-only cleanup now (aligning setup steps to the current files), or \
             keep docs\nas-is and treat missing files as a known gap?\n3) Do you want keyd \
             instructions rewritten to the safer `/etc/keyd/default.conf` path with existence \
             checks and\nrollback notes?";
        let agent_response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::new(numbered_questions)],
            summary: None,
        };

        // Act
        let items = agent_response.question_items();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, numbered_questions);
    }

    #[test]
    /// Ensures transcript output prefers `answer` messages when available.
    fn test_build_assistant_transcript_output_prefers_answer_messages() {
        // Arrange
        let response = AgentResponse {
            answer: "Implemented the fix.".to_string(),
            questions: vec![QuestionItem::new("Need me to run tests?")],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response);

        // Assert
        assert_eq!(
            transcript_output,
            Some("Implemented the fix.\n\n".to_string())
        );
    }

    #[test]
    /// Ensures transcript output falls back to question text when no answers
    /// are present.
    fn test_build_assistant_transcript_output_falls_back_to_question_text() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::new("Should I apply the patch?")],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response);

        // Assert
        assert_eq!(
            transcript_output,
            Some("Should I apply the patch?\n\n".to_string())
        );
    }

    #[test]
    /// Ensures blank protocol messages do not append empty transcript output.
    fn test_build_assistant_transcript_output_returns_none_for_blank_messages() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::new("\n")],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response);

        // Assert
        assert_eq!(transcript_output, None);
    }

    #[test]
    /// Ensures persisted summaries keep the raw turn/session payload for
    /// review-mode rendering.
    fn test_persisted_session_summary_payload_serializes_structured_summary() {
        // Arrange
        let response = AgentResponse {
            answer: "Implemented the fix.".to_string(),
            questions: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: "Updated the greeting flow.".to_string(),
                session: "Session now greets users on startup.".to_string(),
            }),
        };

        // Act
        let persisted_summary = persisted_session_summary_payload(&response);

        // Assert
        let summary = serde_json::from_str::<AgentResponseSummary>(&persisted_summary)
            .expect("summary should deserialize");

        assert_eq!(
            summary,
            AgentResponseSummary {
                session: "Session now greets users on startup.".to_string(),
                turn: "Updated the greeting flow.".to_string(),
            }
        );
    }

    #[tokio::test]
    /// Verifies non-output events do not append transcript content.
    async fn test_consume_turn_events_ignores_pid_only_events_for_transcript_output() {
        // Arrange
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::PidUpdate(Some(4242)))
            .expect("failed to send pid update");
        drop(event_tx);

        // Act
        consume_turn_events(
            event_rx,
            app_event_tx,
            "session-1".into(),
            Arc::clone(&child_pid),
        )
        .await;

        // Assert
        assert_eq!(*child_pid.lock().expect("pid lock poisoned"), Some(4242));
        assert!(app_event_rx.try_recv().is_err());
    }

    #[tokio::test]
    /// Verifies the worker's `select!` cancellation path gracefully stops a
    /// running turn through `shutdown_session` and returns the `[Stopped]`
    /// error text when the cancel token is cancelled during `run_channel_turn`.
    async fn test_run_channel_turn_returns_stopped_when_cancel_token_fires() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        insert_in_progress_test_session(&db).await;

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    // Simulate a long-running app-server turn that never
                    // completes on its own.
                    tokio::time::sleep(std::time::Duration::from_hours(1)).await;
                    unreachable!("should be cancelled before completing")
                })
            });
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_git_client = MockGitClient::new();
        let main_repo_root = base_dir.path().join("main");
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client.expect_main_repo_root().once().returning({
            let main_repo_root = main_repo_root.clone();

            move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(main_repo_root) })
            }
        });
        mock_git_client
            .expect_tracked_worktree_status()
            .once()
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));

        let cancel_token = Arc::new(Mutex::new(CancellationToken::new()));
        let output = Arc::new(Mutex::new(String::new()));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::clone(&cancel_token),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            output: Arc::clone(&output),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Cancel the token shortly after the turn starts.
        let token_handle = Arc::clone(&cancel_token);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token_handle.lock().expect("cancel token lock").cancel();
        });

        // Act
        let result = run_channel_turn(
            &context,
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            "test prompt".into(),
        )
        .await;

        // Assert
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
        let output_text = output.lock().expect("output lock").clone();
        assert!(
            output_text.contains("[Stopped]"),
            "stopped message should be appended to output, got: {output_text}"
        );
        assert_eq!(
            *context.status.lock().expect("status lock poisoned"),
            Status::InProgress,
            "stopped turn worker must not fall back to Review before the UI cancellation path \
             finalizes Canceled"
        );
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(
            sessions[0].status, "InProgress",
            "stopped turn worker must not persist Review and trigger automatic focused review"
        );
    }

    #[tokio::test]
    /// Verifies that a previous turn's cancelled token does not affect the
    /// next turn. Each turn swaps in a fresh `CancellationToken`, so stale
    /// cancellations are structurally impossible.
    async fn test_run_channel_turn_proceeds_after_previous_cancellation() {
        // Arrange — pre-cancel the token to simulate a previous turn's
        // cancellation. `run_channel_turn` swaps in a fresh token so the
        // stale cancellation is discarded.
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            questions: Vec::new(),
                            summary: None,
                        },
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });

        let mut mock_git_client = MockGitClient::new();
        let main_repo_root = base_dir.path().join("main");
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client.expect_main_repo_root().once().returning({
            let main_repo_root = main_repo_root.clone();

            move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(main_repo_root) })
            }
        });
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        // Pre-cancel the token to simulate a previous turn's cancellation.
        let stale_token = CancellationToken::new();
        stale_token.cancel();
        let session_agent =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini3FlashPreview);

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(stale_token)),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act — the turn should complete normally because
        // `run_channel_turn` swaps in a fresh token.
        let result = run_channel_turn(
            &context,
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            "test prompt".into(),
        )
        .await;

        // Assert — turn succeeded despite the stale cancellation.
        assert!(
            result.is_ok(),
            "stale cancelled token should not cancel the new turn"
        );
    }

    #[tokio::test]
    /// Verifies a turn that dirties the main checkout records a warning while
    /// preserving the successful agent response.
    async fn test_run_channel_turn_warns_when_main_checkout_status_changes() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        insert_in_progress_test_session(&db).await;

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .once()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            questions: Vec::new(),
                            summary: None,
                        },
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });

        let main_repo_root = base_dir.path().join("main");
        let status_call_count = Arc::new(Mutex::new(0));
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client.expect_main_repo_root().once().returning({
            let main_repo_root = main_repo_root.clone();

            move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(main_repo_root) })
            }
        });
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(move |_| {
                let status_call_count = Arc::clone(&status_call_count);

                Box::pin(async move {
                    let mut call_count = status_call_count
                        .lock()
                        .expect("status call count lock poisoned");
                    *call_count += 1;
                    if *call_count == 1 {
                        Ok(String::new())
                    } else {
                        Ok(" M README.md\n".to_string())
                    }
                })
            });
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let output = Arc::new(Mutex::new(String::new()));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            output: Arc::clone(&output),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let result = run_channel_turn(
            &context,
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            "test prompt".into(),
        )
        .await;

        // Assert
        assert!(result.is_ok(), "main checkout changes should warn only");
        let output_text = output.lock().expect("output lock poisoned");
        assert!(output_text.contains("[Main Checkout Warning]"));
        assert!(output_text.contains("done"));
    }

    /// Builds the default turn metadata used by session worker tests that
    /// exercise the `Gemini3FlashPreview` path without branch publication.
    fn default_turn_metadata() -> TurnMetadata {
        TurnMetadata {
            published_upstream_ref: None,
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
        }
    }

    #[tokio::test]
    /// Verifies that a cancel arriving during the pre-turn setup window
    /// (between the token swap in `run_channel_turn` and the entry into
    /// `run_turn_with_cancellation`) is honoured immediately. The token is
    /// already cancelled before `run_turn_with_cancellation` starts, so
    /// `run_turn` must never be called.
    async fn test_run_turn_with_cancellation_honours_pre_turn_cancel() {
        // Arrange — create a pre-cancelled token, simulating a Ctrl+c
        // that arrived during pre-turn setup.
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();

        let mut mock_channel = MockAgentChannel::new();
        // `run_turn` must NOT be called — the early-exit path returns
        // before reaching the select.
        mock_channel.expect_run_turn().never();
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await,
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-preturn".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        let req = TurnRequest {
            folder: context.folder.clone(),
            live_session_output: None,
            model: "gemini-3-flash-preview".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "test".into(),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
        };

        // Act — pass the pre-cancelled token directly.
        let result =
            run_turn_with_cancellation(&context, cancel_token, req, mpsc::unbounded_channel().0)
                .await;

        // Assert — should return [Stopped] without ever calling run_turn.
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
    }

    #[tokio::test]
    /// Verifies that `run_turn_with_cancellation` returns `[Stopped]` even
    /// when `run_turn` does not resolve after `shutdown_session`. The
    /// 5-second timeout guard ensures the cancellation branch does not
    /// block indefinitely.
    async fn test_run_turn_with_cancellation_returns_stopped_after_drain_timeout() {
        // Arrange — mock channel whose `run_turn` never resolves and
        // whose `shutdown_session` completes immediately (simulating a
        // channel that ignores the shutdown request).
        let cancel_token = CancellationToken::new();

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    // Never resolves — simulates a stuck channel.
                    std::future::pending::<Result<TurnResult, AgentError>>().await
                })
            });
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await,
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-timeout".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        let req = TurnRequest {
            folder: context.folder.clone(),
            live_session_output: None,
            model: "gemini-3-flash-preview".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "test".into(),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
        };

        // Spawn a task that cancels the token after a small delay so the
        // select branch fires mid-turn (not before the pre-check).
        let token_for_cancel = cancel_token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token_for_cancel.cancel();
        });

        // Act — the drain timeout (5 seconds) runs with real wall-clock
        // delay. This test validates that the function does not block
        // indefinitely when `run_turn` never resolves.
        let result =
            run_turn_with_cancellation(&context, cancel_token, req, mpsc::unbounded_channel().0)
                .await;

        // Assert — returns [Stopped] despite `run_turn` never resolving.
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
    }

    #[tokio::test]
    /// Verifies that `terminate_child_process` sends `SIGTERM` to the
    /// child process tracked in the context's PID slot, killing it.
    async fn test_terminate_child_process_sends_sigterm_to_active_child() {
        // Arrange — spawn a long-running child and store its PID in the
        // context.
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep");
        let child_pid = child.id().expect("child has no pid");

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(Some(child_pid))),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await,
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-term".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        terminate_child_process(&context);

        // Assert — the child should have been terminated by SIGTERM.
        let exit_status = child.wait().await.expect("failed to wait on child");
        assert!(
            !exit_status.success(),
            "child should have been killed by SIGTERM"
        );
        // PID slot should be cleared after termination.
        assert!(
            context.child_pid.lock().expect("child_pid lock").is_none(),
            "PID slot should be cleared after termination"
        );
    }

    #[tokio::test]
    /// Verifies that `terminate_child_process` is a no-op when no child
    /// PID is stored (app-server channels never set a PID).
    async fn test_terminate_child_process_noop_when_no_pid() {
        // Arrange
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await,
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-nopid".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act — should not panic or error.
        terminate_child_process(&context);

        // Assert — PID slot remains None.
        assert!(
            context.child_pid.lock().expect("child_pid lock").is_none(),
            "PID slot should still be None"
        );
    }

    #[tokio::test]
    /// Verifies thought deltas update the loader state without appending
    /// transcript output.
    async fn test_consume_turn_events_routes_thought_delta_to_progress_state_only() {
        // Arrange
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::ThoughtDelta("Inspecting files".to_string()))
            .expect("failed to send thought delta");
        drop(event_tx);

        // Act
        consume_turn_events(event_rx, app_event_tx, "session-1".into(), child_pid).await;

        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            events,
            vec![
                AppEvent::SessionProgressUpdated {
                    progress_message: Some("Inspecting files".to_string()),
                    session_id: "session-1".into(),
                },
                AppEvent::SessionProgressUpdated {
                    progress_message: None,
                    session_id: "session-1".into(),
                },
            ]
        );
    }

    #[tokio::test]
    /// Verifies turn summaries are persisted to the database when the agent
    /// returns them.
    async fn test_apply_turn_result_persists_summary_to_database() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let session_agent =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini3FlashPreview);
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                summary: Some(AgentResponseSummary {
                    turn: "- Updated the worker flow.".to_string(),
                    session: "- Active review now reloads summary from persistence.".to_string(),
                }),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(status, Status::Review);
        let summary = sessions[0].summary.as_deref().map(|raw| {
            serde_json::from_str::<AgentResponseSummary>(raw)
                .expect("stored summary should deserialize")
        });
        assert_eq!(
            summary,
            Some(AgentResponseSummary {
                session: "- Active review now reloads summary from persistence.".to_string(),
                turn: "- Updated the worker flow.".to_string(),
            })
        );
        let output = context.output.lock().expect("output lock poisoned");
        assert!(output.starts_with("Implemented the change.\n\n"));
        assert!(!output.contains("[Commit] No changes to commit."));
        assert!(!output.contains("## Change Summary"));
        assert!(!output.contains("Document the worker summary flow."));
    }

    #[tokio::test]
    /// Verifies completed turns keep a linked open PR/MR title and
    /// description aligned with the latest session commit message.
    async fn test_apply_turn_result_syncs_linked_review_request_metadata_after_commit() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        insert_in_progress_session_with_review_request(&db).await;
        let folder = base_dir.path().join("sess1");
        let commit_message =
            "Refine review metadata sync\n\n- Update the linked review request body.";
        let mut sequence = Sequence::new();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder,
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(auto_commit_git_client(commit_message, &mut sequence)),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(review_metadata_sync_client(
                base_dir.path(),
                &mut sequence,
            )),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Antigravity,
                    crate::domain::agent::AgentModel::Gemini3FlashPreview,
                ),
            },
            Ok(successful_turn_result("Implemented the change.")),
        )
        .await
        .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated { sync_status, .. } = event {
                    sync_events.push(sync_status);
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");
        let review_request = db
            .reviews()
            .load_session_review_request("sess1")
            .await
            .expect("failed to load review request")
            .expect("review request should remain linked");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            sync_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Succeeded,
            ]
        );
        assert_eq!(review_request.title, "Refine review metadata sync");
    }

    #[tokio::test]
    /// Verifies failed post-turn auto-push skips linked PR/MR metadata sync.
    async fn test_apply_turn_result_skips_review_request_metadata_sync_when_auto_push_fails() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        insert_in_progress_session_with_review_request(&db).await;
        let commit_message =
            "Refine review metadata sync\n\n- Update the linked review request body.";
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(auto_commit_git_client_with_push_failure(commit_message)),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Antigravity,
                    crate::domain::agent::AgentModel::Gemini3FlashPreview,
                ),
            },
            Ok(successful_turn_result("Implemented the change.")),
        )
        .await
        .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated { sync_status, .. } = event {
                    sync_events.push(sync_status);
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");
        let review_request = db
            .reviews()
            .load_session_review_request("sess1")
            .await
            .expect("failed to load review request")
            .expect("review request should remain linked");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            sync_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Failed,
            ]
        );
        assert_eq!(review_request.title, "Old title");
    }

    /// Inserts an in-progress session linked to an open GitHub review request.
    async fn insert_in_progress_session_with_review_request(db: &AppRepositories) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.reviews()
            .update_session_review_request("sess1", Some(linked_github_review_request()))
            .await
            .expect("failed to persist review request");
    }

    /// Returns one linked GitHub review request fixture for metadata sync
    /// tests.
    fn linked_github_review_request() -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: 100,
            summary: forge::ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: forge::ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Draft".to_string()),
                target_branch: "main".to_string(),
                title: "Old title".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        }
    }

    /// Returns one git client mock that produces a successful auto-commit
    /// outcome.
    fn auto_commit_git_client(commit_message: &str, sequence: &mut Sequence) -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .once()
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .once()
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .once()
            .returning({
                let commit_message = commit_message.to_string();

                move |_| {
                    let commit_message = commit_message.clone();

                    Box::pin(async move { Ok(Some(commit_message)) })
                }
            });
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .once()
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .once()
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
            })
            .in_sequence(sequence)
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        mock_git_client
            .expect_repo_url()
            .once()
            .in_sequence(sequence)
            .returning(|_| {
                Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
            });

        mock_git_client
    }

    /// Returns one git client mock that commits successfully but fails the
    /// follow-up auto-push.
    fn auto_commit_git_client_with_push_failure(commit_message: &str) -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .once()
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .once()
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .once()
            .returning({
                let commit_message = commit_message.to_string();

                move |_| {
                    let commit_message = commit_message.clone();

                    Box::pin(async move { Ok(Some(commit_message)) })
                }
            });
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .once()
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .once()
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
            })
            .returning(|_, _| {
                Box::pin(async {
                    Err(crate::infra::git::GitError::CommandFailed {
                        command: "git push origin wt/session-id".to_string(),
                        stderr: "fatal: remote rejected the push".to_string(),
                    })
                })
            });

        mock_git_client
    }

    /// Returns one review-request client mock that expects the latest commit
    /// message metadata.
    fn review_metadata_sync_client(
        base_dir: &std::path::Path,
        sequence: &mut Sequence,
    ) -> forge::MockReviewRequestClient {
        let folder = base_dir.join("sess1");
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .in_sequence(sequence)
            .returning(|_| Ok(github_forge_remote()));
        mock_review_request_client
            .expect_sync_review_request_metadata()
            .once()
            .in_sequence(sequence)
            .withf(move |remote, display_id, input| {
                remote.command_working_directory.as_deref() == Some(folder.as_path())
                    && display_id == "#42"
                    && input.title == "Refine review metadata sync"
                    && input.body.as_deref() == Some("- Update the linked review request body.")
            })
            .returning(|_, _, input| {
                Box::pin(async move {
                    Ok(forge::ReviewRequestSummary {
                        display_id: "#42".to_string(),
                        forge_kind: forge::ForgeKind::GitHub,
                        source_branch: "wt/session-id".to_string(),
                        state: ReviewRequestState::Open,
                        status_summary: Some("Draft".to_string()),
                        target_branch: "main".to_string(),
                        title: input.title,
                        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                    })
                })
            });

        mock_review_request_client
    }

    /// Returns one GitHub forge remote fixture for worker metadata sync tests.
    fn github_forge_remote() -> forge::ForgeRemote {
        forge::ForgeRemote {
            command_working_directory: None,
            forge_kind: forge::ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Returns one successful turn result with the provided answer text.
    fn successful_turn_result(answer: &str) -> TurnResult {
        TurnResult {
            assistant_message: AgentResponse {
                answer: answer.to_string(),
                questions: Vec::new(),
                summary: None,
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        }
    }

    #[tokio::test]
    /// Verifies completed turns auto-push already-published session branches
    /// in the background and report sync progress through app events.
    async fn test_apply_turn_result_starts_background_push_for_published_branch() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini3FlashPreview);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
            })
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                summary: None,
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
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

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
        assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
        assert_eq!(sync_events[0].0, "sess1");
        assert_eq!(sync_events[1].0, "sess1");
        assert_eq!(sync_events[0].1, sync_events[1].1);
    }

    #[tokio::test]
    /// Verifies completed turns leave auto-push idle while queued follow-up
    /// messages are waiting to run.
    async fn test_apply_turn_result_skips_background_push_while_messages_are_queued() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini3FlashPreview);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::from([TurnPrompt::from_text(
                "queued follow-up".to_string(),
            )]))),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                summary: None,
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let mut emitted_sync_event = false;
        while let Ok(event) = app_event_rx.try_recv() {
            if matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }) {
                emitted_sync_event = true;
            }
        }

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            !emitted_sync_event,
            "queued follow-up messages should suppress post-turn auto-push events"
        );
    }

    #[tokio::test]
    /// Verifies failed background auto-push attempts append a visible error
    /// and keep the session marked as failed for the latest sync attempt.
    async fn test_apply_turn_result_reports_background_push_failures() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini3FlashPreview);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .returning(|_, _| {
                Box::pin(async {
                    Err(crate::infra::git::GitError::CommandFailed {
                        command: "git push origin wt/session-id".to_string(),
                        stderr:
                            "fatal: could not read username for 'https://github.com/openai/agentty': terminal prompts disabled"
                                .to_string(),
                    })
                })
            });
        let output = Arc::new(Mutex::new(String::new()));
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::clone(&output),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                summary: None,
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated { sync_status, .. } = event {
                    sync_events.push(sync_status);
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");
        let output = output.lock().expect("output lock poisoned");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            sync_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Failed,
            ]
        );
        assert!(output.contains("[Branch Push Error]"));
        assert!(output.contains("gh auth login"));
    }

    #[tokio::test]
    /// Verifies failed turn-metadata persistence forces a refresh and skips
    /// reducer projection emission.
    async fn test_apply_turn_result_refreshes_when_turn_metadata_persistence_fails() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .delete_session("sess1")
            .await
            .expect("failed to delete session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                summary: Some(AgentResponseSummary {
                    turn: "- Attempted the update.".to_string(),
                    session: "- Session state should not project without persistence.".to_string(),
                }),
            },
            context_reset: false,
            input_tokens: 2,
            output_tokens: 3,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
        };
        let error = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect_err("turn result should fail when metadata persistence fails");
        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
        let output = context.output.lock().expect("output lock poisoned");

        // Assert
        assert!(
            error
                .to_string()
                .contains("no rows returned by a query that expected to return at least one row")
        );
        assert!(output.contains("Implemented the change."));
        assert!(
            output.contains("[Turn Metadata Error] Failed to persist completed turn metadata:")
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AppEvent::RefreshSessions))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AppEvent::AgentResponseReceived { .. }))
        );
    }

    #[tokio::test]
    /// Verifies persisted assistant text stays unchanged when summaries are
    /// stored only in structured session metadata.
    async fn test_apply_turn_result_keeps_summary_out_of_transcript_output() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let output = Arc::new(Mutex::new("Hey! How can I help you today?".to_string()));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::clone(&output),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Hey! How can I help you today?".to_string(),
                questions: Vec::new(),
                summary: Some(AgentResponseSummary {
                    turn: "No changes".to_string(),
                    session: "No changes".to_string(),
                }),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let output = output.lock().expect("output lock poisoned");

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            output.starts_with("Hey! How can I help you today?Hey! How can I help you today?\n\n")
        );
        assert!(!output.contains("[Commit] No changes to commit."));
        assert!(!output.contains("## Change Summary"));
    }

    #[tokio::test]
    /// Persists the current app-server instruction bootstrap marker after a
    /// successful turn so later follow-ups can reuse the compact reminder.
    async fn test_apply_turn_result_persists_instruction_conversation_id_for_app_server_turns() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess1", "gpt-5.5", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                summary: None,
            },
            context_reset: true,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: Some("thread-123".to_string()),
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                AgentModel::Gpt55,
            ),
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let instruction_conversation_id = db
            .sessions()
            .get_session_instruction_conversation_id("sess1")
            .await
            .expect("failed to load instruction conversation id");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            instruction_conversation_id,
            agent::normalize_instruction_conversation_id(Some("thread-123"))
        );
    }

    /// Test harness for existing-session rebase assistance worker coverage.
    struct RebaseAssistWorkerHarness {
        context: SessionWorkerContext,
        db: AppRepositories,
        output: Arc<Mutex<String>>,
        status: Arc<Mutex<Status>>,
    }

    /// Writes one conflict-marked file used by rebase-assist worker tests.
    fn write_rebase_conflict_file(base_dir: &std::path::Path) {
        let conflict_file = base_dir.join("src/lib.rs");
        std::fs::create_dir_all(
            conflict_file
                .parent()
                .expect("conflict file should have a parent"),
        )
        .expect("failed to create conflict directory");
        std::fs::write(
            conflict_file,
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> incoming\n",
        )
        .expect("failed to write conflict file");
    }

    /// Seeds one rebasing session with existing provider conversation ids.
    async fn seed_existing_session_rebase_metadata(db: &AppRepositories) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess1", "gpt-5.5", "main", "Rebasing", project_id)
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_provider_conversation_id("sess1", Some("thread-before".to_string()))
            .await
            .expect("failed to seed provider conversation id");
        db.sessions()
            .update_session_instruction_conversation_id(
                "sess1",
                Some("instruction-before".to_string()),
            )
            .await
            .expect("failed to seed instruction conversation id");
    }

    /// Builds the mock channel expected for one existing-session rebase turn.
    fn mock_existing_session_rebase_channel() -> MockAgentChannel {
        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .times(1)
            .withf(|session_id, request, _| {
                session_id == "sess1"
                    && request.request_kind == AgentRequestKind::UtilityPrompt
                    && request.provider_conversation_id.as_deref() == Some("thread-before")
                    && request.persisted_instruction_conversation_id.as_deref()
                        == Some("instruction-before")
                    && request
                        .prompt
                        .text
                        .contains("Resolve conflicts in only these files")
                    && request.prompt.text.contains("src/lib.rs")
            })
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "Resolved conflicts inside existing session.".to_string(),
                            questions: Vec::new(),
                            summary: None,
                        },
                        context_reset: false,
                        input_tokens: 11,
                        output_tokens: 7,
                        provider_conversation_id: Some("thread-after".to_string()),
                    })
                })
            });

        mock_channel
    }

    /// Builds the git mock expected for one assisted rebase conflict.
    fn mock_successful_conflict_rebase_git_client() -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
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
            .returning(|_, target| {
                assert_eq!(target, "main");
                Box::pin(async {
                    Ok(RebaseStepResult::Conflict {
                        detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                    })
                })
            });
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, paths| {
                assert!(paths.is_empty());
                Box::pin(async { Ok(Vec::new()) })
            });
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
            .returning(|_, paths| {
                assert_eq!(paths, vec!["src/lib.rs".to_string()]);
                Box::pin(async { Ok(Vec::new()) })
            });
        mock_git_client
            .expect_rebase_continue()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(RebaseStepResult::Completed) }));

        mock_git_client
    }

    /// Builds one worker harness for session rebase command tests.
    fn rebase_assist_worker_harness(
        base_dir: PathBuf,
        db: AppRepositories,
    ) -> RebaseAssistWorkerHarness {
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new(Status::Rebasing));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_existing_session_rebase_channel()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir,
            fs_client: Arc::new(fs::RealFsClient),
            git_client: Arc::new(mock_successful_conflict_rebase_git_client()),
            output: Arc::clone(&output),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                AgentModel::Gpt55,
            ),
            status: Arc::clone(&status),
        };

        RebaseAssistWorkerHarness {
            context,
            db,
            output,
            status,
        }
    }

    #[tokio::test]
    /// Verifies session rebase conflict assistance runs through the existing
    /// session channel, preserving provider conversation identifiers while
    /// Agentty owns staging and `git rebase --continue`.
    async fn test_run_rebase_command_uses_existing_session_channel_for_conflicts() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        write_rebase_conflict_file(base_dir.path());
        let db = AppRepositories::in_memory().await;
        seed_existing_session_rebase_metadata(&db).await;
        let harness = rebase_assist_worker_harness(base_dir.path().to_path_buf(), db);

        // Act
        SessionWorkerService::run_rebase_command(&harness.context, "main".to_string())
            .await
            .expect("rebase command should complete");
        let provider_conversation_id = harness
            .db
            .sessions()
            .get_session_provider_conversation_id("sess1")
            .await
            .expect("failed to load provider conversation id");
        let instruction_conversation_id = harness
            .db
            .sessions()
            .get_session_instruction_conversation_id("sess1")
            .await
            .expect("failed to load instruction conversation id");
        let output_text = harness.output.lock().expect("output lock").clone();
        let final_status = *harness.status.lock().expect("status lock");

        // Assert
        assert_eq!(provider_conversation_id.as_deref(), Some("thread-after"));
        assert_eq!(
            instruction_conversation_id,
            agent::normalize_instruction_conversation_id(Some("thread-after"))
        );
        assert_eq!(final_status, Status::Review);
        assert!(output_text.contains("[Sync Assist] Attempt 1/3. Resolving conflicts in:"));
        assert!(output_text.contains("- src/lib.rs"));
        assert!(output_text.contains("Resolved conflicts inside existing session."));
        assert!(output_text.contains("[Sync] Successfully synced wt/sess1 onto main"));
    }

    #[tokio::test]
    /// Verifies restart recovery marks unfinished operations failed and
    /// restores affected sessions to `Review`.
    async fn test_fail_unfinished_operations_from_previous_run_restores_session_review_status() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_status_with_timing_at("sess1", "InProgress", 0)
            .await
            .expect("failed to open in-progress timing window");
        db.operations()
            .insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_is_rebase_in_progress().times(0);
        mock_git_client.expect_abort_rebase().times(0);

        // Act
        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await;
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");
        let operation_is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, "Review");
        assert_eq!(sessions[0].in_progress_started_at, None);
        assert_eq!(sessions[0].in_progress_total_seconds, 300);
        assert!(!operation_is_unfinished);
    }

    #[tokio::test]
    /// Verifies restart recovery aborts stale rebase metadata for interrupted
    /// rebase operations before restoring review state.
    async fn test_fail_unfinished_operations_from_previous_run_aborts_interrupted_rebase() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "Rebasing",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", "rebase")
            .await
            .expect("failed to insert session operation");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_rebase_in_progress()
            .once()
            .withf(|repo_path| repo_path.ends_with("sess1"))
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_abort_rebase()
            .once()
            .withf(|repo_path| repo_path.ends_with("sess1"))
            .returning(|_| Box::pin(async { Ok(()) }));

        // Act
        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await;
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(sessions[0].status, "Review");
    }

    #[tokio::test]
    /// Verifies unfinished operations remain executable when cancel has not
    /// been requested.
    async fn test_should_skip_worker_command_without_cancel_request() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(!should_skip);
        assert!(is_unfinished);
    }

    #[tokio::test]
    /// Verifies cancel requests skip queued operations before execution and
    /// mark them canceled.
    async fn test_should_skip_worker_command_when_cancel_is_requested() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");
        db.operations()
            .request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(should_skip);
        assert!(!is_unfinished);
    }

    #[tokio::test]
    /// Verifies a new operation created after a session-level cancel request
    /// is not skipped. The operation-scoped check ensures stale cancel flags
    /// on older operations do not block newly enqueued work.
    async fn test_should_skip_worker_command_allows_new_operation_after_cancel() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        // Old operation that gets cancelled.
        db.operations()
            .insert_session_operation("op-old", "sess1", "reply")
            .await
            .expect("failed to insert old operation");
        db.operations()
            .mark_session_operation_running("op-old")
            .await
            .expect("failed to mark old operation running");
        db.operations()
            .request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");

        // New operation created after the cancel request — its
        // `cancel_requested` defaults to 0.
        db.operations()
            .insert_session_operation("op-new", "sess1", "reply")
            .await
            .expect("failed to insert new operation");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act — the new operation should proceed despite the old
        // cancelled operation still being in 'running' state.
        let should_skip =
            SessionWorkerService::should_skip_worker_command(&context, "op-new").await;

        // Assert
        assert!(
            !should_skip,
            "new operation should not be skipped by stale cancel on older operation"
        );
    }

    /// Builds one [`SessionWorkerContext`] backed by the supplied
    /// [`MockAgentChannel`], a fresh in-memory database, and the queued
    /// prompt list. The session row is pre-inserted as `InProgress` so the
    /// worker reaches drainage without first transitioning status.
    async fn queue_test_context(
        channel: MockAgentChannel,
        queued_messages: VecDeque<TurnPrompt>,
        status: Status,
    ) -> (
        SessionWorkerContext,
        AppRepositories,
        Arc<Mutex<VecDeque<TurnPrompt>>>,
        tempfile::TempDir,
    ) {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        let main_repo_root = base_dir.path().join("main");
        mock_git_client
            .expect_detect_git_info()
            .times(0..)
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_repo_root()
            .times(0..)
            .returning(move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(main_repo_root) })
            });
        mock_git_client
            .expect_tracked_worktree_status()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let queue_handle = Arc::new(Mutex::new(queued_messages));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: Arc::clone(&queue_handle),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(status)),
        };

        (context, db, queue_handle, base_dir)
    }

    /// Builds one [`SessionWorkerContext`] whose only meaningful state is the
    /// shared `queued_messages` mutex; every other field is wired with a stub
    /// value because these tests only exercise the queue helpers.
    async fn queue_helper_context(queue: Arc<Mutex<VecDeque<TurnPrompt>>>) -> SessionWorkerContext {
        SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await,
            folder: PathBuf::new(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            queued_messages: queue,
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini3FlashPreview,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        }
    }

    #[tokio::test]
    async fn test_pop_queued_prompt_returns_messages_in_submission_order() {
        // Arrange
        let queue: Arc<Mutex<VecDeque<TurnPrompt>>> = Arc::new(Mutex::new(VecDeque::from([
            TurnPrompt::from_text("first".to_string()),
            TurnPrompt::from_text("second".to_string()),
        ])));
        let context = queue_helper_context(Arc::clone(&queue)).await;

        // Act
        let first_pop = context.pop_queued_prompt();
        let second_pop = context.pop_queued_prompt();
        let empty_pop = context.pop_queued_prompt();

        // Assert
        assert_eq!(first_pop.expect("first prompt").text, "first");
        assert_eq!(second_pop.expect("second prompt").text, "second");
        assert!(empty_pop.is_none());
        assert!(queue.lock().expect("queue lock").is_empty());
    }

    #[tokio::test]
    async fn test_clear_queued_messages_drops_all_pending_prompts() {
        // Arrange
        let queue: Arc<Mutex<VecDeque<TurnPrompt>>> = Arc::new(Mutex::new(VecDeque::from([
            TurnPrompt::from_text("alpha".to_string()),
            TurnPrompt::from_text("beta".to_string()),
        ])));
        let context = queue_helper_context(Arc::clone(&queue)).await;

        // Act
        context.clear_queued_messages();

        // Assert
        assert!(queue.lock().expect("queue lock").is_empty());
    }

    #[tokio::test]
    async fn test_clear_queued_messages_updates_shared_queue_state() {
        // Arrange
        let queue: Arc<Mutex<VecDeque<TurnPrompt>>> =
            Arc::new(Mutex::new(VecDeque::from([TurnPrompt::from_text(
                "queued reply".to_string(),
            )])));
        let context = queue_helper_context(Arc::clone(&queue)).await;

        // Act
        let has_queued_before_clear = !queue.lock().expect("queue lock").is_empty();
        context.clear_queued_messages();
        let has_queued_after_clear = !queue.lock().expect("queue lock").is_empty();

        // Assert
        assert!(has_queued_before_clear);
        assert!(!has_queued_after_clear);
    }

    #[tokio::test]
    /// Verifies that drainage holds while the session is in `Question` state
    /// so queued prompts wait for the clarification flow to resolve before
    /// dispatching as new turns.
    async fn test_drain_queued_messages_pauses_while_status_is_question() {
        // Arrange
        let mut mock_channel = MockAgentChannel::new();
        mock_channel.expect_run_turn().never().returning(|_, _, _| {
            Box::pin(async { unreachable!("drain must not dispatch while status is Question") })
        });
        let queued = VecDeque::from([TurnPrompt::from_text("queued reply".to_string())]);
        let (context, _db, queue_handle, _base_dir) =
            queue_test_context(mock_channel, queued, Status::Question).await;

        // Act
        SessionWorkerService::drain_queued_messages(&context).await;

        // Assert — queued prompt remains untouched until status becomes
        // runnable again.
        let queue = queue_handle.lock().expect("queue lock");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.front().expect("queued head").text, "queued reply");
    }

    #[tokio::test]
    /// Verifies the last queued follow-up turn reloads the persisted
    /// published branch and starts auto-push after the queue has drained.
    async fn test_drain_queued_messages_auto_pushes_after_last_published_branch_follow_up() {
        // Arrange
        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .times(1)
            .withf(|session_id, request, _events| {
                session_id == "sess1" && request.prompt.text == "queued reply"
            })
            .returning(|_, _, _| Box::pin(async { Ok(successful_turn_result("Queued done.")) }));
        let queued = VecDeque::from([TurnPrompt::from_text("queued reply".to_string())]);
        let (mut context, db, queue_handle, base_dir) =
            queue_test_context(mock_channel, queued, Status::InProgress).await;
        db.sessions()
            .update_session_published_upstream_ref(
                "sess1",
                Some("origin/wt/session-id".to_string()),
            )
            .await
            .expect("failed to persist published upstream ref");

        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;

        let mut mock_git_client = MockGitClient::new();
        let main_repo_root = base_dir.path().join("main");
        mock_git_client
            .expect_detect_git_info()
            .times(1)
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client.expect_main_repo_root().times(1).returning({
            let main_repo_root = main_repo_root.clone();

            move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(main_repo_root) })
            }
        });
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(1)
            .withf(|_folder, remote_branch_name| remote_branch_name == "wt/session-id")
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        context.git_client = Arc::new(mock_git_client);

        // Act
        SessionWorkerService::drain_queued_messages(&context).await;
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
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

        // Assert
        assert!(queue_handle.lock().expect("queue lock").is_empty());
        assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
        assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
        assert_eq!(sync_events[0].0, "sess1");
        assert_eq!(sync_events[1].0, "sess1");
        assert_eq!(sync_events[0].1, sync_events[1].1);
    }

    #[tokio::test]
    /// Verifies that drainage stops and clears every queued prompt once the
    /// running queued turn returns `StoppedByUser`, matching the `Ctrl+C`
    /// expectation that cancellation drops pending follow-ups together with
    /// the active turn.
    async fn test_drain_queued_messages_clears_queue_when_user_stops_running_turn() {
        // Arrange
        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .times(1)
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(AgentError::InterruptedByUser(
                        "[Stopped] Session interrupted by user.".to_string(),
                    ))
                })
            });
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        let queued = VecDeque::from([
            TurnPrompt::from_text("queued first".to_string()),
            TurnPrompt::from_text("queued second".to_string()),
        ]);
        let (context, _db, queue_handle, _base_dir) =
            queue_test_context(mock_channel, queued, Status::InProgress).await;

        // Act
        SessionWorkerService::drain_queued_messages(&context).await;

        // Assert — first prompt was dispatched, the StoppedByUser result
        // propagated, and the remaining queued prompt was cleared.
        let queue = queue_handle.lock().expect("queue lock");
        assert!(queue.is_empty(), "queue should be cleared on Ctrl+C");
    }
}
