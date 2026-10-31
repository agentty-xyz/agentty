use std::num::NonZeroU64;

use ag_protocol::ProtocolRequestProfile;

use crate::execution_graph::test_support::{graph, reader, result};
use crate::{
    EnvId, ExecutionGraph, GraphCompletionGate, GraphContext, GraphContractError, GraphDependency,
    GraphExecutionRef, GraphNode, GraphPostProcessing, GraphResult, GraphResultState,
    GraphWorkspaceAccess,
};

type InvalidGraphCase = (fn(&mut ExecutionGraph), GraphContractError);

#[test]
fn graphs_reject_empty_and_conflicting_identities() {
    // Arrange
    type Mutation = fn(&mut ExecutionGraph);
    let cases: &[(Mutation, GraphContractError)] = &[
        (
            |graph| graph.id.clear(),
            GraphContractError::EmptyField("graph_id"),
        ),
        (
            |graph| graph.session_id = " ".into(),
            GraphContractError::EmptyField("session_id"),
        ),
        (|graph| graph.nodes.clear(), GraphContractError::EmptyGraph),
        (
            |graph| graph.nodes[0].id.clear(),
            GraphContractError::EmptyField("node_id"),
        ),
        (
            |graph| graph.nodes[0].definition.id.clear(),
            GraphContractError::EmptyField("definition.id"),
        ),
        (
            |graph| graph.nodes[0].definition.prompt.clear(),
            GraphContractError::EmptyField("definition.prompt"),
        ),
        (
            |graph| graph.nodes[0].objective.clear(),
            GraphContractError::EmptyField("objective"),
        ),
        (
            |graph| graph.nodes[0].request.text.clear(),
            GraphContractError::EmptyField("request"),
        ),
        (
            |graph| graph.nodes.push(graph.nodes[0].clone()),
            duplicate("node", "first"),
        ),
        (
            |graph| graph.nodes[0].env_id = EnvId::SessionWorktree("other".into()),
            GraphContractError::ForeignEnv,
        ),
        (
            |graph| {
                task_node(&mut graph.nodes[0], "same-task");
                task_node(&mut graph.nodes[1], "same-task");
            },
            duplicate("task", "same-task"),
        ),
    ];

    // Act and Assert
    assert_invalid_graphs(cases);
}

#[test]
fn graphs_reject_invalid_output_and_input_declarations() {
    // Arrange
    type Mutation = fn(&mut ExecutionGraph);
    let cases: &[(Mutation, GraphContractError)] = &[
        (
            |graph| graph.nodes[0].outputs[0].name.clear(),
            GraphContractError::EmptyField("output_name"),
        ),
        (
            |graph| {
                let output = graph.nodes[0].outputs[0].clone();
                graph.nodes[0].outputs.push(output);
            },
            duplicate("output", "response"),
        ),
        (
            |graph| graph.nodes[0].outputs[0].schema = ProtocolRequestProfile::UtilityPrompt,
            GraphContractError::InvalidProfile(
                "output schema must match the producing response profile",
            ),
        ),
        (
            |graph| graph.nodes[1].inputs[0].name.clear(),
            GraphContractError::EmptyField("input_name"),
        ),
        (
            |graph| graph.nodes[1].inputs[0].producer_node_id.clear(),
            GraphContractError::EmptyField("producer_node_id"),
        ),
        (
            |graph| graph.nodes[1].inputs[0].output_name.clear(),
            GraphContractError::EmptyField("output_name"),
        ),
        (
            |graph| {
                let input = graph.nodes[1].inputs[0].clone();
                graph.nodes[1].inputs.push(input);
            },
            duplicate("input", "evidence"),
        ),
    ];

    // Act and Assert
    assert_invalid_graphs(cases);
}

