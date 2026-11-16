//! Public built-in role contracts for future task workflow consumers.

use ag_session::TaskAgentRole;

#[test]
fn planner_definition_proposes_without_editing() {
    // Arrange
    let definition = TaskAgentRole::Planner;

    // Act
    let id = definition.id();
    let instructions = ag_session::AgentDefinition::bundled(id)
        .expect("bundled planner")
        .prompt;

    // Assert
    assert_eq!(id, "planner");
    assert!(instructions.contains("Do not edit files or run mutating commands."));
    assert!(instructions.contains("existing questions field"));
    assert!(instructions.contains("does not accept the plan or complete the task"));
}

#[test]
fn implementer_definition_reports_observed_work() {
    // Arrange
    let definition = TaskAgentRole::Implementer;

    // Act
    let id = definition.id();
    let instructions = ag_session::AgentDefinition::bundled(id)
        .expect("bundled implementer")
        .prompt;

    // Assert
    assert_eq!(id, "implementer");
    assert!(instructions.contains("Do not assume access to the planner's conversation."));
    assert!(instructions.contains("existing questions field"));
    assert!(instructions.contains("checks actually run and their results"));
    assert!(instructions.contains("does not accept the outcome or complete the task"));
}
