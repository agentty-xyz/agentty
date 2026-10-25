//! Foreground-owned session workflow actor and cloneable control handle.
//!
//! [`SessionRuntime`] owns the live [`SessionManager`] plus a bounded command
//! mailbox. The terminal runtime drives accepted commands on the foreground
//! task so reducer-owned render snapshots and session handles remain coherent,
//! while [`SessionRuntimeHandle`] gives background callers a cloneable
//! `Send + Sync` capability without sharing all of [`crate::app::App`] behind
//! a mutex.

use std::collections::HashMap;
use std::future::poll_fn;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, ReviewRequest, Session,
    SessionError, SessionId,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::app::SessionManager;
use crate::app::command_gateway::{
    CommandDelivery, CommandInitiator, ReviewRequestCommand, SessionCommand, SessionMutation,
    TaskChainReplyAction, TaskChainReplyCommand,
};

/// Maximum number of accepted API commands waiting for foreground execution.
const SESSION_RUNTIME_COMMAND_CAPACITY: usize = 32;
/// Stable error returned after the foreground session runtime has stopped.
const SESSION_RUNTIME_UNAVAILABLE: &str = "Session runtime is unavailable";

/// A read query or an enveloped mutation awaiting foreground admission.
pub(crate) enum SessionRuntimeCommand {
    GetExecutionGraph {
        graph_id: String,
        response_tx:
            oneshot::Sender<Result<Option<ag_session::ExecutionGraphProgress>, SessionError>>,
    },
    /// Trusted handoff of an already admitted graph task to its serial owner.
    RunGraphNode {
        admitted_tx: oneshot::Sender<()>,
        cancellation: CancellationToken,
        request: Box<ag_worker::GraphRunRequest>,
        response_tx: oneshot::Sender<Result<String, ag_contracts::OneShotError>>,
    },
    GetAgentTaskHistory {
        response_tx: oneshot::Sender<Result<Option<ag_session::TaskAgentHistory>, SessionError>>,
        task_id: String,
    },
    GetTaskChain {
        chain_id: String,
        response_tx: oneshot::Sender<Result<Option<ag_session::TaskChain>, SessionError>>,
    },
    ListTaskChains {
        response_tx: oneshot::Sender<Result<Vec<ag_session::TaskChain>, SessionError>>,
        session_id: SessionId,
    },
    ListTasks {
        response_tx: oneshot::Sender<Result<Vec<ag_session::Task>, SessionError>>,
        session_id: SessionId,
    },
    GetTask {
        response_tx: oneshot::Sender<Result<Option<ag_session::Task>, SessionError>>,
        task_id: String,
    },
    Get {
        response_tx: oneshot::Sender<Result<Option<Session>, SessionError>>,
        session_id: SessionId,
    },
    Mutation(SessionCommand),
}

/// Cloneable control capability for the foreground session runtime.
#[derive(Clone)]
pub(crate) struct SessionRuntimeHandle {
    command_tx: mpsc::Sender<SessionRuntimeCommand>,
    consumer_state: Arc<SessionRuntimeConsumerState>,
    initiator: CommandInitiator,
}

