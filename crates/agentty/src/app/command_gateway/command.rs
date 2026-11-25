//! Typed mutation requests and transport-independent admission metadata.

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, MessageImage,
    ProjectScope, ReviewRequest, SessionError, SessionId,
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
    /// A paired device connected through remote control.
    Remote,
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
/// Payloads contain only application inputs. Provenance does not select the
/// reply's kind, but a paired device's command can be answered with a
/// refusal instead of being admitted; see [`Self::remote_scope`].
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

    /// Classifies the delivery for a paired device: creation reaches the
    /// project it names, or the session it inherits from, whose project
    /// creation then requires it to name; publication reaches its session
    /// under the branch name the host chooses, and a device may not name
    /// one.
    pub(super) fn remote_scope(&self) -> RemoteScope<'_> {
        match self {
            Self::Create { request, .. } => request.inherit_from_session_id.as_ref().map_or(
                RemoteScope::Project(ProjectScope::Project(request.project_id)),
                RemoteScope::Session,
            ),
            Self::Admission { request, .. } => request.remote_scope(),
            Self::Review { request, .. } if request.remote_branch_name.is_some() => {
                RemoteScope::NotServed
            }
            Self::Review { request, .. } => RemoteScope::Session(&request.session_id),
        }
    }

    /// Answers the delivery with `refusal` instead of admitting it: on its
    /// reply when it has one, or as the returned error.
    pub(super) fn refuse(self, refusal: SessionError) -> Result<(), SessionError> {
        match self {
            Self::Create { response_tx, .. } => Self::reply_with_refusal(response_tx, refusal),
            Self::Admission { response_tx, .. } => Self::reply_with_refusal(response_tx, refusal),
            Self::Review { response_tx, .. } => Self::reply_with_refusal(response_tx, refusal),
        }
    }

    /// Sends `refusal` on `response_tx`, or returns it to a caller without a
    /// reply.
    fn reply_with_refusal<ResultValue>(
        response_tx: Option<oneshot::Sender<Result<ResultValue, SessionError>>>,
        refusal: SessionError,
    ) -> Result<(), SessionError> {
        let Some(response_tx) = response_tx else {
            return Err(refusal);
        };
        let _ = response_tx.send(Err(refusal));

        Ok(())
    }
}

/// Application inputs for workflows that return an admission result.
pub(crate) enum GraphProposalCommand {
    Generate(Box<ag_session::GenerateGraphProposalRequest>),
    Act {
        id: String,
        version: u32,
        action: ag_session::GraphProposalAction,
    },
}

/// Application inputs for workflows that return an admission result.
pub(crate) enum SessionMutation {
    GraphProposal {
        command: GraphProposalCommand,
    },
    CancelExecutionGraph {
        request: ag_session::GraphControlRequest,
    },
    ResumeExecutionGraph {
        request: ag_session::ResumeExecutionGraphRequest,
    },
    /// Records an explicit provider-call increase for one launched budget.
    ExtendExecutionGraphBudget {
        request: ag_session::ExtendGraphBudgetRequest,
    },
    ContinueExecutionGraphNode {
        request: Box<ag_session::ContinueGraphNodeRequest>,
    },
    /// Starts an explicit static graph using existing worker paths.
    StartExecutionGraph {
        request: Box<ag_session::StartExecutionGraphRequest>,
    },
    /// Accepts one exact output through the trusted host admission path.
    AcceptExecutionGraphResult {
        reference: Box<ag_session::GraphResultRef>,
    },
    /// Applies one snapshot-bound inspector action from the session API
    /// through the mutation the terminal inspector admits for it.
    ActOnSessionGraph {
        request: Box<ag_session::SessionGraphActionRequest>,
    },
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
    /// Submits the terminal's prepared prompt without losing attachment or
    /// staging intent.
    Prompt {
        intent: MessageIntent,
        prompt: TurnPrompt,
        session_id: SessionId,
    },
    /// Starts a previously staged terminal draft.
    StartStaged {
        session_id: SessionId,
    },
    /// Sends one user message, with the images a remote device sent.
    SendMessage {
        images: Vec<MessageImage>,
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
    CompleteTask {
        task_id: String,
    },
    /// Answers one complete clarification-question set.
    AnswerQuestions {
        request: AnswerQuestionsRequest,
        session_id: SessionId,
    },
    /// Cancels one session.
    Cancel {
        session_id: SessionId,
    },
    /// Stops the running turn and returns the session to review.
    StopTurn {
        session_id: SessionId,
    },
    /// Rebases the session branch onto its base branch.
    Rebase {
        session_id: SessionId,
    },
    /// Changes the session's model, reasoning, style, or speed.
    UpdateSettings {
        change: ag_session::SessionSettingsChange,
        session_id: SessionId,
    },
    /// Enqueues one session for merge.
    Merge {
        session_id: SessionId,
    },
    /// Syncs the branch of the project `scope` names with its upstream.
    SyncProject {
        scope: ProjectScope,
    },
}

impl SessionMutation {
    /// Describes the routing target without exposing message or answer content.
    pub(super) fn target(&self) -> (&'static str, Option<&SessionId>) {
        match self {
            Self::GraphProposal {
                command: GraphProposalCommand::Generate(request),
            } => ("generate_graph_proposal", Some(&request.session_id)),
            Self::GraphProposal { .. } => ("act_on_graph_proposal", None),
            Self::StartExecutionGraph { request } => {
                ("start_execution_graph", Some(&request.graph.session_id))
            }
            Self::CancelExecutionGraph { .. } => ("cancel_execution_graph", None),
            Self::ResumeExecutionGraph { .. } => ("resume_execution_graph", None),
            Self::ExtendExecutionGraphBudget { .. } => ("extend_execution_graph_budget", None),
            Self::ContinueExecutionGraphNode { .. } => ("continue_execution_graph_node", None),
            Self::AcceptExecutionGraphResult { .. } => ("accept_execution_graph_result", None),
            Self::ActOnSessionGraph { request } => {
                ("act_on_session_graph", Some(&request.session_id))
            }
            Self::SubmitAgentTask { session_id, .. } => ("submit_agent_task", Some(session_id)),
            Self::AcceptAgentPlan { session_id, .. } => ("accept_agent_plan", Some(session_id)),
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
            Self::StopTurn { session_id } => ("stop_turn", Some(session_id)),
            Self::Rebase { session_id } => ("rebase", Some(session_id)),
            Self::UpdateSettings { session_id, .. } => ("update_settings", Some(session_id)),
            Self::Merge { session_id, .. } => ("merge", Some(session_id)),
            Self::SyncProject { .. } => ("sync_project", None),
        }
    }

