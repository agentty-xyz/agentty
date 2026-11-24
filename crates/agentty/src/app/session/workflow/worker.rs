//! Application host policy and ordered effects for worker-owned session
//! execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use ag_contracts::{
    AgentError, AgentRequestKind, SessionCommandClient, TurnContinuation, TurnEvent, TurnRequest,
    TurnResult,
};
use ag_forge as forge;
use ag_git::{CommitHookRunner, GitClient};
use ag_protocol::AgentResponse;
use ag_telemetry::{FutureExt as _, KeyValue, Outcome, QueuedTrace};
use ag_worker::{RunClient, SessionRunClient};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::base_branch::{BaseBranchScope, BaseBranchSnapshot};
use super::commit_hook::{SandboxedCommitHookRunner, SessionHookTarget};
use super::merge::{
    ExistingSessionRebaseAssistClient, RebaseAssistFuture, RebaseAssistMode, RebaseCommandInput,
};
use super::sandbox::SandboxNoticeTarget;
use super::steering::{QueuedMessageSteering, SteeringDeliveries};
use super::task::SessionTranscriptMessageAppend;
use super::{SessionTaskService, graph_turn, isolation, session_folder, task_turn, turn};
use crate::analytics::{TurnOutcome, TurnSelection};
use crate::app::branch_publish::{
    BranchPublishTaskContext, BranchPublishTaskSession, review_request_from_publish_result,
    run_branch_publish_action,
};
use crate::app::cargo_cache::CargoCacheWarmer;
use crate::app::graph_activity::{GRAPH_ACTIVITY_OPERATION_KIND, GraphActivityRegistry};
use crate::app::service::{HarnessModels, SessionUpdateVersionMap};
use crate::app::session::{Clock, SessionError, unix_timestamp_from_system_time};
use crate::app::{AppEvent, AppServices, SessionManager, spawn};
use crate::domain::agent::{AgentKind, AgentSelection};
use crate::domain::session::{
    PublishBranchAction, QueuedMessage, ReviewRequest, SessionHandles, SessionId, Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::session_usage::LiveUsageRun;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::{GeneratedTurnLabels, GeneratedTurnUpdate};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{
    AppRepositories, GRAPH_NODE_ACTIVITY_OPERATION_KIND, OperationRepository, SessionOperationRow,
};
use crate::infra::fs::FsClient;

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";
const CREATE_REVIEW_REQUEST_OPERATION_KIND: &str = "create_review_request";
const REBASE_OPERATION_KIND: &str = "rebase";
/// Persisted status of an operation accepted by the worker but not started.
const QUEUED_OPERATION_STATUS: &str = "queued";
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
    /// Labels for an Agentty-generated turn. When set, the worker records
    /// the prompt as hidden agent context when execution starts, because the
    /// host never rendered it as a user prompt, and reports the turn's
    /// progress through [`AppEvent::SessionGeneratedTurnUpdated`].
    pub(super) generated_turn: Option<GeneratedTurnLabels>,
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
    /// Runs an already admitted graph task and retains its settlement reply.
    RunGraphTask(Box<graph_turn::GraphTaskCommand>),
    /// Runs a role-bound task through its independently owned runtime.
    RunAgentTask {
        cancellation: Arc<CancellationToken>,
        operation_id: String,
        request: Box<ag_session::SubmitAgentTaskRequest>,
        factory: Arc<dyn crate::app::service::SessionRunFactory>,
    },
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

    /// Returns the labels of an Agentty-generated turn carried by this
    /// command.
    pub(super) fn generated_turn(&self) -> Option<&GeneratedTurnLabels> {
        match self {
            Self::Run { turn_metadata, .. } => turn_metadata.generated_turn.as_ref(),
            _ => None,
        }
    }

    /// Returns the selected thread count carried through this command's turn.
    pub(super) fn review_comment_count(&self) -> usize {
        match self {
            Self::Run { turn_metadata, .. } => turn_metadata.review_comment_thread_ids.len(),
            _ => 0,
        }
    }

    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::RunGraphTask(task) => &task.operation_id,
            Self::CreateReviewRequest { operation_id, .. }
            | Self::Rebase { operation_id, .. }
            | Self::Run { operation_id, .. }
            | Self::RunAgentTask { operation_id, .. } => operation_id,
        }
    }

    /// Returns the captured selection for a model turn's live accounting.
    fn turn_agent(&self) -> Option<AgentSelection> {
        match self {
            Self::RunGraphTask(task) => Some(task.request.dispatch.node.agent),
            Self::Run {
                request_kind: AgentRequestKind::SessionStart | AgentRequestKind::SessionResume,
                turn_metadata,
                ..
            } => Some(turn_metadata.session_agent),
            Self::RunAgentTask { request, .. } => Some(request.assignment.agent),
            _ => None,
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::RunGraphTask(_) => "graph_node",
            Self::RunAgentTask { .. } => "agent_task",
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
                request_kind: AgentRequestKind::GraphProposal,
                ..
            } => "graph_proposal",
            Self::Run {
                request_kind: AgentRequestKind::ReviewMetadata,
                ..
            } => "review_metadata",
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
pub(super) struct ScheduledSessionCommand {
    command: SessionCommand,
    trace: QueuedTrace,
    /// Reserves workspace preparation until this command is dropped or
    /// finishes.
    preparation_reservation: Option<Arc<()>>,
    queued_order: Option<u64>,
    /// First turns wait until the foreground has persisted their operation
    /// and published their initial metadata.
    ready_rx: Option<oneshot::Receiver<()>>,
}

impl ScheduledSessionCommand {
    fn trace(command: &SessionCommand) -> QueuedTrace {
        let name = match command {
            SessionCommand::Run { .. }
            | SessionCommand::RunAgentTask { .. }
            | SessionCommand::RunGraphTask(_) => "session.turn",
            SessionCommand::Rebase { .. } => "session.sync",
            SessionCommand::CreateReviewRequest { .. } => "session.publish",
        };

        let trace = QueuedTrace::new(
            name,
            vec![
                KeyValue::new("agentty.operation.id", command.operation_id().to_string()),
                KeyValue::new("agentty.purpose", command.kind()),
            ],
        );
        if matches!(command, SessionCommand::Run { .. }) {
            trace.attribute("agentty.turn.id", command.operation_id().to_string());
        }

        trace
    }

    /// Returns whether this command can make a questioned session runnable.
    fn can_run_while_question(&self) -> bool {
        if let SessionCommand::RunGraphTask(task) = &self.command {
            return task.cancellation.is_cancelled();
        }
        if let SessionCommand::RunAgentTask { cancellation, .. } = &self.command {
            return cancellation.is_cancelled();
        }
        self.command.review_comment_count() == 0
            && (self.queued_order.is_none() || matches!(self.command, SessionCommand::Run { .. }))
    }

    /// Wraps a command that is ready to run without joining the visible queue.
    fn immediate(command: SessionCommand) -> Self {
        Self {
            trace: Self::trace(&command),
            command,
            preparation_reservation: None,
            queued_order: None,
            ready_rx: None,
        }
    }

    /// Wraps a command queued at the supplied shared submission order.
    fn queued(command: SessionCommand, queued_order: u64) -> Self {
        Self {
            trace: Self::trace(&command),
            command,
            preparation_reservation: None,
            queued_order: Some(queued_order),
            ready_rx: None,
        }
    }
}

impl ag_worker::ScheduledCommand for ScheduledSessionCommand {
    fn order(&self) -> Option<u64> {
        self.queued_order
    }

    fn can_run_while_paused(&self) -> bool {
        self.can_run_while_question()
    }
}

/// Worker-owned mailbox for this application's scheduled commands.
type SessionWorkerHandle = ag_worker::SessionWorkerHandle<ScheduledSessionCommand>;

/// Next unit selected from the combined action and chat queues.
type ScheduledSessionWork = ag_worker::ScheduledWork<ScheduledSessionCommand, QueuedMessage>;

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

/// Returns whether the session has accepted, not-yet-started worker work,
/// such as a queued review-comment reply, that will run after the current
/// item and therefore owns the next automatic publish.
///
/// Cancel-requested rows are ignored because the worker skips them without
/// running their post-turn effects.
pub(super) fn has_queued_session_operation(
    operations: &[SessionOperationRow],
    session_id: &str,
) -> bool {
    operations.iter().any(|operation| {
        operation.session_id == session_id
            && operation.status == QUEUED_OPERATION_STATUS
            && !operation.cancel_requested
    })
}

type SessionWorkFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Result<(), SessionError>>> + Send + 'a>>;

struct SessionWorkerHost {
    context: SessionWorkerContext,
    message_traces: Arc<Mutex<HashMap<(SessionId, u64), QueuedTrace>>>,
    run_client: Arc<dyn RunClient>,
    graph_resources: Arc<ag_worker::GraphResources>,
    graph_wake: tokio::sync::watch::Sender<u64>,
    /// Shared with [`SessionHandles::steerable_turn`]; set while this worker
    /// runs a chat turn.
    steerable_turn: Arc<AtomicBool>,
    /// Deliveries of queued prompts into this worker's chat turns, settled
    /// before the worker selects more work.
    steering_deliveries: Arc<SteeringDeliveries>,
}

impl SessionWorkerHost {
    /// Returns whether `work` is a chat turn on the session's own
    /// conversation, the only turn queued prompts can be steered into.
    fn is_chat_turn(work: &ScheduledSessionWork) -> bool {
        matches!(
            work,
            ScheduledSessionWork::Message(_)
                | ScheduledSessionWork::Command(ScheduledSessionCommand {
                    command: SessionCommand::Run {
                        request_kind: AgentRequestKind::SessionStart
                            | AgentRequestKind::SessionResume,
                        ..
                    },
                    ..
                })
        )
    }

    /// Ends the traces of this session's prompts that left the queue.
    fn release_unqueued_message_traces(&self) {
        let queued_messages = self
            .context
            .queued_messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let orders: HashSet<_> = queued_messages.iter().map(QueuedMessage::order).collect();
        self.message_traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(session_id, order), _| {
                session_id != &self.context.session_id || orders.contains(order)
            });
    }
}