#[test]
fn graphs_reject_missing_duplicate_and_ungated_dependencies() {
    // Arrange
    type Mutation = fn(&mut ExecutionGraph);
    let cases: &[(Mutation, GraphContractError)] = &[
        (
            |graph| graph.nodes[1].dependencies[0].node_id = "missing".into(),
            GraphContractError::UnknownNode("missing".into()),
        ),
        (
            |graph| {
                let dependency = graph.nodes[1].dependencies[0].clone();
                graph.nodes[1].dependencies.push(dependency);
            },
            duplicate("dependency", "first"),
        ),
        (
            |graph| {
                graph.nodes[1].dependencies[0].gate = GraphCompletionGate::ResultAccepted {
                    output_name: "missing".into(),
                }
            },
            GraphContractError::UnknownOutput {
                node: "first".into(),
                output: "missing".into(),
            },
        ),
        (
            |graph| graph.nodes[1].inputs[0].producer_node_id = "missing".into(),
            GraphContractError::UnknownNode("missing".into()),
        ),
        (
            |graph| graph.nodes[1].inputs[0].output_name = "missing".into(),
            GraphContractError::UnknownOutput {
                node: "first".into(),
                output: "missing".into(),
            },
        ),
        (
            |graph| graph.nodes[1].dependencies.clear(),
            GraphContractError::UngatedInput("evidence".into()),
        ),
        (
            |graph| graph.nodes[1].dependencies[0].gate = GraphCompletionGate::ExecutionSucceeded,
            GraphContractError::UngatedInput("evidence".into()),
        ),
    ];

    // Act and Assert
    assert_invalid_graphs(cases);
}

#[test]
fn graphs_reject_invalid_execution_requirements() {
    // Arrange
    type Mutation = fn(&mut ExecutionGraph);
    let cases: &[(Mutation, GraphContractError)] = &[
        (
            |graph| {
                task_node(&mut graph.nodes[0], "task");
                graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
                graph.nodes[0].execution.post_processing = GraphPostProcessing::SessionWorkflow;
            },
            GraphContractError::InvalidProfile(
                "read-only definitions cannot perform host mutation effects",
            ),
        ),
        (
            |graph| graph.nodes[0].execution.shared_budget_id = Some(String::new()),
            GraphContractError::EmptyField("shared_budget_id"),
        ),
    ];

    // Act and Assert
    assert_invalid_graphs(cases);
}

fn assert_invalid_graphs(cases: &[InvalidGraphCase]) {
    for (mutate, expected) in cases {
        let mut graph = graph();
        mutate(&mut graph);

        // Act
        let validation = graph.validate();

        // Assert
        assert_eq!(validation, Err(expected.clone()), "{graph:?}");
    }
}

#[test]
fn graphs_reject_self_dependencies_and_cycles_even_in_disconnected_components() {
    // Arrange
    for predecessor in ["second", "first"] {
        let mut graph = graph();
        graph.nodes.push(reader("disconnected-root"));
        graph.nodes[0].dependencies.push(GraphDependency {
            gate: GraphCompletionGate::ExecutionSucceeded,
            node_id: predecessor.into(),
        });

        // Act
        let validation = graph.validate();

        // Assert
        assert_eq!(validation, Err(GraphContractError::Cycle));
    }
}

#[test]
fn graphs_allow_multiple_gates_and_join_producers_in_any_declaration_order() {
    // Arrange
    let mut graph = graph();
    graph.nodes[1].dependencies.extend([
        GraphDependency {
            gate: GraphCompletionGate::ExecutionSucceeded,
            node_id: "first".into(),
        },
        GraphDependency {
            gate: GraphCompletionGate::ResultAccepted {
                output_name: "response".into(),
            },
            node_id: "first".into(),
        },
        GraphDependency {
            gate: GraphCompletionGate::ExecutionSucceeded,
            node_id: "third".into(),
        },
    ]);
    graph.nodes.push(reader("third"));
    graph.nodes.reverse();
    let original = graph.clone();

    // Act
    let validation = graph.validate();

    // Assert
    assert_eq!(validation, Ok(()));
    assert_eq!(graph, original);
}

#[test]
fn graphs_validate_deep_dependency_chains_without_recursion() {
    // Arrange
    let mut graph = graph();
    graph.nodes = (0..4096)
        .map(|index| {
            let mut node = reader(&format!("node-{index}"));
            if index > 0 {
                node.dependencies.push(GraphDependency {
                    gate: GraphCompletionGate::ExecutionSucceeded,
                    node_id: format!("node-{}", index - 1),
                });
            }

            node
        })
        .collect();

    // Act
    let validation = graph.validate();

    // Assert
    assert_eq!(validation, Ok(()));
}

#[test]
fn input_gates_require_the_state_of_the_exact_version() {
    // Arrange
    for acceptance_required in [false, true] {
        for state in [
            GraphResultState::Produced,
            GraphResultState::Validated,
            GraphResultState::Accepted,
        ] {
            let mut graph = graph();
            if acceptance_required {
                graph.nodes[1].dependencies.push(GraphDependency {
                    gate: GraphCompletionGate::ResultAccepted {
                        output_name: "response".into(),
                    },
                    node_id: "first".into(),
                });
            }
            let result = GraphResult { state, ..result() };

            // Act
            let binding = graph.bind_input("second", "evidence", &result);

            // Assert
            if state == GraphResultState::Produced
                || (acceptance_required && state != GraphResultState::Accepted)
            {
                assert_eq!(binding, Err(GraphContractError::UnsatisfiedGate));
            } else {
                let binding = binding.expect("gate satisfied");
                assert_eq!(binding.name, "evidence");
                assert_eq!(binding.result, result.reference);
            }
        }
    }
}

