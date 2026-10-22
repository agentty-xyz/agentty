use crate::execution_graph::test_support::result;
use crate::{GraphContractError, GraphExecutionRef, GraphResultRef};

#[test]
fn references_require_every_graph_output_and_execution_identity() {
    // Arrange
    type Mutation = fn(&mut GraphResultRef);
    let cases: &[(Mutation, &str)] = &[
        (|reference| reference.graph_id.clear(), "graph_id"),
        (|reference| reference.id = " ".into(), "result_id"),
        (|reference| reference.node_id.clear(), "node_id"),
        (|reference| reference.output_name.clear(), "output_name"),
        (
            |reference| {
                reference.execution = GraphExecutionRef::UtilityRun {
                    run_id: String::new(),
                }
            },
            "run_id",
        ),
        (
            |reference| reference.execution = task_execution("", "operation", "task"),
            "attempt_id",
        ),
        (
            |reference| reference.execution = task_execution("attempt", "", "task"),
            "operation_id",
        ),
        (
            |reference| reference.execution = task_execution("attempt", "operation", ""),
            "task_id",
        ),
    ];

    for (mutate, field) in cases {
        let mut reference = result().reference;
        mutate(&mut reference);

        // Act
        let validation = reference.validate();

        // Assert
        assert_eq!(validation, Err(GraphContractError::EmptyField(field)));
    }
}

#[test]
fn references_retain_both_existing_execution_authorities() {
    // Arrange
    for execution in [
        GraphExecutionRef::UtilityRun {
            run_id: "run".into(),
        },
        task_execution("attempt", "operation", "task"),
    ] {
        let reference = GraphResultRef {
            execution: execution.clone(),
            ..result().reference
        };

        // Act
        let validation = reference.validate();

        // Assert
        assert_eq!(validation, Ok(()));
        assert_eq!(reference.execution, execution);
    }
}

fn task_execution(attempt_id: &str, operation_id: &str, task_id: &str) -> GraphExecutionRef {
    GraphExecutionRef::TaskAttempt {
        attempt_id: attempt_id.into(),
        operation_id: operation_id.into(),
        task_id: task_id.into(),
    }
}
