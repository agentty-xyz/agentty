use crate::orchestration::{OrchestrationPlanTask, OrchestrationTaskKind, validate_subtasks};

#[test]
fn validation_allows_one_research_task_and_ignores_its_touched_areas() {
    // Arrange
    let plan = [OrchestrationPlanTask {
        acceptance_criteria: vec!["Architecture questions are answered".to_string()],
        kind: OrchestrationTaskKind::Research,
        prompt: "Inspect the architecture".to_string(),
        task_key: "architecture".to_string(),
        title: "Architecture research".to_string(),
        touched_areas: vec!["**".to_string()],
    }];

    // Act
    let result = validate_subtasks(&plan, false);

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn validation_rejects_one_implementation_task_and_invalid_implementation_scope() {
    // Arrange
    let single = [OrchestrationPlanTask {
        acceptance_criteria: vec!["Feature is complete".to_string()],
        kind: OrchestrationTaskKind::Implementation,
        prompt: "Implement the feature".to_string(),
        task_key: "feature".to_string(),
        title: "Feature".to_string(),
        touched_areas: Vec::new(),
    }];
    let invalid_scope = [
        OrchestrationPlanTask {
            touched_areas: vec!["crates/one/**".to_string()],
            ..single[0].clone()
        },
        OrchestrationPlanTask {
            task_key: "tests".to_string(),
            touched_areas: Vec::new(),
            ..single[0].clone()
        },
    ];
    let mixed = [
        OrchestrationPlanTask {
            kind: OrchestrationTaskKind::Research,
            ..single[0].clone()
        },
        OrchestrationPlanTask {
            task_key: "implementation".to_string(),
            ..single[0].clone()
        },
    ];

    // Act
    let single_result = validate_subtasks(&single, false);
    let empty_result = validate_subtasks(&[], false);
    let scope_result = validate_subtasks(&invalid_scope, false);
    let mixed_result = validate_subtasks(&mixed, false);

    // Assert
    assert_eq!(
        single_result,
        Err("a meaningful orchestration requires at least two subtasks.".to_string())
    );
    assert_eq!(
        empty_result,
        Err("a meaningful orchestration requires at least two subtasks.".to_string())
    );
    assert!(scope_result.is_err_and(|reason| reason.contains("wildcard patterns")));
    assert_eq!(
        mixed_result,
        Err("research and implementation tasks must be proposed in separate waves.".to_string())
    );
}