    /// Classifies the mutation for a paired device: the operations the
    /// project owner approved for remote control, each with the session or
    /// project it must name. A new mutation is not served until it is
    /// listed here.
    ///
    /// Payloads count too: a device sends no attachments, which name files
    /// on this computer, so a question answer or graph proposal request that
    /// carries any is not served. A message's images are bytes this host
    /// stores itself, so they name no file.
    pub(super) fn remote_scope(&self) -> RemoteScope<'_> {
        match self {
            Self::GraphProposal {
                command: GraphProposalCommand::Generate(request),
            } if request.request.attachments.is_empty() => {
                RemoteScope::Session(&request.session_id)
            }
            Self::AnswerQuestions {
                request,
                session_id,
            } if request.attachments.is_empty() => RemoteScope::Session(session_id),
            Self::ActOnSessionGraph { request } => RemoteScope::Session(&request.session_id),
            Self::SendMessage { session_id, .. }
            | Self::Cancel { session_id }
            | Self::StopTurn { session_id }
            | Self::Rebase { session_id }
            | Self::UpdateSettings { session_id, .. }
            | Self::Merge { session_id } => RemoteScope::Session(session_id),
            Self::SyncProject { scope } => RemoteScope::Project(*scope),
            Self::GraphProposal {
                command: GraphProposalCommand::Generate(_) | GraphProposalCommand::Act { .. },
            }
            | Self::AnswerQuestions { .. }
            | Self::CancelExecutionGraph { .. }
            | Self::ResumeExecutionGraph { .. }
            | Self::ExtendExecutionGraphBudget { .. }
            | Self::ContinueExecutionGraphNode { .. }
            | Self::StartExecutionGraph { .. }
            | Self::AcceptExecutionGraphResult { .. }
            | Self::SubmitAgentTask { .. }
            | Self::AcceptAgentPlan { .. }
            | Self::Prompt { .. }
            | Self::StartStaged { .. }
            | Self::SubmitDurableMessage { .. }
            | Self::CreateTask { .. }
            | Self::SubmitTask { .. }
            | Self::CompleteTask { .. } => RemoteScope::NotServed,
        }
    }

    /// Whether the mutation changes a stored graph proposal or execution
    /// graph, which no session event announces when it is written.
    pub(super) fn changes_graph(&self) -> bool {
        matches!(
            self,
            Self::GraphProposal { .. }
                | Self::CancelExecutionGraph { .. }
                | Self::ResumeExecutionGraph { .. }
                | Self::ExtendExecutionGraphBudget { .. }
                | Self::ContinueExecutionGraphNode { .. }
                | Self::StartExecutionGraph { .. }
                | Self::AcceptExecutionGraphResult { .. }
                | Self::ActOnSessionGraph { .. }
        )
    }
}

/// What a request from a paired device reaches, which decides whether the
/// host serves it to remote control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteScope<'a> {
    /// Remote control does not serve the request.
    NotServed,
    /// The request reaches the project this scope names, not a named
    /// session: the active project, or a project the host lists.
    Project(ProjectScope),
    /// The request reads the list of projects the host lists.
    ProjectList,
    /// The request reaches this session, only while the host lists it: on
    /// the session page of a project the host lists.
    Session(&'a SessionId),
}

#[cfg(test)]
#[path = "command_test.rs"]
mod tests;