/// Marks the worker's chat turn as running until dropped, including when the
/// worker is torn down mid-turn.
struct SteerableTurn<'a> {
    context: &'a SessionWorkerContext,
    is_running: &'a AtomicBool,
}

impl<'a> SteerableTurn<'a> {
    /// Publishes that queued prompts can now join the running turn.
    fn begin(context: &'a SessionWorkerContext, is_running: &'a AtomicBool) -> Self {
        is_running.store(true, Ordering::Release);
        SessionWorkerService::emit_queue_session_updated(context);

        Self {
            context,
            is_running,
        }
    }
}

impl Drop for SteerableTurn<'_> {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Release);
        SessionWorkerService::emit_queue_session_updated(self.context);
    }
}

impl Drop for SessionWorkerHost {
    fn drop(&mut self) {
        self.message_traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(session_id, _), _| session_id != &self.context.session_id);
    }
}

impl ag_worker::WorkQueue for SessionWorkerHost {
    type Command = ScheduledSessionCommand;
    type Message = QueuedMessage;

    fn paused(&self) -> bool {
        ag_worker::WorkQueue::paused(&self.context)
    }

    fn message_order(&self) -> Option<u64> {
        self.context.next_queued_message_order()
    }

    fn pop_message(&self) -> Option<QueuedMessage> {
        self.context.pop_queued_message()
    }
}

#[async_trait::async_trait]
impl ag_worker::WorkerHost for SessionWorkerHost {
    async fn execute(&self, work: ScheduledSessionWork) {
        let environment = self
            .graph_resources
            .environment(&ag_session::EnvId::SessionWorktree(
                self.context.session_id.clone(),
            ));
        // Graph turns publish active ownership before acquiring their leases;
        // completion retains those leases through operation settlement.
        let writer = if matches!(
            &work,
            ScheduledSessionWork::Command(ScheduledSessionCommand {
                command: SessionCommand::RunGraphTask(_),
                ..
            })
        ) {
            None
        } else {
            Some(environment.write_owned().await)
        };
        let _environment_lease = crate::app::execution_graph::EnvironmentLease::from_writer(
            writer,
            self.graph_wake.clone(),
        );
        let is_turn = matches!(
            &work,
            ScheduledSessionWork::Message(_)
                | ScheduledSessionWork::Command(ScheduledSessionCommand {
                    command: SessionCommand::Run { .. }
                        | SessionCommand::RunAgentTask { .. }
                        | SessionCommand::RunGraphTask(_),
                    ..
                })
        );
        let is_chat_turn = Self::is_chat_turn(&work);
        let (trace, operation, _reservation): (
            QueuedTrace,
            SessionWorkFuture<'_>,
            Option<Arc<()>>,
        ) = match work {
            ScheduledSessionWork::Command(mut command) => {
                let reservation = command.preparation_reservation.take();
                if let Some(ready_rx) = command.ready_rx.take()
                    && ready_rx.await.is_err()
                {
                    return;
                }
                (
                    command.trace,
                    Box::pin(SessionWorkerService::process_session_command(
                        &self.context,
                        &self.run_client,
                        command.command,
                    )),
                    reservation,
                )
            }
            ScheduledSessionWork::Message(message) => {
                let trace = self
                    .message_traces
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&(self.context.session_id.clone(), message.order()))
                    .unwrap_or_else(|| QueuedTrace::new("session.turn", Vec::new()));
                (
                    trace,
                    Box::pin(SessionWorkerService::process_queued_message(
                        &self.context,
                        &self.run_client,
                        message,
                    )),
                    None,
                )
            }
        };
        let steerable_turn =
            is_chat_turn.then(|| SteerableTurn::begin(&self.context, self.steerable_turn.as_ref()));
        let mut span = trace.selected();
        span.identify("agentty.session.id", self.context.session_id.to_string());
        let result = operation.with_context(span.context()).await;
        drop(steerable_turn);
        // A delivery taken for this turn settles here, so it cannot join the
        // next turn, and a prompt it returns keeps its place in the queue.
        self.steering_deliveries.settled().await;
        let interrupted = is_turn
            && self
                .context
                .cancel_token
                .lock()
                .is_ok_and(|token| token.is_cancelled());
        let outcome = match &result {
            _ if interrupted => Outcome::Canceled,
            Some(Ok(())) => Outcome::Completed,
            Some(Err(SessionError::StoppedByUser(_))) | None => Outcome::Canceled,
            Some(Err(_)) => Outcome::Failed,
        };
        span.finish(outcome);
        SessionWorkerService::clear_queued_messages_after_stop(&self.context, result.as_ref());
        self.release_unqueued_message_traces();
    }

    async fn abandon(&self, work: ScheduledSessionWork) {
        match work {
            ScheduledSessionWork::Command(command) => {
                if let Err(error) = self
                    .context
                    .db
                    .operations()
                    .mark_session_operation_canceled(
                        command.command.operation_id(),
                        "Session worker closed before execution",
                    )
                    .await
                {
                    tracing::warn!(%error, "Failed to record abandoned worker command");
                }
                SessionWorkerService::complete_skipped_session_command(
                    &self.context,
                    &command.command,
                );
            }
            ScheduledSessionWork::Message(message) => {
                self.message_traces
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&(self.context.session_id.clone(), message.order()));
                SessionWorkerService::emit_queue_session_updated(&self.context);
            }
        }
    }

    async fn shutdown(&self) {
        self.message_traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(session_id, _), _| session_id != &self.context.session_id);
        let _ = self.context.session_run.shutdown().await;
        if let Ok(mut guard) = self.context.child_pid.lock() {
            *guard = None;
        }
    }
}