#[test]
fn input_resolution_rejects_foreign_stale_or_mistyped_result_provenance() {
    // Arrange
    type Mutation = fn(&mut GraphResult);
    let cases: &[(Mutation, GraphContractError)] = &[
        (
            |result| result.reference.graph_id = "other".into(),
            GraphContractError::ResultMismatch("graph identity or revision"),
        ),
        (
            |result| result.reference.graph_revision = NonZeroU64::new(2).expect("nonzero"),
            GraphContractError::ResultMismatch("graph identity or revision"),
        ),
        (
            |result| result.reference.node_id = "second".into(),
            GraphContractError::ResultMismatch("producer or output"),
        ),
        (
            |result| result.reference.output_name = "other".into(),
            GraphContractError::ResultMismatch("producer or output"),
        ),
        (
            |result| result.reference.schema = ProtocolRequestProfile::UtilityPrompt,
            GraphContractError::ResultMismatch("output schema"),
        ),
        (
            |result| {
                result.reference.execution = GraphExecutionRef::TaskAttempt {
                    attempt_id: "attempt".into(),
                    operation_id: "operation".into(),
                    task_id: "task".into(),
                }
            },
            GraphContractError::ResultMismatch("execution authority"),
        ),
        (
            |result| result.reference.id.clear(),
            GraphContractError::EmptyField("result_id"),
        ),
    ];

    for (mutate, expected) in cases {
        let graph = graph();
        let mut result = result();
        mutate(&mut result);

        // Act
        let binding = graph.bind_input("second", "evidence", &result);

        // Assert
        assert_eq!(binding, Err(expected.clone()));
    }
}

#[test]
fn input_resolution_requires_a_valid_graph_and_declared_consumer_slot() {
    // Arrange
    let mut invalid = graph();
    invalid.nodes.clear();

    // Act
    let invalid_graph = invalid.bind_input("second", "evidence", &result());
    let unknown_consumer = graph().bind_input("missing", "evidence", &result());
    let unknown_slot = graph().bind_input("second", "missing", &result());

    // Assert
    assert_eq!(invalid_graph, Err(GraphContractError::EmptyGraph));
    assert_eq!(
        unknown_consumer,
        Err(GraphContractError::UnknownNode("missing".into()))
    );
    assert_eq!(
        unknown_slot,
        Err(GraphContractError::ResultMismatch("declared input"))
    );
}

#[test]
fn task_result_resolution_requires_the_assigned_task_authority() {
    // Arrange
    let mut graph = graph();
    task_node(&mut graph.nodes[0], "task");
    let mut result = result();
    result.reference.schema = ProtocolRequestProfile::SessionTurn;

    for execution in [
        GraphExecutionRef::UtilityRun {
            run_id: "run".into(),
        },
        GraphExecutionRef::TaskAttempt {
            attempt_id: "attempt".into(),
            operation_id: "operation".into(),
            task_id: "other".into(),
        },
        GraphExecutionRef::TaskAttempt {
            attempt_id: "attempt".into(),
            operation_id: "operation".into(),
            task_id: "task".into(),
        },
    ] {
        result.reference.execution = execution.clone();
        let expected_valid = matches!(&execution, GraphExecutionRef::TaskAttempt { task_id, .. } if task_id == "task");

        // Act
        let binding = graph.bind_input("second", "evidence", &result);

        // Assert
        if expected_valid {
            assert_eq!(binding.expect("assigned task").result.execution, execution);
        } else {
            assert_eq!(
                binding,
                Err(GraphContractError::ResultMismatch("execution authority"))
            );
        }
    }
}

fn duplicate(kind: &'static str, id: &str) -> GraphContractError {
    GraphContractError::Duplicate {
        kind,
        id: id.into(),
    }
}

fn task_node(node: &mut GraphNode, task_id: &str) {
    node.execution.context = GraphContext::TaskConversation {
        task_id: task_id.into(),
    };
    node.execution.response_profile = ProtocolRequestProfile::SessionTurn;
    node.outputs[0].schema = ProtocolRequestProfile::SessionTurn;
}
