//! Programmatic session orchestration facade and host backend port.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::SessionError;
use crate::model::{ReviewRequest, Session, SessionId};
use crate::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, ContinueTaskChainRequest, CreateTaskRequest,
    ReplyToTaskChainRequest, StartPlanThenImplementRequest, SubmitTaskRequest, Task, TaskChain,
    TaskChainReplyOutcome,
};

/// Creation strategy for a new session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CreateSessionMode {
    /// Creates a regular root session with an eagerly materialized worktree.
    #[default]
    Regular,
    /// Creates a root draft whose worktree is materialized on first send.
    Draft,
    /// Creates a regular root session that runs on the in-process
    /// [`crate::AgentKind::Harness`] with its default model.
    Harness,
    /// Creates a draft stacked on an existing parent session.
    Stacked {
        /// Review-ready parent session whose branch becomes the stack base.
        parent_session_id: SessionId,
    },
}

/// Explicit input for creating one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSessionRequest {
    /// Existing session whose launch settings should be copied.
    ///
    /// When absent, the host resolves the owning project's current defaults.
    pub inherit_from_session_id: Option<SessionId>,
    /// Determines whether the session is regular, deferred, or stacked.
    pub mode: CreateSessionMode,
    /// Project that owns the new session.
    pub project_id: i64,
}

/// One structured response to a persisted clarification question.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionAnswer {
    /// User response paired with `question`.
    pub answer: String,
    /// Exact persisted question text being answered.
    pub question: String,
}

/// Structured input for resuming one session from clarification questions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerQuestionsRequest {
    /// Ordered question and answer pairs for the current question set.
    pub answers: Vec<QuestionAnswer>,
}

/// Idempotent turn submitted to one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableMessageRequest {
    /// Agent-facing prompt.
    pub message: String,
    /// Stable operation identifier reused when delivery is retried.
    pub operation_id: String,
    /// Whether the machine-authored prompt is shown in the human transcript.
    pub visibility: DurableMessageVisibility,
}

/// Transcript treatment for one durable prompt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DurableMessageVisibility {
    /// Deliver generated context without displaying it as user chat.
    #[default]
    Hidden,
    /// Display the submitted instruction in session history.
    Visible,
}

