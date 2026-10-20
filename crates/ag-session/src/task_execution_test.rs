use crate::{AcceptedTaskPlan, BuiltinAgentDefinition, TaskAgentInput};

#[test]
fn validates_role_inputs_without_modifying_user_content() {
    // Arrange
    let valid_plan = AcceptedTaskPlan {
        attempt_id: "plan-attempt".to_string(),
        task_id: "plan-task".to_string(),
        text: "  Implement the accepted scope.\n".to_string(),
    };
    let cases = [
        (BuiltinAgentDefinition::Planner, None, true),
        (
            BuiltinAgentDefinition::Planner,
            Some(valid_plan.clone()),
            false,
        ),
        (BuiltinAgentDefinition::Implementer, None, false),
        (
            BuiltinAgentDefinition::Implementer,
            Some(valid_plan.clone()),
            true,
        ),
        (
            BuiltinAgentDefinition::Implementer,
            Some(AcceptedTaskPlan {
                task_id: " \n".to_string(),
                ..valid_plan.clone()
            }),
            false,
        ),
        (
            BuiltinAgentDefinition::Implementer,
            Some(AcceptedTaskPlan {
                attempt_id: "\t".to_string(),
                ..valid_plan.clone()
            }),
            false,
        ),
        (
            BuiltinAgentDefinition::Implementer,
            Some(AcceptedTaskPlan {
                text: " \n".to_string(),
                ..valid_plan
            }),
            false,
        ),
    ];

    for (definition, accepted_plan, expected) in cases {
        let input = TaskAgentInput {
            accepted_plan,
            clarification: Some("  Keep literal user guidance.\n".to_string()),
            user_request: "  Original request.\n".to_string(),
        };
        let original = input.clone();

        // Act
        let result = input.validate(definition);

        // Assert
        assert_eq!(result.is_ok(), expected, "{definition:?}: {input:?}");
        assert_eq!(input, original);
    }
}

#[test]
fn rejects_empty_requests_before_role_validation() {
    // Arrange
    let input = TaskAgentInput {
        accepted_plan: None,
        clarification: None,
        user_request: " \n\t".to_string(),
    };

    // Act
    let error = input
        .validate(BuiltinAgentDefinition::Planner)
        .expect_err("An empty original request must be rejected");

    // Assert
    assert_eq!(error.to_string(), "Task user request is empty");
}
