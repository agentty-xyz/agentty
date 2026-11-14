//! Public role-aware task input and captured selection contracts.

use ag_protocol::TurnPromptAttachment;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, DurableMessageVisibility, EnvId, SubmitAgentTaskRequest,
    TaskAgentAssignment, TaskAgentContext, TaskAgentInput, TaskAgentRole,
};

#[test]
fn task_input_images_round_trip_and_old_text_only_inputs_still_load() {
    // Arrange
    let legacy = r#"{"accepted_plan":null,"clarification":null,"user_request":"Plan this"}"#;
    let mut input: TaskAgentInput = serde_json::from_str(legacy).expect("old input");
    assert_eq!(input.attachments, [] as [TurnPromptAttachment; 0]);
    input.attachments.push(TurnPromptAttachment {
        local_image_path: "reference.png".into(),
        placeholder: "[Image #1]".into(),
    });

    // Act
    let encoded = serde_json::to_string(&input).expect("encode input");
    let restored: TaskAgentInput = serde_json::from_str(&encoded).expect("restore input");

    // Assert
    assert_eq!(restored, input);
}

#[test]
fn role_assignment_retains_the_callers_captured_selection() {
    // Arrange
    let captured = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol);
    let request = SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: captured,
            role: TaskAgentRole::Planner,
            definition: ag_session::AgentDefinition::bundled("planner").expect("bundled agent"),
        },
        attempt_id: "plan-first-attempt".to_string(),
        input: TaskAgentInput {
            attachments: Vec::new(),
            accepted_plan: None,
            clarification: None,
            user_request: "Plan the requested change".to_string(),
        },
        objective: "Produce an implementation plan".to_string(),
        task_id: "planner".to_string(),
        visibility: DurableMessageVisibility::Visible,
    };
    let later_session_selection = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55);

    // Act
    let validation = request.input.validate(request.assignment.role);

    // Assert
    assert!(validation.is_ok());
    assert_eq!(request.assignment.agent, captured);
    assert_ne!(request.assignment.agent, later_session_selection);
}

#[test]
fn independent_task_contexts_share_the_sessions_environment() {
    // Arrange
    let planner = TaskAgentContext {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            role: TaskAgentRole::Planner,
            definition: ag_session::AgentDefinition::bundled("planner").expect("bundled agent"),
        },
        id: "host-context-a".to_string(),
        session_id: "session".into(),
        task_id: "plan".to_string(),
    };
    let implementer = TaskAgentContext {
        assignment: TaskAgentAssignment {
            role: TaskAgentRole::Implementer,
            definition: ag_session::AgentDefinition::bundled("implementer").expect("bundled agent"),
            ..planner.assignment
        },
        id: "host-context-b".to_string(),
        task_id: "implement".to_string(),
        ..planner.clone()
    };

    // Act
    let planner_env = planner.env_id();
    let implementer_env = implementer.env_id();

    // Assert
    assert_eq!(planner_env, EnvId::SessionWorktree("session".into()));
    assert_eq!(implementer_env, planner_env);
    assert_ne!(planner.id, implementer.id);
    assert_eq!(planner.assignment.agent, implementer.assignment.agent);
}
