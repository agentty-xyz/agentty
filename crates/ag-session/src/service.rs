//! Programmatic session orchestration facade and host backend port.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::SessionError;
use crate::model::{ReviewRequest, Session, SessionId};
use crate::{CreateTaskRequest, SubmitTaskRequest, Task};

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

impl SessionService {
    /// Creates an owned session capability backed by a shared host handle.
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
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
