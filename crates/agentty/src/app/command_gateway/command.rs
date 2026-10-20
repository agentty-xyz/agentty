//! Typed mutation requests and transport-independent admission metadata.

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, ReplyToTaskChainRequest,
    ReviewRequest, SessionError, SessionId, TaskChainReplyOutcome,
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
            CommandDelivery::Admission {
                request: SessionMutation::StartTaskChain { request, .. },
                ..
            } => Some(format!("task-attempt:{}", request.planner_attempt_id)),
            CommandDelivery::Admission {
                request: SessionMutation::AcceptTaskChainPlan { request, .. },
                ..
            } => Some(format!("task-attempt:{}", request.attempt_id)),
            CommandDelivery::Admission {
                request: SessionMutation::ContinueTaskChain { request, .. },
                ..
            } => Some(format!("task-attempt:{}", request.attempt_id)),
            CommandDelivery::Review { request, .. } => Some(request.operation_id.clone()),
            CommandDelivery::TaskChainReply { request, .. } => {
                Some(format!("task-chain-reply:{}", request.reply_id()))
            }
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
    /// A workflow reply may transfer its reply to a background decision,
    /// which applies the decided action through this gateway again.
    TaskChainReply {
        request: TaskChainReplyCommand,
        response_tx: oneshot::Sender<Result<TaskChainReplyOutcome, SessionError>>,
    },
}

impl CommandDelivery {
    /// Describes the routing target without exposing private request content.
    pub(super) fn target(&self) -> (&'static str, Option<&SessionId>) {
        match self {
            Self::Create { .. } => ("create", None),
            Self::Admission { request, .. } => request.target(),
            Self::Review { request, .. } => ("create_review_request", Some(&request.session_id)),
            Self::TaskChainReply {
                request: TaskChainReplyCommand::Reply { session_id, .. },
                ..
            } => ("reply_to_task_chain", Some(session_id)),
            Self::TaskChainReply {
                request: TaskChainReplyCommand::Apply(action),
                ..
            } => ("apply_task_chain_reply", Some(&action.session_id)),
        }
    }
}

/// Inputs for natural-language workflow replies.
pub(crate) enum TaskChainReplyCommand {
    /// Records a public reply once and decides its workflow action.
    Reply {
        request: ReplyToTaskChainRequest,
        session_id: SessionId,
    },
    /// Admits an action decided for an already recorded reply.
    Apply(TaskChainReplyAction),
}

impl TaskChainReplyCommand {
    /// Returns the delivery identity shared by a reply and its action.
    pub(super) fn reply_id(&self) -> &str {
        match self {
            Self::Reply { request, .. } => &request.reply_id,
            Self::Apply(action) => &action.reply_id,
        }
    }
}

/// One decided workflow action, anchored to the attempt observed when the
/// reply was routed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskChainReplyAction {
    pub(crate) chain_id: String,
    pub(crate) expected_attempt_id: String,
    pub(crate) message: String,
    pub(crate) outcome: TaskChainReplyOutcome,
    pub(crate) reply_id: String,
    pub(crate) session_id: SessionId,
}

/// Application inputs for workflows that return an admission result.
pub(crate) enum SessionMutation {
    /// Admits one role-bound attempt with an isolated conversation.
    SubmitAgentTask {
        request: Box<ag_session::SubmitAgentTaskRequest>,
        session_id: SessionId,
    },
    /// Accepts one exact standalone Planner version without dispatching work.
    AcceptAgentPlan {
        request: ag_session::AcceptTaskPlanRequest,
        session_id: SessionId,
    },
    /// Starts a static workflow with one captured role selection.
    StartTaskChain {
        request: ag_session::StartPlanThenImplementRequest,
        session_id: SessionId,
    },
    /// Atomically records acceptance and admits the first implementation.
    AcceptTaskChainPlan {
        chain_id: String,
        request: ag_session::AcceptTaskChainPlanRequest,
    },
    /// Explicitly advances the current role using a new attempt identity.
    ContinueTaskChain {
        chain_id: String,
        request: ag_session::ContinueTaskChainRequest,
    },
    /// Requests cancellation without canceling unrelated session work.
    CancelTaskChain { chain_id: String },
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
            Self::AcceptAgentPlan { session_id, .. } => ("accept_agent_plan", Some(session_id)),
            Self::StartTaskChain { session_id, .. } => ("start_task_chain", Some(session_id)),
            Self::AcceptTaskChainPlan { .. } => ("accept_task_chain_plan", None),
            Self::ContinueTaskChain { .. } => ("continue_task_chain", None),
            Self::CancelTaskChain { .. } => ("cancel_task_chain", None),
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