/// Host implementation boundary for session persistence and workflows.
///
/// The trait is object-safe so agent loops and background workflows can hold a
/// programmatic session capability without depending on a concrete frontend.
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Reads durable progress without launching or retrying work.
    async fn get_execution_graph(
        &self,
        graph_id: &str,
    ) -> Result<Option<crate::ExecutionGraphProgress>, SessionError> {
        let _ = graph_id;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Closes graph admission durably, then signals its owned workers.
    async fn cancel_execution_graph(
        &self,
        request: crate::GraphControlRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Explicitly restarts dispatch without replaying admitted executions.
    async fn resume_execution_graph(
        &self,
        request: crate::ResumeExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Authorizes one new attempt against an exact failed/question execution.
    async fn continue_execution_graph_node(
        &self,
        request: crate::ContinueGraphNodeRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Saves and starts a static graph. Existing admitted nodes are never
    /// replayed. Named budgets are shared within this graph's live driver.
    async fn start_execution_graph(
        &self,
        request: StartExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        let _ = request;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }

    /// Records explicit acceptance of an exact graph output and wakes ready
    /// dependents. Model output must never call this trusted host boundary.
    async fn accept_execution_graph_result(
        &self,
        reference: crate::GraphResultRef,
    ) -> Result<(), SessionError> {
        let _ = reference;

        Err(SessionError::Operation(
            "Host does not support execution graphs".into(),
        ))
    }
    /// Admits a role-bound attempt with its own isolated conversation.
    async fn submit_agent_task(
        &self,
        session_id: &SessionId,
        request: crate::SubmitAgentTaskRequest,
    ) -> Result<(), SessionError> {
        let _ = (session_id, request);

        Err(SessionError::Operation(
            "Host does not support role-aware tasks".to_string(),
        ))
    }

    /// Reads isolated task responses without exposing host storage types.
    async fn get_agent_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::TaskAgentHistory>, SessionError> {
        let _ = task_id;

        Err(SessionError::Operation(
            "Host does not support role-aware tasks".to_string(),
        ))
    }

    /// Explicitly accepts a standalone Planner response by producing version.
    async fn accept_agent_plan(
        &self,
        _session_id: &SessionId,
        _request: AcceptTaskPlanRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support plan acceptance".into(),
        ))
    }

    /// Launches a static workflow with one isolated Planner attempt.
    async fn start_plan_then_implement(
        &self,
        _session_id: &SessionId,
        _request: StartPlanThenImplementRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Reads a workflow snapshot without recovering or replaying work.
    async fn get_task_chain(&self, _chain_id: &str) -> Result<Option<TaskChain>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Lists a session's workflows in creation order.
    async fn list_task_chains(
        &self,
        _session_id: &SessionId,
    ) -> Result<Vec<TaskChain>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Explicitly accepts an exact Planner version and admits implementation.
    async fn accept_task_chain_plan(
        &self,
        _chain_id: &str,
        _request: AcceptTaskChainPlanRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Explicitly continues the current stage; uses a new attempt identity.
    async fn continue_task_chain(
        &self,
        _chain_id: &str,
        _request: ContinueTaskChainRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Requests cancellation of only this workflow's current attempt.
    async fn cancel_task_chain(&self, _chain_id: &str) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Routes a natural-language reply to the session's latest workflow.
    async fn reply_to_task_chain(
        &self,
        _session_id: &SessionId,
        _request: ReplyToTaskChainRequest,
    ) -> Result<TaskChainReplyOutcome, SessionError> {
        Err(SessionError::Operation(
            "Host does not support task chains".to_string(),
        ))
    }

    /// Persists an outcome without execution. Older hosts may omit support.
    async fn create_task(
        &self,
        _session_id: &SessionId,
        _request: CreateTaskRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Lists a session's tasks, including unattempted and completed outcomes.
    async fn list_tasks(&self, _session_id: &SessionId) -> Result<Vec<Task>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Admits an explicit task attempt. Older hosts may omit task support.
    async fn submit_task(
        &self,
        _session_id: &SessionId,
        _request: SubmitTaskRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Loads the desired outcome and retained attempt history.
    async fn get_task(&self, _task_id: &str) -> Result<Option<Task>, SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Explicitly accepts the task outcome after all attempts have settled.
    async fn complete_task(&self, _task_id: &str) -> Result<(), SessionError> {
        Err(SessionError::Operation(
            "Host does not support tasks".to_string(),
        ))
    }

    /// Creates one session and returns its stable identifier.
    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError>;

    /// Loads one complete session aggregate, including settings and messages.
    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError>;

    /// Sends one text message, starting, resuming, or queueing as appropriate.
    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError>;

    /// Submits a durable turn without entering the lossy live-chat
    /// queue used while an ordinary user turn is active.
    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError>;

    /// Answers the complete current clarification-question set.
    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError>;

    /// Cancels one session through the host lifecycle workflow.
    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Requests merge processing for one review-ready session.
    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Queues publication of one session branch and creates or refreshes its
    /// review request.
    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError>;
}

/// Stable programmatic facade for session lifecycle operations.
#[derive(Clone)]
pub struct SessionService {
    backend: Arc<dyn SessionBackend>,
}

/// Explicit static-graph launch, separate from ordinary chat and `/plan`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartExecutionGraphRequest {
    /// Validated immutable snapshot to save before admitting nodes.
    pub graph: crate::ExecutionGraph,
    /// Provider calls shared by nodes naming the same budget, including
    /// repairs. These limits cannot be replenished by redelivering an
    /// active launch.
    pub provider_call_limits: std::collections::BTreeMap<String, usize>,
}

impl SessionService {
    /// Reads graph/node progress and immutable execution history.
    ///
    /// # Errors
    /// Returns failed persistence or query delivery errors.
    pub async fn get_execution_graph(
        &self,
        graph_id: &str,
    ) -> Result<Option<crate::ExecutionGraphProgress>, SessionError> {
        self.backend.get_execution_graph(graph_id).await
    }

    /// Permanently cancels this graph without canceling unrelated session work.
    ///
    /// # Errors
    /// Rejects missing/stale graphs and failed durable cancellation.
    pub async fn cancel_execution_graph(
        &self,
        request: crate::GraphControlRequest,
    ) -> Result<(), SessionError> {
        self.backend.cancel_execution_graph(request).await
    }

    /// Resumes dispatch explicitly; failed nodes still require a new action.
    ///
    /// # Errors
    /// Rejects canceled/stale graphs, invalid limits, or ineligible sessions.
    pub async fn resume_execution_graph(
        &self,
        request: crate::ResumeExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        self.backend.resume_execution_graph(request).await
    }

    /// Retries a failed node or answers its exact settled clarification set.
    ///
    /// # Errors
    /// Rejects stale sources, partial answers, conflicting action identities,
    /// canceled graphs, and failed persistence or delivery.
    pub async fn continue_execution_graph_node(
        &self,
        request: crate::ContinueGraphNodeRequest,
    ) -> Result<(), SessionError> {
        self.backend.continue_execution_graph_node(request).await
    }

    /// Starts an explicit static graph without waiting for model execution.
    ///
    /// # Errors
    /// Rejects unsupported profiles, stale/conflicting identities, missing
    /// budgets, ineligible sessions, and failed persistence or delivery.
    pub async fn start_execution_graph(
        &self,
        request: StartExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        self.backend.start_execution_graph(request).await
    }

    /// Accepts one exact validated output and releases its acceptance gate.
    ///
    /// # Errors
    /// Rejects stale or unvalidated references and failed persistence.
    pub async fn accept_execution_graph_result(
        &self,
        reference: crate::GraphResultRef,
    ) -> Result<(), SessionError> {
        self.backend.accept_execution_graph_result(reference).await
    }

    /// Accepts a standalone Planner version from durable task history.
    ///
    /// # Errors
    /// Returns an error for stale/ineligible versions or failed persistence.
    pub async fn accept_agent_plan(
        &self,
        session_id: &SessionId,
        request: AcceptTaskPlanRequest,
    ) -> Result<(), SessionError> {
        self.backend.accept_agent_plan(session_id, request).await
    }

    /// Starts one Planner attempt with a captured provider/model selection.
    ///
    /// # Errors
    /// Returns an error for identity conflicts, ineligible sessions, or failed
    /// admission.
    pub async fn start_plan_then_implement(
        &self,
        session_id: &SessionId,
        request: StartPlanThenImplementRequest,
    ) -> Result<(), SessionError> {
        self.backend
            .start_plan_then_implement(session_id, request)
            .await
    }

    /// Reads a durable workflow snapshot without dispatching work.
    ///
    /// # Errors
    /// Returns an error when the host cannot read or decode the workflow.
    pub async fn get_task_chain(&self, chain_id: &str) -> Result<Option<TaskChain>, SessionError> {
        self.backend.get_task_chain(chain_id).await
    }

    /// Lists workflows owned by one session.
    ///
    /// # Errors
    /// Returns an error when the host cannot read workflow snapshots.
    pub async fn list_task_chains(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<TaskChain>, SessionError> {
        self.backend.list_task_chains(session_id).await
    }

    /// Accepts an exact latest plan and admits its isolated Implementer
    /// attempt.
    ///
    /// # Errors
    /// Returns an error for stale/ineligible plans, conflicts, or failed
    /// admission.
    pub async fn accept_task_chain_plan(
        &self,
        chain_id: &str,
        request: AcceptTaskChainPlanRequest,
    ) -> Result<(), SessionError> {
        self.backend.accept_task_chain_plan(chain_id, request).await
    }

    /// Explicitly revises, clarifies, or retries the current stage.
    ///
    /// # Errors
    /// Returns an error for unfinished attempts, conflicts, or failed
    /// admission.
    pub async fn continue_task_chain(
        &self,
        chain_id: &str,
        request: ContinueTaskChainRequest,
    ) -> Result<(), SessionError> {
        self.backend.continue_task_chain(chain_id, request).await
    }

    /// Requests cancellation of this workflow's current attempt.
    ///
    /// # Errors
    /// Returns an error when the workflow is missing or cancellation cannot
    /// persist.
    pub async fn cancel_task_chain(&self, chain_id: &str) -> Result<(), SessionError> {
        self.backend.cancel_task_chain(chain_id).await
    }

    /// Records a user reply once and lets the host decide what it means.
    ///
    /// While a plan awaits acceptance, the host decides through a model run
    /// whether the reply clearly approves the plan. Only clear approval with
    /// no requested changes starts the Implementer; anything else, including
    /// a failed or uncertain decision, revises the plan. In every other
    /// settled phase the reply continues the current role. Redelivering the
    /// same `reply_id` returns the original outcome.
    ///
    /// # Errors
    /// Returns [`SessionError::TaskChainNotFound`] when the session owns no
    /// workflow, [`SessionError::TaskChainBusy`] while an agent is working
    /// or an earlier reply is being decided, and other errors for invalid
    /// input, conflicts, or failed admission.
    pub async fn reply_to_task_chain(
        &self,
        session_id: &SessionId,
        request: ReplyToTaskChainRequest,
    ) -> Result<TaskChainReplyOutcome, SessionError> {
        self.backend.reply_to_task_chain(session_id, request).await
    }

    /// Creates an owned session capability backed by a shared host handle.
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
    }

    /// Admits an isolated role-bound attempt, acknowledging durable admission.
    /// Identical redeliveries do not execute again. Task completion is
    /// explicit.
    ///
    /// # Errors
    /// Returns an error for invalid input, unaccepted implementation plans,
    /// conflicting identity, or a session unable to accept work.
    pub async fn submit_agent_task(
        &self,
        session_id: &SessionId,
        request: crate::SubmitAgentTaskRequest,
    ) -> Result<(), SessionError> {
        self.backend.submit_agent_task(session_id, request).await
    }

    /// Reads the task's own responses. Attempt status remains authoritative.
    ///
    /// # Errors
    /// Returns an error when the host cannot load the isolated conversation.
    pub async fn get_agent_task_history(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::TaskAgentHistory>, SessionError> {
        self.backend.get_agent_task_history(task_id).await
    }

    /// Persists a task with zero attempts in an existing session, regardless
    /// of its execution status. Does not start work or change session state.
    /// Identical creation requests preserve the task and its history.
    ///
    /// # Errors
    /// Returns an error for empty inputs, conflicting identity, a missing
    /// session, or a host that cannot persist tasks.
    pub async fn create_task(
        &self,
        session_id: &SessionId,
        request: CreateTaskRequest,
    ) -> Result<(), SessionError> {
        self.backend.create_task(session_id, request).await
    }

    /// Lists all tasks owned by a session in creation order, including their
    /// attempt histories in one snapshot. Equal timestamps retain insertion
    /// order. Returns an empty list for a missing session or one without tasks.
    ///
    /// # Errors
    /// Returns an error when the host cannot read tasks.
    pub async fn list_tasks(&self, session_id: &SessionId) -> Result<Vec<Task>, SessionError> {
        self.backend.list_tasks(session_id).await
    }

    /// Admits one task attempt through the existing serialized session worker.
    /// Creates the task atomically if it does not already exist.
    /// Acceptance does not wait for execution or imply task completion.
    /// Identical deliveries are acknowledged without running again.
    ///
    /// # Errors
    /// Returns an error for conflicting identity, an unavailable host, a
    /// completed task, or a session that cannot accept a new durable turn.
    pub async fn submit_task(
        &self,
        session_id: &SessionId,
        request: SubmitTaskRequest,
    ) -> Result<(), SessionError> {
        self.backend.submit_task(session_id, request).await
    }

    /// Loads a task and all its attempts from durable storage.
    ///
    /// # Errors
    /// Returns an error when the host cannot read tasks.
    pub async fn get_task(&self, task_id: &str) -> Result<Option<Task>, SessionError> {
        self.backend.get_task(task_id).await
    }

    /// Explicitly accepts the outcome. Successful model execution alone never
    /// completes a task. Repeated completion retains its original timestamp.
    ///
    /// # Errors
    /// Returns an error if the task is missing, an attempt is unfinished, or
    /// the host cannot persist completion.
    pub async fn complete_task(&self, task_id: &str) -> Result<(), SessionError> {
        self.backend.complete_task(task_id).await
    }

    /// Creates one session and returns its stable identifier.
    ///
    /// # Errors
    /// Returns an error when the host cannot create the requested session.
    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.backend.create_session(request).await
    }

    /// Loads one complete session aggregate by identifier.
    ///
    /// # Errors
    /// Returns an error when persisted data cannot be loaded or decoded.
    pub async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Session>, SessionError> {
        self.backend.get_session(session_id).await
    }

    /// Sends a text message to one session.
    ///
    /// # Errors
    /// Returns an error when the session cannot accept or enqueue the message.
    pub async fn send_message(
        &self,
        session_id: &SessionId,
        message: impl Into<String> + Send,
    ) -> Result<(), SessionError> {
        self.backend.send_message(session_id, message.into()).await
    }

    /// Submits one durable turn directly to the serialized worker.
    ///
    /// # Errors
    /// Returns an error when the session is busy or cannot accept the turn.
    pub async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.backend
            .submit_durable_message(session_id, request)
            .await
    }

    /// Answers the complete current clarification-question set.
    ///
    /// # Errors
    /// Returns an error when the answers are stale, incomplete, or cannot be
    /// enqueued as a follow-up turn.
    pub async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        self.backend.answer_questions(session_id, request).await
    }

    /// Cancels one session through its host lifecycle workflow.
    ///
    /// # Errors
    /// Returns an error when the session does not exist or cannot be canceled
    /// in its current state.
    pub async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.cancel_session(session_id).await
    }

    /// Requests merge processing for one session.
    ///
    /// # Errors
    /// Returns an error when the session is not mergeable or queueing fails.
    pub async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.merge_session(session_id).await
    }

    /// Queues publication of one session and creates or refreshes its review
    /// request.
    ///
    /// # Errors
    /// Returns an error when queueing, branch publication, forge access, or
    /// persistence fails.
    pub async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.backend.create_review_request(session_id).await
    }
}

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;
