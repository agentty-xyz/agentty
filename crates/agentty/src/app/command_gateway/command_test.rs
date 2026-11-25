//! Remote-control classification of every session mutation and delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPromptAttachment};
use ag_session::{
    AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection, AnswerQuestionsRequest,
    ContinueGraphNodeRequest, CreateSessionMode, CreateSessionRequest, CreateTaskRequest,
    DurableMessageRequest, DurableMessageVisibility, ExtendGraphBudgetRequest,
    GenerateGraphProposalRequest, GraphControlRequest, GraphNodeContinuation, GraphProposalAction,
    GraphResultRef, ProjectScope, ResumeExecutionGraphRequest, SessionError, SessionGraphAction,
    SessionGraphActionRequest, SessionId, SessionSettingsChange, StartExecutionGraphRequest,
    SubmitAgentTaskRequest, SubmitTaskRequest, TaskAgentAssignment, TaskAgentInput, TaskAgentRole,
};
use tokio::sync::oneshot;

use super::{CommandDelivery, GraphProposalCommand, RemoteScope, SessionMutation};
use crate::app::command_gateway::ReviewRequestCommand;
use crate::app::execution_graph::tests::{graph, run_request};
use crate::app::message_admission::MessageIntent;
use crate::domain::turn_prompt::TurnPrompt;

#[test]
fn remote_control_is_served_exactly_the_mutations_the_owner_approved() {
    // Arrange
    let session_id = SessionId::from("sess");
    let served = RemoteScope::Session(&session_id);
    let rows = [
        (generate_graph_proposal(), served),
        (
            generate_graph_proposal_with_attachment(),
            RemoteScope::NotServed,
        ),
        (act_on_session_graph(), served),
        (send_message(), served),
        (answer_questions(), served),
        (answer_questions_with_attachment(), RemoteScope::NotServed),
        (cancel(), served),
        (stop_turn(), served),
        (rebase(), served),
        (update_settings(), served),
        (merge(), served),
        (
            SessionMutation::SyncProject {
                scope: ProjectScope::Active,
            },
            RemoteScope::Project(ProjectScope::Active),
        ),
        // A sync reaches the project it names, which a paired device may
        // name only while it is the active one.
        (
            SessionMutation::SyncProject {
                scope: ProjectScope::Project(7),
            },
            RemoteScope::Project(ProjectScope::Project(7)),
        ),
        (act_on_graph_proposal(), RemoteScope::NotServed),
        (cancel_execution_graph(), RemoteScope::NotServed),
        (resume_execution_graph(), RemoteScope::NotServed),
        (extend_execution_graph_budget(), RemoteScope::NotServed),
        (continue_execution_graph_node(), RemoteScope::NotServed),
        (start_execution_graph(), RemoteScope::NotServed),
        (accept_execution_graph_result(), RemoteScope::NotServed),
        (submit_agent_task(), RemoteScope::NotServed),
        (accept_agent_plan(), RemoteScope::NotServed),
        (prompt(), RemoteScope::NotServed),
        (start_staged(), RemoteScope::NotServed),
        (submit_durable_message(), RemoteScope::NotServed),
        (create_task(), RemoteScope::NotServed),
        (submit_task(), RemoteScope::NotServed),
        (complete_task(), RemoteScope::NotServed),
    ];

    // Act
    let classified = rows
        .iter()
        .map(|(mutation, expected)| {
            (
                variant_number(mutation),
                mutation.target().0,
                mutation.remote_scope(),
                expected,
            )
        })
        .collect::<Vec<_>>();

    // Assert
    let covered = classified
        .iter()
        .map(|(number, ..)| *number)
        .collect::<BTreeSet<_>>();
    assert_eq!(covered, (0..MUTATION_VARIANTS).collect::<BTreeSet<_>>());
    for (_, kind, scope, expected) in classified {
        assert_eq!(scope, *expected, "{kind}");
    }
}

/// Number of [`SessionMutation`] shapes [`variant_number`] tells apart.
const MUTATION_VARIANTS: usize = 25;

