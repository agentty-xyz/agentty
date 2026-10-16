//! Typed mutation requests and transport-independent admission metadata.

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, SessionError, SessionId,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::ReviewRequestCommand;
use crate::app::prompt_intent::PromptSessionMode;
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
    pub(super) id: String,
    pub(super) initiator: CommandInitiator,
    pub(super) request: SessionMutation,
}

impl SessionCommand {
    /// Assigns an admission identity while retaining any durable operation
    /// correlation.
    pub(crate) fn new(initiator: CommandInitiator, request: SessionMutation) -> Self {
        let correlation_id = match &request {
            SessionMutation::SubmitDurableMessage { request, .. } => {
                Some(request.operation_id.clone())
            }
            SessionMutation::CreateReviewRequest(request) => Some(request.operation_id.clone()),
            _ => None,
        };

        Self {
            correlation_id,
            id: Uuid::new_v4().to_string(),
            initiator,
            request,
        }
    }
}

/// A session workflow request with its existing acknowledgement contract.
pub(crate) enum SessionMutation {
    /// Creates one session, optionally acknowledging prepared workspace setup.
    Create {
        request: CreateSessionRequest,
        response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    },
    /// Submits the terminal's prepared prompt without losing attachment or
    /// staging intent.
    Prompt {
        prompt: TurnPrompt,
        session_id: SessionId,
        session_mode: PromptSessionMode,
    },
    /// Starts a previously staged terminal draft.
    StartStaged { session_id: SessionId },
    /// Sends one user message.
    SendMessage {
        message: String,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Submits one durable turn directly on the session worker.
    SubmitDurableMessage {
        request: DurableMessageRequest,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Answers one complete clarification-question set.
    AnswerQuestions {
        request: AnswerQuestionsRequest,
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Cancels one session.
    Cancel {
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Enqueues one session for merge.
    Merge {
        response_tx: oneshot::Sender<Result<(), SessionError>>,
        session_id: SessionId,
    },
    /// Queues publication of one session branch and creates or refreshes its
    /// review request.
    CreateReviewRequest(ReviewRequestCommand),
}

impl SessionMutation {
    /// Describes the routing target without exposing message or answer content.
    pub(super) fn target(&self) -> (&'static str, Option<&SessionId>) {
        match self {
            Self::Create { .. } => ("create", None),
            Self::Prompt { session_id, .. } => ("prompt", Some(session_id)),
            Self::StartStaged { session_id } => ("start_staged", Some(session_id)),
            Self::SendMessage { session_id, .. } => ("send_message", Some(session_id)),
            Self::SubmitDurableMessage { session_id, .. } => {
                ("submit_durable_message", Some(session_id))
            }
            Self::AnswerQuestions { session_id, .. } => ("answer_questions", Some(session_id)),
            Self::Cancel { session_id, .. } => ("cancel", Some(session_id)),
            Self::Merge { session_id, .. } => ("merge", Some(session_id)),
            Self::CreateReviewRequest(request) => {
                ("create_review_request", Some(&request.session_id))
            }
        }
    }
}