/// Shared state threaded through all worker turn executions.
pub(super) struct SessionWorkerContext {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Locally installed agent CLIs that title and auto-commit utility
    /// prompts may target.
    pub(super) available_agent_kinds: Arc<[AgentKind]>,
    /// Serializes post-turn publish ownership with queued branch operations.
    pub(super) branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Per-turn cancellation token shared with the UI through
    /// [`SessionHandles`]. The worker swaps in a fresh token at the start
    /// of each turn; the UI calls `cancel()` on the current token to
    /// interrupt a running turn.
    pub(super) cancel_token: Arc<Mutex<CancellationToken>>,
    /// Starts refreshes of the read-only Cargo cache Harness turns reuse.
    pub(super) cargo_cache: CargoCacheWarmer,
    /// Worker-owned execution client for this session.
    pub(super) session_run: SessionRunClient,
    /// Runtime accounting root; adapters retain their own cancellation
    /// ownership.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) db: AppRepositories,
    pub(super) folder: PathBuf,
    pub(super) fs_client: Arc<dyn FsClient>,
    pub(super) git_client: Arc<dyn GitClient>,
    /// Graph leases that hold the session `InProgress` across turn ends.
    pub(super) graph_activity: GraphActivityRegistry,
    /// Harness models a Fast default naming one needs ready before title
    /// and auto-commit utility prompts use it.
    pub(super) harness_models: HarnessModels,
    /// In-memory queue of prompts staged while the session is `InProgress`.
    ///
    /// Shared with [`SessionHandles::queued_messages`]. The worker drains
    /// this queue between turns; the lifecycle pushes new entries when a
    /// user submits a chat message during a running turn.
    pub(super) queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Agent provider and model selected for this session.
    pub(super) session_agent: AgentSelection,
    /// Runs host commands, such as commit hooks, in the session sandbox.
    pub(super) session_commands: Arc<dyn SessionCommandClient>,
    pub(super) session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    pub(super) status: Arc<Mutex<Status>>,
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

impl SessionWorkerContext {
    /// Returns the runner of the session worktree's commit hooks inside the
    /// session sandbox, reporting its sandbox state to the transcript.
    pub(super) fn commit_hooks(&self) -> Arc<dyn CommitHookRunner> {
        Arc::new(SandboxedCommitHookRunner::session(
            Arc::clone(&self.session_commands),
            SessionHookTarget {
                app_event_tx: self.app_event_tx.clone(),
                db: self.db.clone(),
                session_id: self.session_id.clone(),
                session_update_versions: Arc::clone(&self.session_update_versions),
                transcript: Arc::clone(&self.transcript),
            },
        ))
    }

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
    pub(super) async fn load_published_upstream_ref(&self) -> Option<String> {
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

impl ag_worker::WorkQueue for SessionWorkerContext {
    type Command = ScheduledSessionCommand;
    type Message = QueuedMessage;

    fn paused(&self) -> bool {
        self.current_status() == Status::Question
    }

    fn message_order(&self) -> Option<u64> {
        self.next_queued_message_order()
    }

    fn pop_message(&self) -> Option<QueuedMessage> {
        self.pop_queued_message()
    }
}

/// Existing-session rebase assistance backed by the worker's active channel.
#[derive(Clone)]
struct SessionWorkerRebaseAssistClient {
    /// Reducer event sender used for transient progress and output updates.
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Per-turn cancellation token shared with the UI.
    cancel_token: Arc<Mutex<CancellationToken>>,
    /// Worker execution client already associated with this session.
    session_run: SessionRunClient,
    /// Runtime accounting root; app-server cancellation uses channel shutdown.
    child_pid: Arc<Mutex<Option<u32>>>,
    /// Clock that stamps the assist turn's live timeline.
    clock: Arc<dyn Clock>,
    /// Repository bundle used for conversation and usage persistence.
    db: AppRepositories,
    /// Session worktree folder where the utility prompt runs.
    folder: PathBuf,
    /// Git boundary that checks the session's base branch around Harness
    /// assist turns.
    git_client: Arc<dyn GitClient>,
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
            session_run: context.session_run.clone(),
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
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
        let turn_cancel_token = self.turn_cancel_token()?;
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
            // Assist turns resolve conflicts in a few short turns.
            context_compaction: ag_contracts::ContextCompaction::Off,
            provider_call_budget: None,
            execution_policy: ag_contracts::ExecutionPolicy::default(),
            continuation: TurnContinuation::provider(
                Some(turn::live_transcript_source(&self.transcript)),
                persisted_instruction_conversation_id,
                provider_conversation_id,
                None,
            ),
            folder: self.folder.clone(),
            main_checkout_root: self.main_checkout_root.clone(),
            model: self.session_agent.model().provider_model_str().to_string(),
            permission_mode: ag_contracts::PermissionMode::AutoEdit,
            agent_profile: ag_contracts::AgentDefinitionPrompt::default(),
            prompt: TurnPrompt::from_agent_data(prompt),
            reasoning_level,
            request_kind: AgentRequestKind::UtilityPrompt,
            response_style: ag_contracts::ResponseStyle::default(),
            speed_mode,
        };
        let (event_tx, relayed_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let (relay_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let live_usage = turn::LiveUsage::new(
            self.app_event_tx.clone(),
            self.session_id.clone(),
            LiveUsageRun::Rebase,
        );
        let consumer = spawn::spawn_detached(turn::consume_turn_events(
            event_rx,
            self.app_event_tx.clone(),
            self.session_id.clone(),
            Arc::clone(&self.child_pid),
            Arc::clone(&live_usage),
            Arc::clone(&self.clock),
        ));
        let sandbox_notices = SandboxNoticeTarget {
            app_event_tx: &self.app_event_tx,
            db: &self.db,
            session_id: &self.session_id,
            session_update_versions: &self.session_update_versions,
            transcript: &self.transcript,
        };

        let base_branch_scope = BaseBranchScope {
            db: &self.db,
            folder: &self.folder,
            git_client: self.git_client.as_ref(),
            session_id: &self.session_id,
        };
        let base_branch =
            BaseBranchSnapshot::capture(&base_branch_scope, self.session_agent.kind()).await?;

        let (turn_result, ()) = tokio::join!(
            self.run_turn_with_cancellation(turn_cancel_token, req, event_tx),
            sandbox_notices.relay(relayed_rx, relay_tx),
        );
        let activity = consumer.await.unwrap_or_default();
        activity.clear_live(&self.app_event_tx, &self.session_id);
        let turn_result =
            BaseBranchSnapshot::verify(base_branch.as_ref(), &base_branch_scope, turn_result)
                .await
                .map_err(turn::session_error_from_agent_error)?;

        self.append_assist_answer(&turn_result.assistant_message)
            .await;
        // `live_usage` ends when this returns: after the assist's usage is
        // stored, or earlier without it.
        self.persist_assist_turn_metadata(&turn_result).await?;

        Ok(())
    }

    /// Reuses the enclosing rebase operation's token across every child turn.
    fn turn_cancel_token(&self) -> Result<CancellationToken, SessionError> {
        let guard = self
            .cancel_token
            .lock()
            .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))?;

        Ok(guard.clone())
    }