/// Numbers each mutation shape. A new variant fails to compile here until
/// it is numbered; keep [`MUTATION_VARIANTS`] one past the highest number,
/// and the coverage check above then fails until the table has a row for
/// it.
fn variant_number(mutation: &SessionMutation) -> usize {
    match mutation {
        SessionMutation::GraphProposal {
            command: GraphProposalCommand::Generate(_),
        } => 0,
        SessionMutation::GraphProposal {
            command: GraphProposalCommand::Act { .. },
        } => 1,
        SessionMutation::CancelExecutionGraph { .. } => 2,
        SessionMutation::ResumeExecutionGraph { .. } => 3,
        SessionMutation::ExtendExecutionGraphBudget { .. } => 4,
        SessionMutation::ContinueExecutionGraphNode { .. } => 5,
        SessionMutation::StartExecutionGraph { .. } => 6,
        SessionMutation::AcceptExecutionGraphResult { .. } => 7,
        SessionMutation::ActOnSessionGraph { .. } => 8,
        SessionMutation::SubmitAgentTask { .. } => 9,
        SessionMutation::AcceptAgentPlan { .. } => 10,
        SessionMutation::Prompt { .. } => 11,
        SessionMutation::StartStaged { .. } => 12,
        SessionMutation::SendMessage { .. } => 13,
        SessionMutation::SubmitDurableMessage { .. } => 14,
        SessionMutation::CreateTask { .. } => 15,
        SessionMutation::SubmitTask { .. } => 16,
        SessionMutation::CompleteTask { .. } => 17,
        SessionMutation::AnswerQuestions { .. } => 18,
        SessionMutation::Cancel { .. } => 19,
        SessionMutation::StopTurn { .. } => 20,
        SessionMutation::Rebase { .. } => 21,
        SessionMutation::UpdateSettings { .. } => 22,
        SessionMutation::Merge { .. } => 23,
        SessionMutation::SyncProject { .. } => 24,
    }
}

#[test]
fn remote_creation_reaches_the_project_and_publication_its_session() {
    // Arrange
    let session_id = SessionId::from("sess");
    let creation = create_delivery(None);
    let inheriting_creation = create_delivery(Some(session_id.clone()));
    let publication = CommandDelivery::Review {
        request: ReviewRequestCommand::new(session_id.clone(), None),
        response_tx: None,
    };
    let named_branch_publication = CommandDelivery::Review {
        request: ReviewRequestCommand::new(session_id.clone(), Some("wt/chosen".into())),
        response_tx: None,
    };
    let admission = CommandDelivery::Admission {
        request: cancel(),
        response_tx: None,
    };

    // Act
    let scopes = [
        creation.remote_scope(),
        inheriting_creation.remote_scope(),
        publication.remote_scope(),
        named_branch_publication.remote_scope(),
        admission.remote_scope(),
    ];

    // Assert
    assert_eq!(
        scopes,
        [
            RemoteScope::Project(ProjectScope::Project(1)),
            RemoteScope::Session(&session_id),
            RemoteScope::Session(&session_id),
            RemoteScope::NotServed,
            RemoteScope::Session(&session_id),
        ]
    );
}

#[tokio::test]
async fn refused_deliveries_answer_their_reply_or_return_the_refusal() {
    // Arrange
    let (create_tx, create_rx) = oneshot::channel();
    let (admission_tx, admission_rx) = oneshot::channel();
    let (review_tx, review_rx) = oneshot::channel();
    let deliveries = [
        CommandDelivery::Create {
            request: create_request(None),
            response_tx: Some(create_tx),
        },
        CommandDelivery::Admission {
            request: cancel(),
            response_tx: Some(admission_tx),
        },
        CommandDelivery::Review {
            request: ReviewRequestCommand::new("sess".into(), None),
            response_tx: Some(review_tx),
        },
    ];
    let unanswered = CommandDelivery::Admission {
        request: cancel(),
        response_tx: None,
    };

    // Act
    let answered = deliveries.map(|delivery| delivery.refuse(SessionError::NotFound));
    let returned = unanswered.refuse(SessionError::NotFound);

    // Assert
    assert_eq!(answered, [Ok(()), Ok(()), Ok(())]);
    assert_eq!(
        create_rx.await.expect("creation reply"),
        Err(SessionError::NotFound)
    );
    assert_eq!(
        admission_rx.await.expect("admission reply"),
        Err(SessionError::NotFound)
    );
    assert_eq!(
        review_rx.await.expect("review reply"),
        Err(SessionError::NotFound)
    );
    assert_eq!(returned, Err(SessionError::NotFound));
}

