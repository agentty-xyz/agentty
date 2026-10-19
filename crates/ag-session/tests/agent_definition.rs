//! Public built-in role contracts for future task workflow consumers.

use ag_session::{AgentContextRequirement, AgentOutputKind, BuiltinAgentDefinition};

#[test]
fn planner_requires_user_input_and_returns_a_proposal() {
    // Arrange
    let definition = BuiltinAgentDefinition::Planner;

    // Act
    let id = definition.id();
    let context = definition.context_requirement();
    let output = definition.output_kind();
    let instructions = definition.instructions();

    // Assert
    assert_eq!(id, "planner");
    assert_eq!(context, AgentContextRequirement::UserRequest);
    assert_eq!(output, AgentOutputKind::Plan);
    assert!(instructions.contains("Do not edit files or run mutating commands."));
    assert!(instructions.contains("existing questions field"));
    assert!(instructions.contains("does not accept the plan or complete the task"));
}

#[test]
fn implementer_requires_an_accepted_plan_and_reports_observed_work() {
    // Arrange
    let definition = BuiltinAgentDefinition::Implementer;

    // Act
    let id = definition.id();
    let context = definition.context_requirement();
    let output = definition.output_kind();
    let instructions = definition.instructions();

    // Assert
    assert_eq!(id, "implementer");
    assert_eq!(context, AgentContextRequirement::UserRequestAndAcceptedPlan);
    assert_eq!(output, AgentOutputKind::ImplementationReport);
    assert!(instructions.contains("Do not assume access to the planner's conversation."));
    assert!(instructions.contains("existing questions field"));
    assert!(instructions.contains("checks actually run and their results"));
    assert!(instructions.contains("does not accept the outcome or complete the task"));
}