impl SessionRuntimeHandle {
    pub(crate) async fn get_execution_graph(
        &self,
        graph_id: &str,
    ) -> Result<Option<ag_session::ExecutionGraphProgress>, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::GetExecutionGraph {
            graph_id: graph_id.into(),
            response_tx,
        })
        .await
    }

    pub(crate) async fn cancel_execution_graph(
        &self,
        request: ag_session::GraphControlRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::CancelExecutionGraph { request })
            .await
    }

    pub(crate) async fn resume_execution_graph(
        &self,
        request: ag_session::ResumeExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::ResumeExecutionGraph { request })
            .await
    }

    pub(crate) async fn continue_execution_graph_node(
        &self,
        request: ag_session::ContinueGraphNodeRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::ContinueExecutionGraphNode {
            request: Box::new(request),
        })
        .await
    }

    pub(crate) async fn run_graph_node(
        &self,
        request: ag_worker::GraphRunRequest,
        cancellation: CancellationToken,
    ) -> Result<String, ag_contracts::OneShotError> {
        // The executor already owns durable admission. This handoff only asks
        // the foreground owner to enqueue it, and completion belongs to the
        // worker.
        let mut consumer_rx = self.consumer_state.subscribe();
        if !*consumer_rx.borrow_and_update() {
            return Err(ag_contracts::OneShotError::new(SESSION_RUNTIME_UNAVAILABLE));
        }
        let (response_tx, response_rx) = oneshot::channel();
        let (admitted_tx, admitted_rx) = oneshot::channel();
        let command = SessionRuntimeCommand::RunGraphNode {
            admitted_tx,
            cancellation,
            request: Box::new(request),
            response_tx,
        };
        tokio::select! {
            biased;
            _ = consumer_rx.wait_for(|active| !*active) => return Err(ag_contracts::OneShotError::new(SESSION_RUNTIME_UNAVAILABLE)),
            result = self.command_tx.send(command) => result.map_err(|_| ag_contracts::OneShotError::new(SESSION_RUNTIME_UNAVAILABLE))?,
        }
        // Only the worker may acknowledge settlement once it owns delivery.
        tokio::select! {
            biased;
            result = admitted_rx => result.map_err(|_| ag_contracts::OneShotError::new(SESSION_RUNTIME_UNAVAILABLE))?,
            _ = consumer_rx.wait_for(|active| !*active) => return Err(ag_contracts::OneShotError::new(SESSION_RUNTIME_UNAVAILABLE)),
        }
        response_rx
            .await
            .map_err(|_| ag_contracts::OneShotError::new(SESSION_RUNTIME_UNAVAILABLE))?
    }

    pub(crate) async fn start_execution_graph(
        &self,
        request: ag_session::StartExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::StartExecutionGraph {
            request: Box::new(request),
        })
        .await
    }

    pub(crate) async fn accept_execution_graph_result(
        &self,
        reference: ag_session::GraphResultRef,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::AcceptExecutionGraphResult {
            reference: Box::new(reference),
        })
        .await
    }

    /// Admits an isolated task through the same foreground gateway.
    pub(crate) async fn submit_agent_task(
        &self,
        session_id: &SessionId,
        request: ag_session::SubmitAgentTaskRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::SubmitAgentTask {
            request: Box::new(request),
            session_id: session_id.clone(),
        })
        .await
    }

    /// Records standalone acceptance through the shared mutation gateway.
    pub(crate) async fn accept_agent_plan(
        &self,
        session_id: &SessionId,
        request: ag_session::AcceptTaskPlanRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::AcceptAgentPlan {
            request,
            session_id: session_id.clone(),
        })
        .await
    }

    /// Loads a task-owned conversation through the foreground query path.
    pub(crate) async fn get_agent_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<ag_session::TaskAgentHistory>, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::GetAgentTaskHistory {
            response_tx,
            task_id: task_id.to_string(),
        })
        .await
    }

    /// Starts one static workflow through foreground admission.
    pub(crate) async fn start_plan_then_implement(
        &self,
        session_id: &SessionId,
        request: ag_session::StartPlanThenImplementRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::StartTaskChain {
            request,
            session_id: session_id.clone(),
        })
        .await
    }

    /// Reads one durable workflow without dispatching work.
    pub(crate) async fn get_task_chain(
        &self,
        chain_id: &str,
    ) -> Result<Option<ag_session::TaskChain>, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::GetTaskChain {
            chain_id: chain_id.to_string(),
            response_tx,
        })
        .await
    }

    /// Lists persisted workflows for a session.
    pub(crate) async fn list_task_chains(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ag_session::TaskChain>, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::ListTaskChains {
            response_tx,
            session_id: session_id.clone(),
        })
        .await
    }

    /// Accepts an exact plan and admits implementation through the gateway.
    pub(crate) async fn accept_task_chain_plan(
        &self,
        chain_id: &str,
        request: ag_session::AcceptTaskChainPlanRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::AcceptTaskChainPlan {
            chain_id: chain_id.to_string(),
            request,
        })
        .await
    }

    /// Explicitly revises, clarifies, or retries the current workflow stage.
    pub(crate) async fn continue_task_chain(
        &self,
        chain_id: &str,
        request: ag_session::ContinueTaskChainRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::ContinueTaskChain {
            chain_id: chain_id.to_string(),
            request,
        })
        .await
    }

    /// Cancels only the selected workflow's current attempt.
    pub(crate) async fn cancel_task_chain(&self, chain_id: &str) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::CancelTaskChain {
            chain_id: chain_id.to_string(),
        })
        .await
    }

    /// Routes a natural-language reply; the response arrives after its
    /// workflow action is admitted, including any background decision.
    pub(crate) async fn reply_to_task_chain(
        &self,
        session_id: &SessionId,
        request: ag_session::ReplyToTaskChainRequest,
    ) -> Result<ag_session::TaskChainReplyOutcome, SessionError> {
        self.request_mutation(|response_tx| CommandDelivery::TaskChainReply {
            request: TaskChainReplyCommand::Reply {
                request,
                session_id: session_id.clone(),
            },
            response_tx,
        })
        .await
    }

    /// Admits an action decided in the background for a recorded reply.
    pub(crate) async fn apply_task_chain_reply(
        &self,
        action: TaskChainReplyAction,
    ) -> Result<ag_session::TaskChainReplyOutcome, SessionError> {
        self.request_mutation(|response_tx| CommandDelivery::TaskChainReply {
            request: TaskChainReplyCommand::Apply(action),
            response_tx,
        })
        .await
    }

    /// Persists a desired outcome through shared admission without execution.
    pub(crate) async fn create_task(
        &self,
        session_id: &SessionId,
        request: ag_session::CreateTaskRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::CreateTask {
            request,
            session_id: session_id.clone(),
        })
        .await
    }

    /// Lists one session's task history through the foreground actor.
    pub(crate) async fn list_tasks(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ag_session::Task>, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::ListTasks {
            response_tx,
            session_id: session_id.clone(),
        })
        .await
    }

    /// Submits an explicit attempt through shared admission.
    pub(crate) async fn submit_task(
        &self,
        session_id: &SessionId,
        request: ag_session::SubmitTaskRequest,
    ) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::SubmitTask {
            request,
            session_id: session_id.clone(),
        })
        .await
    }

    /// Reads task history through the foreground actor.
    pub(crate) async fn get_task(
        &self,
        task_id: &str,
    ) -> Result<Option<ag_session::Task>, SessionError> {
        self.request(|response_tx| SessionRuntimeCommand::GetTask {
            response_tx,
            task_id: task_id.to_string(),
        })
        .await
    }

    /// Persists explicit outcome acceptance through shared admission.
    pub(crate) async fn complete_task(&self, task_id: &str) -> Result<(), SessionError> {
        self.request_admission(SessionMutation::CompleteTask {
            task_id: task_id.to_string(),
        })
        .await
    }

    /// Selects caller provenance without changing mailbox ordering or replies.
    pub(crate) fn with_initiator(mut self, initiator: CommandInitiator) -> Self {
        self.initiator = initiator;

        self
    }

    /// Creates one session through the runtime actor.
    pub(crate) async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.request_mutation(|response_tx| CommandDelivery::Create {
            request,
            response_tx: Some(response_tx),
        })
        .await
    }

    /// Loads one complete session aggregate through the runtime actor.
    pub(crate) async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Session>, SessionError> {
        let session_id = session_id.clone();

        self.request(|response_tx| SessionRuntimeCommand::Get {
            response_tx,
            session_id,
        })
        .await
    }

    /// Sends one message through the runtime actor.
    pub(crate) async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request_admission(SessionMutation::SendMessage {
            message,
            session_id,
        })
        .await
    }

    /// Submits one durable turn through the runtime actor.
    pub(crate) async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request_admission(SessionMutation::SubmitDurableMessage {
            request,
            session_id,
        })
        .await
    }

    /// Answers one complete clarification-question set through the runtime
    /// actor.
    pub(crate) async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request_admission(SessionMutation::AnswerQuestions {
            request,
            session_id,
        })
        .await
    }

    /// Cancels one session through the runtime actor.
    pub(crate) async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request_admission(SessionMutation::Cancel { session_id })
            .await
    }

    /// Enqueues one session for merge through the runtime actor.
    pub(crate) async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        let session_id = session_id.clone();

        self.request_admission(SessionMutation::Merge { session_id })
            .await
    }

    /// Queues creation or refresh of one review request through the runtime
    /// actor.
    pub(crate) async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        let session_id = session_id.clone();

        self.request_mutation(|response_tx| CommandDelivery::Review {
            request: ReviewRequestCommand::new(session_id, None),
            response_tx: Some(response_tx),
        })
        .await
    }

    /// Delivers ordinary inputs with a reply that ends at admission.
    async fn request_admission(&self, request: SessionMutation) -> Result<(), SessionError> {
        self.request_mutation(|response_tx| CommandDelivery::Admission {
            request,
            response_tx: Some(response_tx),
        })
        .await
    }

    /// Attaches identity before a mutation enters the bounded mailbox.
    async fn request_mutation<ResultValue>(
        &self,
        delivery: impl FnOnce(oneshot::Sender<Result<ResultValue, SessionError>>) -> CommandDelivery,
    ) -> Result<ResultValue, SessionError> {
        self.request(|response_tx| {
            SessionRuntimeCommand::Mutation(SessionCommand::new(
                self.initiator,
                delivery(response_tx),
            ))
        })
        .await
    }

    /// Enqueues one typed command and waits for its per-command response.
    async fn request<ResultValue>(
        &self,
        command: impl FnOnce(
            oneshot::Sender<Result<ResultValue, SessionError>>,
        ) -> SessionRuntimeCommand,
    ) -> Result<ResultValue, SessionError> {
        let mut consumer_rx = self.consumer_state.subscribe();
        let consumer_is_active = *consumer_rx.borrow_and_update();
        if !consumer_is_active {
            return Err(SessionError::Operation(
                SESSION_RUNTIME_UNAVAILABLE.to_string(),
            ));
        }

        let (response_tx, response_rx) = oneshot::channel();
        tokio::select! {
            biased;
            _ = consumer_rx.wait_for(|consumer_is_active| !*consumer_is_active) => {
                return Err(SessionError::Operation(
                    SESSION_RUNTIME_UNAVAILABLE.to_string(),
                ));
            }
            result = self.command_tx.send(command(response_tx)) => {
                result.map_err(|_| {
                    SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
                })?;
            }
        }

        tokio::select! {
            biased;
            result = response_rx => {
                result.map_err(|_| {
                    SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
                })?
            }
            _ = consumer_rx.wait_for(|consumer_is_active| !*consumer_is_active) => {
                Err(SessionError::Operation(
                    SESSION_RUNTIME_UNAVAILABLE.to_string(),
                ))
            }
        }
    }
}