fn create_delivery(inherit_from_session_id: Option<SessionId>) -> CommandDelivery {
    CommandDelivery::Create {
        request: create_request(inherit_from_session_id),
        response_tx: None,
    }
}

fn create_request(inherit_from_session_id: Option<SessionId>) -> CreateSessionRequest {
    CreateSessionRequest {
        inherit_from_session_id,
        mode: CreateSessionMode::Regular,
        project_id: 1,
    }
}

fn generate_graph_proposal() -> SessionMutation {
    SessionMutation::GraphProposal {
        command: GraphProposalCommand::Generate(Box::new(GenerateGraphProposalRequest::from_text(
            "proposal".into(),
            "sess".into(),
            "Plan it".into(),
        ))),
    }
}

/// A proposal request that carries a file on this computer.
fn generate_graph_proposal_with_attachment() -> SessionMutation {
    let mut request =
        GenerateGraphProposalRequest::from_text("proposal".into(), "sess".into(), "Plan it".into());
    request.request.attachments.push(attachment());

    SessionMutation::GraphProposal {
        command: GraphProposalCommand::Generate(Box::new(request)),
    }
}

fn attachment() -> TurnPromptAttachment {
    TurnPromptAttachment {
        local_image_path: "/tmp/sketch.png".into(),
        placeholder: "[Image #1]".into(),
    }
}

fn act_on_session_graph() -> SessionMutation {
    SessionMutation::ActOnSessionGraph {
        request: Box::new(SessionGraphActionRequest {
            action: SessionGraphAction::Cancel,
            graph_id: "graph".into(),
            session_id: "sess".into(),
            snapshot: "snapshot".into(),
        }),
    }
}

fn send_message() -> SessionMutation {
    SessionMutation::SendMessage {
        images: Vec::new(),
        message: "Hello".into(),
        session_id: "sess".into(),
    }
}

fn answer_questions() -> SessionMutation {
    SessionMutation::AnswerQuestions {
        request: AnswerQuestionsRequest {
            answers: Vec::new(),
            attachments: Vec::new(),
            graph_question_target: None,
        },
        session_id: "sess".into(),
    }
}

/// Question answers that carry a file on this computer.
fn answer_questions_with_attachment() -> SessionMutation {
    SessionMutation::AnswerQuestions {
        request: AnswerQuestionsRequest {
            answers: Vec::new(),
            attachments: vec![attachment()],
            graph_question_target: None,
        },
        session_id: "sess".into(),
    }
}

fn cancel() -> SessionMutation {
    SessionMutation::Cancel {
        session_id: "sess".into(),
    }
}

fn stop_turn() -> SessionMutation {
    SessionMutation::StopTurn {
        session_id: "sess".into(),
    }
}

fn rebase() -> SessionMutation {
    SessionMutation::Rebase {
        session_id: "sess".into(),
    }
}

fn update_settings() -> SessionMutation {
    SessionMutation::UpdateSettings {
        change: SessionSettingsChange::default(),
        session_id: "sess".into(),
    }
}

fn merge() -> SessionMutation {
    SessionMutation::Merge {
        session_id: "sess".into(),
    }
}

fn act_on_graph_proposal() -> SessionMutation {
    SessionMutation::GraphProposal {
        command: GraphProposalCommand::Act {
            action: GraphProposalAction::Approve,
            id: "proposal".into(),
            version: 1,
        },
    }
}

fn graph_control() -> GraphControlRequest {
    GraphControlRequest {
        graph_id: "graph".into(),
        revision: NonZeroU64::MIN,
    }
}

fn cancel_execution_graph() -> SessionMutation {
    SessionMutation::CancelExecutionGraph {
        request: graph_control(),
    }
}

