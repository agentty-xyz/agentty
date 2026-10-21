//! Public environment ownership contract, without filesystem provisioning.

use ag_session::{
    AgentKind, AgentModel, AgentSelection, EnvId, PermissionMode, ReasoningLevel, ResponseStyle,
    Session, SessionSettings, SessionStatus, SpeedMode, Task,
};

#[test]
fn tasks_share_their_sessions_environment_across_lifecycle_changes() {
    // Arrange
    let mut session = session();
    let mut task = Task {
        attempts: Vec::new(),
        completed_at: None,
        created_at: 1,
        id: "plan".to_string(),
        objective: "Plan the change".to_string(),
        session_id: session.id.clone(),
    };
    let another_task = Task {
        id: "implement".to_string(),
        objective: "Implement the plan".to_string(),
        ..task.clone()
    };

    // Act
    let original = session.env_id();
    task.completed_at = Some(2);
    session.status = SessionStatus::Done;

    // Assert
    assert_eq!(original, EnvId::SessionWorktree(session.id.clone()));
    assert_eq!(session.env_id(), original);
    assert_eq!(task.env_id(), original);
    assert_eq!(another_task.env_id(), original);
    assert_ne!(original, EnvId::SessionWorktree("12345678-other".into()));
}

fn session() -> Session {
    Session {
        created_at: 1,
        draft_prompt: None,
        id: "12345678-owner".into(),
        messages: Vec::new(),
        published_upstream_ref: None,
        questions: Vec::new(),
        queued_messages: Vec::new(),
        review_request: None,
        settings: SessionSettings {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            base_branch: "main".to_string(),
            is_draft: true,
            parent_session_id: None,
            permission_mode: PermissionMode::AutoEdit,
            project_id: 1,
            reasoning_level: ReasoningLevel::default(),
            response_style: ResponseStyle::default(),
            speed_mode: SpeedMode::default(),
        },
        status: SessionStatus::Draft,
        title: None,
        updated_at: 1,
    }
}