/// Shared foreground-consumer lifecycle observed by runtime handles.
struct SessionRuntimeConsumerState {
    active_count: AtomicUsize,
    active_tx: watch::Sender<bool>,
}

impl SessionRuntimeConsumerState {
    /// Creates an inactive consumer lifecycle signal.
    fn new() -> Self {
        let (active_tx, _active_rx) = watch::channel(false);

        Self {
            active_count: AtomicUsize::new(0),
            active_tx,
        }
    }

    /// Registers one foreground consumer until the returned guard drops.
    fn enter(self: &Arc<Self>) -> SessionRuntimeConsumerGuard {
        if self.active_count.fetch_add(1, Ordering::AcqRel) == 0 {
            self.active_tx.send_replace(true);
        }

        SessionRuntimeConsumerGuard {
            state: Arc::clone(self),
        }
    }

    /// Subscribes to foreground-consumer availability changes.
    fn subscribe(&self) -> watch::Receiver<bool> {
        self.active_tx.subscribe()
    }
}

/// Registration guard for one foreground session-command consumer.
pub(crate) struct SessionRuntimeConsumerGuard {
    state: Arc<SessionRuntimeConsumerState>,
}

impl Drop for SessionRuntimeConsumerGuard {
    fn drop(&mut self) {
        if self.state.active_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state.active_tx.send_replace(false);
        }
    }
}