fn resume_execution_graph() -> SessionMutation {
    SessionMutation::ResumeExecutionGraph {
        request: ResumeExecutionGraphRequest {
            graph: graph_control(),
        },
    }
}

fn extend_execution_graph_budget() -> SessionMutation {
    SessionMutation::ExtendExecutionGraphBudget {
        request: ExtendGraphBudgetRequest {
            action_id: "extension".into(),
            additional_calls: 1,
            budget_id: "calls".into(),
            graph: graph_control(),
        },
    }
}

fn continue_execution_graph_node() -> SessionMutation {
    SessionMutation::ContinueExecutionGraphNode {
        request: Box::new(ContinueGraphNodeRequest {
            action_id: "continuation".into(),
            execution: run_request().dispatch.record.execution,
            graph_id: "graph".into(),
            kind: GraphNodeContinuation::Retry,
            node_id: "first".into(),
            revision: NonZeroU64::MIN,
        }),
    }
}

fn start_execution_graph() -> SessionMutation {
    SessionMutation::StartExecutionGraph {
        request: Box::new(StartExecutionGraphRequest {
            graph: graph(),
            provider_call_limits: BTreeMap::from([("calls".into(), 1)]),
        }),
    }
}

fn accept_execution_graph_result() -> SessionMutation {
    SessionMutation::AcceptExecutionGraphResult {
        reference: Box::new(GraphResultRef {
            execution: run_request().dispatch.record.execution,
            graph_id: "graph".into(),
            graph_revision: NonZeroU64::MIN,
            id: "result".into(),
            node_id: "first".into(),
            output_name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
            version: NonZeroU64::MIN,
        }),
    }
}

fn submit_agent_task() -> SessionMutation {
    SessionMutation::SubmitAgentTask {
        request: Box::new(SubmitAgentTaskRequest {
            assignment: TaskAgentAssignment {
                agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
                definition: ag_session::AgentDefinition::bundled("planner").expect("bundled agent"),
                role: TaskAgentRole::Planner,
            },
            attempt_id: "attempt".into(),
            input: TaskAgentInput {
                accepted_plan: None,
                attachments: Vec::new(),
                clarification: None,
                user_request: "Plan it".into(),
            },
            objective: "Produce a plan".into(),
            task_id: "task".into(),
            visibility: DurableMessageVisibility::Hidden,
        }),
        session_id: "sess".into(),
    }
}

fn accept_agent_plan() -> SessionMutation {
    SessionMutation::AcceptAgentPlan {
        request: AcceptTaskPlanRequest {
            attempt_id: "attempt".into(),
            task_id: "task".into(),
        },
        session_id: "sess".into(),
    }
}

fn prompt() -> SessionMutation {
    SessionMutation::Prompt {
        intent: MessageIntent::FollowUp,
        prompt: TurnPrompt::from_text("Hello".into()),
        session_id: "sess".into(),
    }
}

fn start_staged() -> SessionMutation {
    SessionMutation::StartStaged {
        session_id: "sess".into(),
    }
}

fn submit_durable_message() -> SessionMutation {
    SessionMutation::SubmitDurableMessage {
        request: DurableMessageRequest {
            message: "Hello".into(),
            operation_id: "operation".into(),
            visibility: DurableMessageVisibility::Hidden,
        },
        session_id: "sess".into(),
    }
}

fn create_task() -> SessionMutation {
    SessionMutation::CreateTask {
        request: CreateTaskRequest {
            objective: "Produce a plan".into(),
            task_id: "task".into(),
        },
        session_id: "sess".into(),
    }
}

fn submit_task() -> SessionMutation {
    SessionMutation::SubmitTask {
        request: SubmitTaskRequest {
            attempt_id: "attempt".into(),
            message: "Hello".into(),
            objective: "Produce a plan".into(),
            task_id: "task".into(),
            visibility: DurableMessageVisibility::Hidden,
        },
        session_id: "sess".into(),
    }
}

fn complete_task() -> SessionMutation {
    SessionMutation::CompleteTask {
        task_id: "task".into(),
    }
}
