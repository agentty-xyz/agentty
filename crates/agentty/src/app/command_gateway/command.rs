//! Typed mutation requests and transport-independent admission metadata.

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, ReviewRequest,
    SessionError, SessionId,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::ReviewRequestCommand;
use crate::app::message_admission::MessageIntent;
use crate::domain::turn_prompt::TurnPrompt;

/// The caller that initiated a request, independent of its transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandInitiator {
    SessionView,
    SessionApi,
}

/// One admission attempt. Its identity is not a durable-operation deduplication
/// key; correlation links it to an operation when one already exists.
pub(crate) struct SessionCommand {
    pub(super) correlation_id: Option<String>,
    pub(super) delivery: CommandDelivery,
    pub(super) id: String,
    pub(super) initiator: CommandInitiator,
}

impl SessionCommand {
    /// Assigns an admission identity while retaining any durable operation
    /// correlation.
    pub(crate) fn new(initiator: CommandInitiator, delivery: CommandDelivery) -> Self {
        let correlation_id = match &delivery {
            CommandDelivery::Admission {
                request: SessionMutation::SubmitDurableMessage { request, .. },
                ..
            } => Some(request.operation_id.clone()),
            CommandDelivery::Admission {
                request: SessionMutation::SubmitTask { request, .. },
                ..
            } => Some(request.operation_id()),
            CommandDelivery::Admission {
                request: SessionMutation::SubmitAgentTask { request, .. },
                ..
            } => Some(format!("task-attempt:{}", request.attempt_id)),
            CommandDelivery::Review { request, .. } => Some(request.operation_id.clone()),
            _ => None,
        };

        Self {
            correlation_id,
            id: Uuid::new_v4().to_string(),
            initiator,
            delivery,
        }
    }
}

/// Transport-owned replies, grouped by their result type and lifetime.
/// Payloads contain only application inputs; provenance does not select a
/// reply.
pub(crate) enum CommandDelivery {
    /// Creation may retain its reply until workspace preparation completes.
    Create {
        request: CreateSessionRequest,
        response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    },
    /// Ordinary mutations acknowledge admission, not worker completion.
    /// Direct callers receive the result from foreground dispatch instead.
    Admission {
        request: SessionMutation,
        response_tx: Option<oneshot::Sender<Result<(), SessionError>>>,
    },
    /// Publication transfers its optional completion reply to the worker.
    Review {
        request: ReviewRequestCommand,
        response_tx: Option<oneshot::Sender<Result<ReviewRequest, SessionError>>>,
    },
}

impl CommandDelivery {
    /// Describes the routing target without exposing private request content.
    pub(super) fn target(&self) -> (&'static str, Option<&SessionId>) {
        match self {
            Self::Create { .. } => ("create", None),
            Self::Admission { request, .. } => request.target(),
            Self::Review { request, .. } => ("create_review_request", Some(&request.session_id)),
        }
    }
}

/// Application inputs for workflows that return an admission result.
pub(crate) enum SessionMutation {
    /// Admits one role-bound attempt with an isolated conversation.
    SubmitAgentTask {
        request: Box<ag_session::SubmitAgentTaskRequest>,
        session_id: SessionId,
    },
    /// Submits the terminal's prepared prompt without losing attachment or
    /// staging intent.
    Prompt {
        intent: MessageIntent,
        prompt: TurnPrompt,
        session_id: SessionId,
    },
    /// Starts a previously staged terminal draft.
    StartStaged { session_id: SessionId },
    /// Sends one user message.
    SendMessage {
        message: String,
        session_id: SessionId,
    },
    /// Submits one durable turn directly on the session worker.
    SubmitDurableMessage {
        request: DurableMessageRequest,
        session_id: SessionId,
    },
    /// Persists a desired outcome without admitting an attempt.
    CreateTask {
        request: ag_session::CreateTaskRequest,
        session_id: SessionId,
    },
    /// Admits one explicit task attempt with durable identity.
    SubmitTask {
        request: ag_session::SubmitTaskRequest,
        session_id: SessionId,
    },
    /// Records the caller's acceptance of a desired outcome.
    CompleteTask { task_id: String },
    /// Answers one complete clarification-question set.
    AnswerQuestions {
        request: AnswerQuestionsRequest,
        session_id: SessionId,
    },
    /// Cancels one session.
    Cancel { session_id: SessionId },
    /// Enqueues one session for merge.
    Merge { session_id: SessionId },
}

impl SessionMutation {
    /// Describes the routing target without exposing message or answer content.
    pub(super) fn target(&self) -> (&'static str, Option<&SessionId>) {
        match self {
            Self::SubmitAgentTask { session_id, .. } => ("submit_agent_task", Some(session_id)),
            Self::Prompt { session_id, .. } => ("prompt", Some(session_id)),
            Self::StartStaged { session_id } => ("start_staged", Some(session_id)),
            Self::SendMessage { session_id, .. } => ("send_message", Some(session_id)),
            Self::SubmitDurableMessage { session_id, .. } => {
                ("submit_durable_message", Some(session_id))
            }
            Self::CreateTask { session_id, .. } => ("create_task", Some(session_id)),
            Self::SubmitTask { session_id, .. } => ("submit_task", Some(session_id)),
            Self::CompleteTask { .. } => ("complete_task", None),
            Self::AnswerQuestions { session_id, .. } => ("answer_questions", Some(session_id)),
            Self::Cancel { session_id, .. } => ("cancel", Some(session_id)),
            Self::Merge { session_id, .. } => ("merge", Some(session_id)),
        }
    }
}