/// Session workflow owner and foreground-driven actor mailbox.
pub(crate) struct SessionRuntime {
    /// Task chains whose reply decision has deferred its admission to a
    /// background task, with its independently cancelable run scope. Further
    /// replies are busy until that action arrives.
    pub(crate) deciding_task_chain_ids: HashMap<String, CancellationToken>,
    command_rx: mpsc::Receiver<SessionRuntimeCommand>,
    command_tx: mpsc::Sender<SessionRuntimeCommand>,
    consumer_state: Arc<SessionRuntimeConsumerState>,
    manager: SessionManager,
}

impl SessionRuntime {
    /// Wraps the loaded session manager and creates its bounded actor mailbox.
    pub(crate) fn new(manager: SessionManager) -> Self {
        let (command_tx, command_rx) = mpsc::channel(SESSION_RUNTIME_COMMAND_CAPACITY);

        Self {
            deciding_task_chain_ids: HashMap::new(),
            command_rx,
            command_tx,
            consumer_state: Arc::new(SessionRuntimeConsumerState::new()),
            manager,
        }
    }

    /// Returns a cloneable control handle for background or frontend callers.
    pub(crate) fn handle(&self) -> SessionRuntimeHandle {
        SessionRuntimeHandle {
            initiator: CommandInitiator::SessionApi,
            command_tx: self.command_tx.clone(),
            consumer_state: Arc::clone(&self.consumer_state),
        }
    }

    /// Registers a foreground command consumer for the guard's lifetime.
    pub(crate) fn foreground_consumer(&self) -> SessionRuntimeConsumerGuard {
        self.consumer_state.enter()
    }

    /// Waits for the next accepted actor command.
    ///
    /// The receiver cannot close while the runtime is alive because the
    /// runtime retains its own sender alongside the public handles.
    pub(crate) async fn next_command(&mut self) -> SessionRuntimeCommand {
        poll_fn(|context| match self.command_rx.poll_recv(context) {
            Poll::Ready(Some(command)) => Poll::Ready(command),
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        })
        .await
    }
}

impl From<SessionManager> for SessionRuntime {
    fn from(manager: SessionManager) -> Self {
        Self::new(manager)
    }
}

impl Deref for SessionRuntime {
    type Target = SessionManager;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

impl DerefMut for SessionRuntime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.manager
    }
}

#[cfg(test)]
#[path = "session_runtime_test.rs"]
mod tests;