    /// Runs the provider turn while honoring the shared cancellation token.
    async fn run_turn_with_cancellation(
        &self,
        cancel_token: CancellationToken,
        req: TurnRequest,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
    ) -> Result<TurnResult, AgentError> {
        let result = self.session_run.submit(req, event_tx, cancel_token).await;
        if matches!(result, Err(AgentError::InterruptedByUser(_)))
            && let Ok(mut pid) = self.child_pid.lock()
        {
            *pid = None;
        }
        result
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
        if let Err(error) = self
            .db
            .usage()
            .upsert_session_usage(
                &self.session_id,
                ag_contracts::UsagePurpose::Rebase,
                &turn_result.usage,
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
        if ag_worker::uses_persistent_session(self.session_agent.kind()) {
            // This assist bootstraps a utility profile, so the next interactive
            // turn must receive the full session contract again.
            self.db
                .sessions()
                .update_session_instruction_conversation_id(&self.session_id, None)
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
    queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    queued_work_sequence: Arc<AtomicU64>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Agent provider and model selected for this session.
    session_agent: AgentSelection,
    session_id: SessionId,
    /// Per-app session update versions shared with the main runtime.
    session_update_versions: SessionUpdateVersionMap,
    status: Arc<Mutex<Status>>,
    /// Shared with [`SessionHandles::steerable_turn`].
    steerable_turn: Arc<AtomicBool>,
    transcript: Arc<Mutex<SessionTranscript>>,
}

/// Foreground access to the running turns of one live session worker.
struct LiveSessionRun {
    session_run: SessionRunClient,
    /// Shared with the worker host, which settles deliveries after each turn.
    steering_deliveries: Arc<SteeringDeliveries>,
}

/// Owns per-session worker queue senders and preparation reservations.
pub(crate) struct SessionWorkerService {
    message_traces: Arc<Mutex<HashMap<(SessionId, u64), QueuedTrace>>>,
    preparation_reservations: HashMap<SessionId, Weak<()>>,
    /// Execution handles owned by each worker in [`Self::workers`], retained
    /// so the foreground can steer their running turns.
    session_runs: HashMap<SessionId, LiveSessionRun>,
    task_cancellations: HashMap<String, (SessionId, Weak<CancellationToken>)>,
    workers: HashMap<SessionId, SessionWorkerHandle>,
}

impl SessionWorkerService {
    /// Creates an empty worker service with no active session workers.
    pub(in crate::app::session) fn new() -> Self {
        Self {
            message_traces: Arc::new(Mutex::new(HashMap::new())),
            preparation_reservations: HashMap::new(),
            session_runs: HashMap::new(),
            task_cancellations: HashMap::new(),
            workers: HashMap::new(),
        }
    }

    pub(super) fn trace_queued_message(&self, session_id: &str, order: u64) {
        let trace = QueuedTrace::new(
            "session.turn",
            vec![KeyValue::new("agentty.session.id", session_id.to_string())],
        );
        self.message_traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((SessionId::from(session_id), order), trace);
    }

    /// Ends a retracted message's waiting interval and canceled workflow now.
    pub(super) fn discard_message_trace(&self, session_id: &str, order: u64) {
        self.message_traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(SessionId::from(session_id), order));
    }

    /// Returns whether saved preparation or a mutating task owns branch work.
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
        let mutating_task = matches!(&command.command, SessionCommand::RunAgentTask { request, .. } if request.assignment.role == ag_session::TaskAgentRole::Implementer)
            || matches!(&command.command, SessionCommand::RunGraphTask(task) if task.request.dispatch.node.execution.workspace_access == ag_session::GraphWorkspaceAccess::ExclusiveWrite);
        if !command.command.is_preparation_prompt(session_id) && !mutating_task {
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
    /// closes any open active-work timing window at `timestamp_seconds`,
    /// retaining clarification when only unstarted queued work remains.
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
        ag_worker::recover(
            db.operations(),
            RESTART_FAILURE_REASON,
            |unfinished_operations| async move {
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
                    let session_operations: Vec<_> = unfinished_operations
                        .iter()
                        .filter(|operation| operation.session_id == session_id)
                        .collect();
                    // A graph lease only ever moves `Draft` or `Review` to
                    // `InProgress` and ends in `Review`, so interrupted graph
                    // work alone undoes just that and leaves every other
                    // status as the lease found it.
                    let is_graph_lease = |operation: &&SessionOperationRow| {
                        matches!(
                            operation.kind.as_str(),
                            GRAPH_ACTIVITY_OPERATION_KIND | GRAPH_NODE_ACTIVITY_OPERATION_KIND
                        )
                    };
                    let is_graph_activity_only = session_operations.iter().any(is_graph_lease)
                        && session_operations.iter().all(|operation| {
                            operation.started_at.is_none() || is_graph_lease(operation)
                        });
                    if is_graph_activity_only {
                        let is_in_progress = db
                            .sessions()
                            .load_session(session_id)
                            .await?
                            .is_some_and(|session| {
                                session.status.parse::<Status>().ok() == Some(Status::InProgress)
                            });
                        if is_in_progress {
                            db.sessions()
                                .update_session_status_with_timing_at(
                                    session_id,
                                    &Status::Review.to_string(),
                                    timestamp_seconds,
                                )
                                .await?;
                        }

                        continue;
                    }

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
                    let awaiting_clarification =
                        unfinished_operations
                            .iter()
                            .filter(|operation| operation.session_id == session_id)
                            .all(|operation| operation.started_at.is_none())
                            && db.sessions().load_session(session_id).await?.is_some_and(
                                |session| session.status == Status::Question.to_string(),
                            );
                    let recovered_status = if awaiting_clarification {
                        Status::Question
                    } else if unstarted_first_prompt {
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

                Ok(())
            },
        )
        .await
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
        task: Option<ag_session::SubmitTaskRequest>,
    ) -> Result<bool, SessionError> {
        let session_id = runtime.session_id.clone();
        let operation_id = command.operation_id().to_string();
        let claimed = if let Some(task) = task {
            services
                .db()
                .tasks()
                .claim_attempt(&session_id, &task, command.kind())
                .await?
        } else {
            services
                .db()
                .operations()
                .claim_session_operation(&operation_id, &session_id, command.kind())
                .await?
        };
        if !claimed {
            return Ok(false);
        }

        self.enqueue_claimed_command(services, runtime, command)
            .await?;

        Ok(true)
    }

    /// Hands an already durable claim to its serial owner exactly once.
    async fn enqueue_claimed_command(
        &mut self,
        services: &AppServices,
        runtime: SessionWorkerRuntime,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        let session_id = runtime.session_id.clone();
        let operation_id = command.operation_id().to_string();

        if let SessionCommand::RunAgentTask { cancellation, .. } = &command {
            self.register_task_cancellation(&session_id, &operation_id, cancellation);
        }

        let worker = self.ensure_session_worker(services, &runtime);
        let mut scheduled_command = ScheduledSessionCommand::immediate(command);
        self.reserve_preparation_command(&session_id, &mut scheduled_command);
        self.send_persisted_command(
            services.db().operations(),
            &session_id,
            worker,
            scheduled_command,
        )
        .await?;

        Ok(())
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
            worker,
            ScheduledSessionCommand::queued(command, queued_order),
        )
        .await?;

        Ok(queued_order)
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.workers.remove(session_id);
        self.session_runs.remove(session_id);
    }

    /// Prepares delivery of queued prompts into the session's running turn
    /// through the worker's own runtime, or returns `None` without a live
    /// worker or while the worker runs anything but a chat turn.
    ///
    /// The delivery begins before the chat-turn check. The worker waits for
    /// begun deliveries after it ends a chat turn, so the turn that check
    /// observes is the only one the prompts can join.
    ///
    /// Also returns `None` while the session's current-turn token is
    /// cancelled, so the prompts stay queued. Either the user stopped the
    /// running turn, whose queue the worker clears, or they stopped an
    /// earlier turn and the running one has not installed its own token yet.
    /// A delivery holding that stale token would discard a rejected prompt
    /// as stopped.
    pub(super) fn queued_message_steering(
        &self,
        services: &AppServices,
        session_id: &str,
        handles: &SessionHandles,
    ) -> Option<QueuedMessageSteering> {
        let worker = self.workers.get(session_id)?.clone();
        let live_run = self.session_runs.get(session_id)?;
        let delivery = live_run.steering_deliveries.begin();
        if !handles.has_steerable_turn() {
            return None;
        }
        let turn_cancel_token = handles
            .cancel_token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if turn_cancel_token.is_cancelled() {
            return None;
        }

        Some(QueuedMessageSteering {
            app_event_tx: services.event_sender(),
            cancel_token: Arc::clone(&handles.cancel_token),
            db: services.db().clone(),
            delivery,
            message_traces: Arc::clone(&self.message_traces),
            queued_messages: Arc::clone(&handles.queued_messages),
            session_id: SessionId::from(session_id),
            session_run: live_run.session_run.clone(),
            session_update_versions: services.session_update_versions(),
            transcript: Arc::clone(&handles.transcript),
            turn_cancel_token,
            wake_worker: Box::new(move || worker.wake()),
        })
    }

    /// Holds scheduling while the host attempts a reversible model update.
    async fn pause_session_worker(
        &self,
        session_id: &str,
    ) -> Option<ag_worker::SessionWorkerPause> {
        Some(self.workers.get(session_id)?.pause().await)
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
        scheduled_command
            .trace
            .attribute("agentty.session.id", runtime.session_id.to_string());
        scheduled_command.ready_rx = Some(ready_rx);
        self.reserve_preparation_command(&runtime.session_id, &mut scheduled_command);
        if worker.submit(scheduled_command).is_err() {
            self.clear_session_worker(&runtime.session_id);

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

        let session_run = services.session_run(&runtime.session_id, runtime.session_agent.kind());
        let steering_deliveries = Arc::<SteeringDeliveries>::default();
        self.session_runs.insert(
            runtime.session_id.clone(),
            LiveSessionRun {
                session_run: session_run.clone(),
                steering_deliveries: Arc::clone(&steering_deliveries),
            },
        );

        let context = SessionWorkerContext {
            app_event_tx: services.event_sender(),
            available_agent_kinds: services.available_agent_kinds().into(),
            branch_operation_lock: Arc::clone(&runtime.branch_operation_lock),
            cancel_token: Arc::clone(&runtime.cancel_token),
            cargo_cache: services.cargo_cache(),
            session_run,
            child_pid: Arc::clone(&runtime.child_pid),
            clock: services.clock(),
            db: services.db().clone(),
            folder: runtime.folder.clone(),
            fs_client: services.fs_client(),
            git_client: services.git_client(),
            graph_activity: services.graph_activity.clone(),
            harness_models: services.harness_models(),
            queued_messages: Arc::clone(&runtime.queued_messages),
            review_request_client: Arc::clone(&runtime.review_request_client),
            session_update_versions: Arc::clone(&runtime.session_update_versions),
            session_id: runtime.session_id.clone(),
            session_agent: runtime.session_agent,
            session_commands: services.task_run_factory().session_commands(),
            status: Arc::clone(&runtime.status),
            transcript: Arc::clone(&runtime.transcript),
        };
        let worker = SessionWorkerHandle::spawn(
            SessionWorkerHost {
                graph_resources: Arc::clone(&services.graph_resources),
                graph_wake: services.graph_wake.clone(),
                context,
                message_traces: Arc::clone(&self.message_traces),
                run_client: services.session_run_client(&runtime.session_id),
                steerable_turn: Arc::clone(&runtime.steerable_turn),
                steering_deliveries,
            },
            Arc::clone(&runtime.queued_work_sequence),
        );
        services.track_session_worker(&runtime.session_id, worker.task());
        self.workers
            .insert(runtime.session_id.clone(), worker.clone());

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
            worker,
            ScheduledSessionCommand::immediate(command),
        )
        .await
    }

    /// Sends one command whose operation row has already been persisted.
    async fn send_persisted_command(
        &mut self,
        operations: &dyn OperationRepository<crate::infra::db::DbError>,
        session_id: &SessionId,
        worker: SessionWorkerHandle,
        scheduled_command: ScheduledSessionCommand,
    ) -> Result<(), SessionError> {
        scheduled_command
            .trace
            .attribute("agentty.session.id", session_id.to_string());

        let operation_id = scheduled_command.command.operation_id().to_string();
        if worker.submit(scheduled_command).is_err() {
            self.clear_session_worker(session_id);
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

    /// Shares per-execution cancellation for role and graph task commands.
    fn register_task_cancellation(
        &mut self,
        session_id: &SessionId,
        operation_id: &str,
        cancellation: &Arc<CancellationToken>,
    ) {
        self.task_cancellations
            .retain(|_, (_, token)| token.strong_count() > 0);
        self.task_cancellations.insert(
            operation_id.to_string(),
            (session_id.clone(), Arc::downgrade(cancellation)),
        );
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
    ///
    /// The command's purpose and selected harness identify the workflow span
    /// and every provider operation span the command starts.
    async fn process_session_command(
        context: &SessionWorkerContext,
        run_client: &Arc<dyn RunClient>,
        command: SessionCommand,
    ) -> Option<Result<(), SessionError>> {
        let mut identity = vec![KeyValue::new("agentty.purpose", command.kind())];
        if let SessionCommand::Run { turn_metadata, .. } = &command {
            identity.push(KeyValue::new(
                "agentty.harness",
                turn_metadata.session_agent.kind().to_string(),
            ));
        }

        ag_telemetry::identified(
            identity,
            Self::run_identified_session_command(context, run_client, command),
        )
        .await
    }

    async fn run_identified_session_command(
        context: &SessionWorkerContext,
        run_client: &Arc<dyn RunClient>,
        command: SessionCommand,
    ) -> Option<Result<(), SessionError>> {
        let operation_id = command.operation_id().to_string();
        let is_agent_task = matches!(&command, SessionCommand::RunAgentTask { .. });
        let graph_completion = match &command {
            SessionCommand::RunGraphTask(task) => Some(task.completion.clone()),
            _ => None,
        };
        ag_telemetry::current_attribute("agentty.operation.id", operation_id.clone());
        if matches!(&command, SessionCommand::Run { .. }) {
            ag_telemetry::current_attribute("agentty.turn.id", operation_id.clone());
        }
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
            if matches!(request_kind, AgentRequestKind::SessionStart) {
                SessionTaskService::append_auto_address_notice(
                    &context.transcript,
                    &context.db,
                    &context.app_event_tx,
                    &context.session_update_versions,
                    &context.session_id,
                )
                .await;
            }
        }

        let is_generated_turn = Self::begin_generated_turn(context, &command).await;

        let turn_selection = Self::emit_turn_started(context, command.turn_agent());

        let result = ag_worker::execute(
            context.db.operations(),
            &ag_worker::HeartbeatClock,
            &operation_id,
            ag_worker::in_scope(
                ag_worker::RunScope {
                    parent_id: Some(operation_id.clone()),
                    session_id: Some(context.session_id.to_string()),
                    usage_purpose: Some(ag_contracts::UsagePurpose::Other),
                    ..ag_worker::RunScope::default()
                },
                Box::pin(Self::execute_session_command(context, run_client, command)),
            ),
            |error| matches!(error, SessionError::StoppedByUser(_)),
            |error| tracing::warn!(%error, "Worker operation tracking failed"),
        )
        .await;

        if let Some(completion) = graph_completion {
            completion.finish(result.as_ref().copied().map_err(ToString::to_string));
        }

        if is_agent_task {
            Self::emit_queue_session_updated(context);
        }

        if is_generated_turn {
            Self::emit_generated_turn_update(context, GeneratedTurnUpdate::Finished);
        }

        // Auto-commit can swallow cancellation errors during post-processing.
        // The active turn's token still records the user's interruption.
        let interrupted = context
            .cancel_token
            .lock()
            .is_ok_and(|token| token.is_cancelled());
        let outcome = match &result {
            _ if interrupted => Some(TurnOutcome::Interrupted),
            Ok(()) => Some(TurnOutcome::Finished),
            Err(SessionError::StoppedByUser(_)) => Some(TurnOutcome::Interrupted),
            Err(_) => None,
        };
        if let (Some(turn), Some(outcome)) = (turn_selection, outcome) {
            let _ = context
                .app_event_tx
                .send(AppEvent::SessionTurnEnded { outcome, turn });
        }

        Some(result)
    }

    /// Persists generated context after skip checks and starts generated-turn
    /// progress, returning whether the command is a generated turn.
    async fn begin_generated_turn(
        context: &SessionWorkerContext,
        command: &SessionCommand,
    ) -> bool {
        let SessionCommand::Run {
            prompt,
            turn_metadata,
            ..
        } = command
        else {
            return false;
        };

        if !turn_metadata.review_comment_thread_ids.is_empty() {
            let _ = context
                .db
                .sessions()
                .update_session_focused_review(&context.session_id, None, None, None)
                .await;
        }

        let Some(labels) = &turn_metadata.generated_turn else {
            return false;
        };

        let prompt_text = prompt.transcript_text();
        SessionTaskService::append_session_transcript_message(
            &context.transcript,
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            SessionTranscriptMessageAppend {
                kind: SessionMessageKind::AgentPrompt,
                raw_content: &prompt_text,
            },
        )
        .await;
        Self::emit_generated_turn_update(
            context,
            GeneratedTurnUpdate::Started(labels.active.clone()),
        );

        true
    }

    /// Reports a model turn beginning execution with its queued selection,
    /// returning it for the turn's settlement event.
    fn emit_turn_started(
        context: &SessionWorkerContext,
        turn_agent: Option<AgentSelection>,
    ) -> Option<TurnSelection> {
        let agent = turn_agent?;
        let selection = TurnSelection { agent };
        let _ = context.app_event_tx.send(AppEvent::SessionTurnStarted {
            session_id: context.session_id.clone(),
            turn: selection,
        });

        Some(selection)
    }

    /// Sends one generated-turn lifecycle update to the foreground reducer.
    fn emit_generated_turn_update(context: &SessionWorkerContext, update: GeneratedTurnUpdate) {
        let _ = context
            .app_event_tx
            .send(AppEvent::SessionGeneratedTurnUpdated {
                session_id: context.session_id.clone(),
                update,
            });
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
        if let SessionCommand::RunGraphTask(task) = command {
            task.completion
                .finish(Err("Graph task was canceled or already settled before \
                             execution"
                    .into()));
        }
        if let SessionCommand::Run { turn_metadata, .. } = command
            && turn_metadata.generated_turn.is_some()
        {
            Self::emit_generated_turn_update(context, GeneratedTurnUpdate::Skipped);
        }

        if let SessionCommand::CreateReviewRequest { operation_id, .. } = command {
            let _ = context
                .app_event_tx
                .send(AppEvent::BranchPublishActionResolved {
                    operation_id: operation_id.clone(),
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
        run_client: &Arc<dyn RunClient>,
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
                generated_turn: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: context.session_agent,
            },
        };

        Self::process_session_command(context, run_client, command).await
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
        run_client: &Arc<dyn RunClient>,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        match command {
            SessionCommand::RunGraphTask(task) => {
                graph_turn::run(context, Arc::clone(run_client), &task).await
            }
            SessionCommand::RunAgentTask {
                request,
                factory,
                cancellation,
                ..
            } => {
                task_turn::run(
                    context,
                    Arc::clone(run_client),
                    &request,
                    factory.as_ref(),
                    cancellation.as_ref().clone(),
                )
                .await
            }
            SessionCommand::CreateReviewRequest {
                branch_publish_session,
                operation_id,
                remote_branch_name,
                response,
            } => {
                Self::run_create_review_request_command(
                    context,
                    Arc::clone(run_client),
                    branch_publish_session,
                    operation_id,
                    remote_branch_name,
                    response,
                )
                .await
            }
            SessionCommand::Rebase { base_branch, .. } => {
                Self::run_rebase_command(context, Arc::clone(run_client), base_branch).await
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
                    Arc::clone(run_client),
                    turn_metadata,
                    request_kind,
                    replay_transcript,
                    prompt,
                )
                .await
            }
        }
    }

    /// Restores unfinished, request-scoped targets at execution time, including
    /// question answers and chat messages queued before the previous turn
    /// ended.
    pub(super) async fn prepare_review_comment_turn(
        context: &SessionWorkerContext,
        turn_metadata: &mut TurnMetadata,
        prompt: &mut TurnPrompt,
    ) -> Result<(), SessionError> {
        let pending = context
            .db
            .reviews()
            .load_session_review_comment_targets(&context.session_id)
            .await?;
        let has_pending = !pending.is_empty();
        for thread_id in pending {
            if !turn_metadata.review_comment_thread_ids.contains(&thread_id) {
                turn_metadata.review_comment_thread_ids.push(thread_id);
            }
        }
        if turn_metadata.review_comment_thread_ids.is_empty() {
            return Ok(());
        }
        context
            .db
            .reviews()
            .prepare_session_review_comment_turn(
                &context.session_id,
                &turn_metadata.review_comment_thread_ids,
            )
            .await?;
        turn_metadata.published_upstream_ref = context.load_published_upstream_ref().await;
        if has_pending {
            let targets = serde_json::json!(turn_metadata.review_comment_thread_ids);
            let continuation = include_str!("../../template/review_comment_continuation_prompt.md")
                .replace("{{ thread_ids }}", &targets.to_string());
            prompt.text.push_str("\n\n");
            prompt.text.push_str(&continuation);
        }

        Ok(())
    }

    /// Publishes one review request inside the serialized session worker.
    ///
    /// The live status is applied at execution time so an action accepted
    /// during `InProgress` or `Rebasing` observes the completed work's
    /// review-ready state instead of the enqueue-time snapshot.
    async fn run_create_review_request_command(
        context: &SessionWorkerContext,
        run_client: Arc<dyn RunClient>,
        mut branch_publish_session: BranchPublishTaskSession,
        operation_id: String,
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
                app_event_tx: context.app_event_tx.clone(),
                branch_operation_lock: Arc::clone(&context.branch_operation_lock),
                run_client,
                session: branch_publish_session,
                session_agent: context.session_agent,
                session_update_versions: context.session_update_versions.clone(),
                transcript: Arc::clone(&context.transcript),
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
                operation_id: Some(operation_id),
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
        run_client: Arc<dyn RunClient>,
        base_branch: String,
    ) -> Result<(), SessionError> {
        let cancellation = turn::fresh_turn_cancel_token(context)?;
        let run_client = ag_worker::scoped_client(
            run_client,
            ag_worker::RunScope {
                cancellation: Some(cancellation),
                ..ag_worker::RunScope::default()
            },
        );
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
            available_agent_kinds: Arc::clone(&context.available_agent_kinds),
            base_branch,
            branch_operation_lock: Arc::clone(&context.branch_operation_lock),
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            commit_hooks: context.commit_hooks(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            fs_client: Arc::clone(&context.fs_client),
            git_client: Arc::clone(&context.git_client),
            graph_activity: context.graph_activity.clone(),
            harness_models: context.harness_models.clone(),
            id: context.session_id.clone(),
            run_client,
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
    /// Queues an admitted graph task without reclaiming its operation.
    pub(in crate::app) async fn enqueue_claimed_graph_task(
        &mut self,
        services: &AppServices,
        request: ag_worker::GraphRunRequest,
        cancellation: CancellationToken,
        response_tx: oneshot::Sender<Result<String, ag_contracts::OneShotError>>,
    ) {
        let completion = graph_turn::GraphTaskCompletion::new(response_tx);
        let result = async {
            let node = &request.dispatch.node;
            let record = &request.dispatch.record;
            let ag_session::GraphExecutionRef::TaskAttempt {
                task_id,
                attempt_id,
                operation_id,
            } = &record.execution
            else {
                return Err(SessionError::Workflow(
                    "Graph handoff requires a task attempt".into(),
                ));
            };
            let ag_session::EnvId::SessionWorktree(session_id) = &node.env_id;
            self.state.sync_from_handles();
            let session = self.session_or_err(session_id)?;
            if !matches!(
                session.status,
                Status::Draft
                    | Status::Review
                    | Status::AgentReview
                    | Status::Watching
                    | Status::InProgress
                    | Status::Question
            ) || request.folder != session.folder
            {
                return Err(SessionError::Workflow(
                    "Graph session is unavailable or its Env changed".into(),
                ));
            }
            Self::validate_graph_task_claim(services, &request, task_id, attempt_id).await?;
            self.validate_agent_task_workspace(services, session_id)?;
            let runtime = self.session_worker_runtime_or_err(services, session_id)?;
            let owner = session_id.clone();
            let operation_id = operation_id.clone();
            let cancellation = Arc::new(cancellation);
            // Stage cancellation interrupts exactly this execution while the
            // graph and its other nodes stay open.
            self.worker_service_mut().register_task_cancellation(
                &owner,
                &operation_id,
                &cancellation,
            );
            let command = SessionCommand::RunGraphTask(Box::new(graph_turn::GraphTaskCommand {
                cancellation,
                completion: completion.clone(),
                factory: services.task_run_factory(),
                operation_id,
                request,
                resources: services.graph_resources.clone(),
            }));
            let worker = self
                .worker_service_mut()
                .ensure_session_worker(services, &runtime);
            let order = worker.next_queued_work_order();
            let mut scheduled = ScheduledSessionCommand::queued(command, order);
            self.worker_service_mut()
                .reserve_preparation_command(&owner, &mut scheduled);
            self.worker_service_mut()
                .send_persisted_command(services.db().operations(), &owner, worker, scheduled)
                .await
        }
        .await;
        if let Err(error) = result {
            completion.finish(Err(error.to_string()));
        }
    }

    /// Verifies the captured graph request and queued operation before handoff.
    async fn validate_graph_task_claim(
        services: &AppServices,
        request: &ag_worker::GraphRunRequest,
        task_id: &str,
        attempt_id: &str,
    ) -> Result<(), SessionError> {
        let record = &request.dispatch.record;
        let graph = services
            .db()
            .graphs()
            .load_graph(&record.graph_id, Some(record.graph_revision))
            .await?
            .ok_or_else(|| SessionError::Workflow("Graph snapshot is missing".into()))?;
        let ag_session::EnvId::SessionWorktree(session_id) = &request.dispatch.node.env_id;
        if graph.session_id != *session_id
            || !graph.nodes.contains(&request.dispatch.node)
            || services
                .db()
                .graphs()
                .load_execution(&record.execution)
                .await?
                .as_ref()
                != Some(record)
        {
            return Err(SessionError::Workflow(
                "Graph task differs from its durable claim".into(),
            ));
        }
        let queued = services
            .db()
            .tasks()
            .load_task(task_id)
            .await?
            .is_some_and(|task| {
                task.attempts.iter().any(|attempt| {
                    attempt.id == attempt_id
                        && attempt.status == ag_session::TaskAttemptStatus::Queued
                })
            });
        if !queued {
            return Err(SessionError::Workflow("Graph task is not queued".into()));
        }

        Ok(())
    }

    /// Checks live readiness before isolated admission. Reading the live
    /// handle prevents a delayed reducer event from hiding active work.
    pub(in crate::app) fn validate_agent_task_admission(
        &mut self,
        session_id: &str,
        definition: ag_session::TaskAgentRole,
    ) -> Result<(), SessionError> {
        self.sync_session_from_handle(session_id);
        let status = self.session_or_err(session_id)?.status;
        let planner_start =
            definition == ag_session::TaskAgentRole::Planner && status == Status::Draft;
        if !status.allows_review_actions() && !planner_start {
            return Err(SessionError::Workflow(format!(
                "Session cannot accept isolated task in status `{status}`"
            )));
        }

        Ok(())
    }

    /// Enqueues a new role attempt already claimed by an atomic coordinator
    /// mutation. This method never adopts or repeats a running/settled attempt.
    pub(in crate::app) async fn enqueue_claimed_agent_task(
        &mut self,
        services: &AppServices,
        session_id: &str,
        request: ag_session::SubmitAgentTaskRequest,
    ) -> Result<(), SessionError> {
        let operation_id = format!("task-attempt:{}", request.attempt_id);
        let tasks = services.db().tasks();
        let queued = tasks
            .load_task(&request.task_id)
            .await?
            .is_some_and(|task| {
                task.attempts.iter().any(|attempt| {
                    attempt.id == request.attempt_id
                        && attempt.status == ag_session::TaskAttemptStatus::Queued
                })
            });
        if !queued || !tasks.contains_agent_attempt(session_id, &request).await? {
            return Err(SessionError::Workflow(
                "Task attempt is not a matching queued claim".to_string(),
            ));
        }
        let result = async {
            self.validate_agent_task_admission(session_id, request.assignment.role)?;
            tasks.verify_agent_plan(session_id, &request).await?;
            self.validate_agent_task_workspace(services, session_id)?;
            self.ensure_session_worktree_ready(services, session_id)
                .await?;
            self.clear_session_draft_flag(services, session_id).await?;
            let runtime = self.session_worker_runtime_or_err(services, session_id)?;
            let command = SessionCommand::RunAgentTask {
                cancellation: Arc::new(CancellationToken::new()),
                operation_id: operation_id.clone(),
                request: Box::new(request),
                factory: services.task_run_factory(),
            };
            self.worker_service_mut()
                .enqueue_claimed_command(services, runtime, command)
                .await
        }
        .await;
        if let Err(error) = &result {
            services
                .db()
                .operations()
                .mark_session_operation_failed(&operation_id, &error.to_string())
                .await?;
        }

        result
    }

    pub(in crate::app) fn validate_agent_task_workspace(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let session = self.session_or_err(session_id)?;
        if session.is_draft_session() && !services.fs_client().is_dir(session.folder.clone()) {
            return Err(SessionError::Workflow(
                "Task execution requires a prepared workspace; create a regular session"
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Interrupts only the admitted task attempt owning this operation.
    pub(in crate::app) fn cancel_agent_task_attempt(&mut self, operation_id: &str) {
        if let Some((owner, token)) = self
            .worker_service_mut()
            .task_cancellations
            .get(operation_id)
            .and_then(|(owner, token)| token.upgrade().map(|token| (owner.clone(), token)))
        {
            token.cancel();
            self.worker_service_mut().wake_session_worker(&owner);
        }
    }

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

    /// Persists a reply in the shared queue, creating a paused worker for
    /// review batches in restored question sessions. Active work must retain
    /// its existing worker to avoid concurrent execution.
    ///
    /// # Errors
    /// Returns an error when the session runtime is missing, an active worker
    /// is unavailable, or operation persistence or delivery fails.
    pub(super) async fn enqueue_queued_session_command(
        &mut self,
        services: &AppServices,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<u64, SessionError> {
        if self.session_or_err(session_id)?.status == Status::Question
            && command.review_comment_count() > 0
        {
            let runtime = self.session_worker_runtime_or_err(services, session_id)?;
            self.worker_service_mut()
                .ensure_session_worker(services, &runtime);
        }

        self.worker_service_mut()
            .enqueue_existing_session_command(services, &SessionId::from(session_id), command)
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
        task: Option<ag_session::SubmitTaskRequest>,
    ) -> Result<bool, SessionError> {
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;

        self.worker_service_mut()
            .enqueue_session_command_idempotently(services, runtime, command, task)
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
        operation_id: String,
        remote_branch_name: Option<String>,
        response_tx: Option<oneshot::Sender<Result<ReviewRequest, ag_session::SessionError>>>,
    ) -> Result<Option<u64>, SessionError> {
        let session_id = branch_publish_session.id.clone();
        let status = branch_publish_session.status;
        let response = response_tx.map(|response_tx| Arc::new(Mutex::new(Some(response_tx))));
        let command = SessionCommand::CreateReviewRequest {
            branch_publish_session,
            operation_id,
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
        let operations = self
            .worker_service_mut()
            .task_cancellations
            .iter()
            .filter(|(_, (owner, _))| owner.as_str() == session_id)
            .map(|(operation, _)| operation.clone())
            .collect::<Vec<_>>();
        for operation in operations {
            self.cancel_agent_task_attempt(&operation);
        }
        self.worker_service_mut().clear_session_worker(session_id);
    }

    /// Settles current work and retains pending work until persistence commits.
    pub(super) async fn pause_session_worker(
        &mut self,
        session_id: &str,
    ) -> Option<ag_worker::SessionWorkerPause> {
        self.worker_service_mut()
            .pause_session_worker(session_id)
            .await
    }

    /// Wakes an existing worker so it re-evaluates buffered work against the
    /// current session status.
    pub(crate) fn wake_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().wake_session_worker(session_id);
    }

    /// Wakes the worker that drains a newly queued chat message.
    ///
    /// A graph lease holds `InProgress` without a worker turn, so a session
    /// with no worker yet, such as one restored at startup, gets one here.
    /// The lease may already have ended, leaving `Review`, which gets one
    /// too. Any other status keeps the message queued for the worker that
    /// owns the running work.
    ///
    /// # Errors
    /// Returns an error when the session runtime handles are missing.
    pub(super) fn wake_queued_message_worker(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;
        if !services
            .graph_activity
            .needs_queue_worker(&runtime.session_id, &runtime.status)
        {
            return Ok(());
        }
        self.worker_service_mut()
            .ensure_session_worker(services, &runtime);
        self.wake_session_worker(session_id);

        Ok(())
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
            queued_messages: Arc::clone(&handles.queued_messages),
            queued_work_sequence: Arc::clone(&handles.queued_work_sequence),
            review_request_client: services.review_request_client(),
            session_update_versions: services.session_update_versions(),
            session_id: session.id.clone(),
            session_agent: session.agent,
            status: Arc::clone(&handles.status),
            steerable_turn: Arc::clone(&handles.steerable_turn),
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
