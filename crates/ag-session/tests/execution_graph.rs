//! Public graph contracts without storage, workers, or providers.

use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPromptAttachment};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    GraphCompletionGate, GraphContext, GraphContractError, GraphDependency, GraphExecutionProfile,
    GraphExecutionRef, GraphInputBinding, GraphNode, GraphOutput, GraphPostProcessing, GraphResult,
    GraphResultRef, GraphResultState, GraphWorkspaceAccess,
};

#[test]
fn plan_handoff_pins_an_explicitly_accepted_attempt_without_retargeting_it() {
    // Arrange
    let mut planner = node("plan", Some("planner-task"));
    planner.request.attachments.push(TurnPromptAttachment {
        local_image_path: "reference.png".into(),
        placeholder: "[Image #1]".into(),
    });
    planner.request.text.push_str(" [Image #1]");
    let mut implementer = node("implement", Some("implementer-task"));
    implementer.definition.read_only = false;
    implementer.execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    implementer.execution.post_processing = GraphPostProcessing::SessionWorkflow;
    consume(&mut implementer, "plan", true);
    let graph = graph(vec![planner, implementer]);
    let mut result = GraphResult {
        reference: GraphResultRef {
            execution: GraphExecutionRef::TaskAttempt {
                attempt_id: "planner-attempt-1".into(),
                operation_id: "task-attempt:planner-attempt-1".into(),
                task_id: "planner-task".into(),
            },
            graph_id: graph.id.clone(),
            graph_revision: graph.revision,
            id: "plan-result-1".into(),
            node_id: "plan".into(),
            output_name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
            version: NonZeroU64::MIN,
        },
        state: GraphResultState::Validated,
    };
    assert_eq!(graph.validate(), Ok(()));
    assert_eq!(
        graph.bind_input("implement", "plan", &result),
        Err(GraphContractError::UnsatisfiedGate),
    );

    // Act
    result.state = GraphResultState::Accepted;
    let input = graph
        .bind_input("implement", "plan", &result)
        .expect("accepted version");
    result.reference.id = "plan-result-2".into();
    result.reference.version = NonZeroU64::new(2).expect("nonzero");
    result.state = GraphResultState::Validated;

    // Assert
    assert_eq!(input.result.id, "plan-result-1");
    assert_eq!(input.result.version, NonZeroU64::MIN);
    assert_eq!(graph.nodes[0].request.attachments.len(), 1);
    assert_eq!(
        input.result.execution,
        GraphExecutionRef::TaskAttempt {
            attempt_id: "planner-attempt-1".into(),
            operation_id: "task-attempt:planner-attempt-1".into(),
            task_id: "planner-task".into(),
        },
    );
    assert_eq!(
        graph.bind_input("implement", "plan", &result),
        Err(GraphContractError::UnsatisfiedGate),
    );
}

#[test]
fn review_graph_supports_fresh_parallel_branches_and_explicit_join_inputs() {
    // Arrange
    let first = node("batch-a", None);
    let second = node("batch-b", None);
    let mut cross_file = node("cross-file", None);
    consume(&mut cross_file, "batch-a", false);
    consume(&mut cross_file, "batch-b", false);
    let mut consolidate = node("consolidate", None);
    consume(&mut consolidate, "cross-file", false);
    let graph = graph(vec![consolidate, first, cross_file, second]);
    let result = GraphResult {
        reference: GraphResultRef {
            execution: GraphExecutionRef::UtilityRun {
                run_id: "batch-a-run".into(),
            },
            graph_id: graph.id.clone(),
            graph_revision: graph.revision,
            id: "batch-a-result".into(),
            node_id: "batch-a".into(),
            output_name: "response".into(),
            schema: ProtocolRequestProfile::FocusedReview,
            version: NonZeroU64::MIN,
        },
        state: GraphResultState::Validated,
    };

    // Act
    let validation = graph.validate();
    let input = graph.bind_input("cross-file", "batch-a", &result);

    // Assert
    assert_eq!(validation, Ok(()));
    assert_eq!(
        input.expect("validated batch output").result,
        result.reference
    );
    assert_eq!(graph.nodes[2].inputs.len(), 2);
    for node in &graph.nodes {
        assert_eq!(node.execution.context, GraphContext::Fresh);
        assert_eq!(
            node.execution.workspace_access,
            GraphWorkspaceAccess::SharedRead
        );
        assert_eq!(
            node.execution.shared_budget_id.as_deref(),
            Some("shared-review-budget")
        );
        assert_eq!(
            node.execution.response_profile,
            ProtocolRequestProfile::FocusedReview
        );
    }
}

fn graph(nodes: Vec<GraphNode>) -> ExecutionGraph {
    ExecutionGraph {
        id: "workflow".into(),
        nodes,
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
    }
}

fn node(id: &str, task_id: Option<&str>) -> GraphNode {
    let profile = if task_id.is_some() {
        ProtocolRequestProfile::SessionTurn
    } else {
        ProtocolRequestProfile::FocusedReview
    };

    GraphNode {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        definition: AgentDefinition {
            description: "Captured workflow instructions".into(),
            id: id.into(),
            name: id.into(),
            prompt: "Work only from the explicit inputs".into(),
            read_only: true,
        },
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("session".into()),
        execution: GraphExecutionProfile {
            context: task_id.map_or(GraphContext::Fresh, |task_id| {
                GraphContext::TaskConversation {
                    task_id: task_id.into(),
                }
            }),
            post_processing: GraphPostProcessing::None,
            response_profile: profile,
            shared_budget_id: Some("shared-review-budget".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: "Deliver the requested outcome".into(),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: profile,
        }],
        request: "Inspect the captured inputs and perform this stage".into(),
    }
}

fn consume(consumer: &mut GraphNode, producer: &str, acceptance: bool) {
    let output_name = "response".to_string();
    consumer.dependencies.push(GraphDependency {
        gate: if acceptance {
            GraphCompletionGate::ResultAccepted {
                output_name: output_name.clone(),
            }
        } else {
            GraphCompletionGate::ResultValidated {
                output_name: output_name.clone(),
            }
        },
        node_id: producer.into(),
    });
    consumer.inputs.push(GraphInputBinding {
        name: producer.into(),
        output_name,
        producer_node_id: producer.into(),
    });
}
