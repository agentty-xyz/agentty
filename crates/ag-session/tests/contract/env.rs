//! Public environment ownership contract, without filesystem provisioning.

use ag_session::{
    AgentKind, AgentModel, AgentSelection, EnvId, ReasoningLevel, ResponseStyle, Session,
    SessionSettings, SessionStatus, SpeedMode, Task, TaskWorktreeId, session_branch,
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

#[test]
fn task_worktrees_belong_to_their_session_without_sharing_its_names() {
    // Arrange
    let session = session();
    let worktree = TaskWorktreeId {
        session_id: session.id.clone(),
        worktree_id: "87654321-writer".to_string(),
    };
    let env_id = EnvId::TaskWorktree(worktree.clone());

    // Act
    let owner = env_id.session_id();

    // Assert
    assert_eq!(owner, &session.id);
    assert_eq!(session.env_id().session_id(), &session.id);
    assert_ne!(env_id, session.env_id());
    assert_eq!(worktree.branch(), "wt/12345678-87654321");
    assert_eq!(worktree.directory_name(), "12345678-87654321");
    assert_ne!(worktree.branch(), session_branch(&session.id));
}

#[test]
fn short_task_worktree_names_keep_whole_characters() {
    // Arrange
    let worktree = TaskWorktreeId {
        session_id: "s".into(),
        worktree_id: "ééééééééé".to_string(),
    };

    // Act
    let directory_name = worktree.directory_name();

    // Assert
    assert_eq!(directory_name, "s-éééééééé");
    assert_eq!(worktree.branch(), "wt/s-éééééééé");
}

fn session() -> Session {
    Session {
        created_at: 1,
        draft_prompt: None,
        focused_review: None,
        graph: None,
        graph_question_target: None,
        id: "12345678-owner".into(),
        messages: Vec::new(),
        progress: None,
        published_upstream_ref: None,
        questions: Vec::new(),
        queued_actions: Vec::new(),
        queued_messages: Vec::new(),
        review_request: None,
        settings: SessionSettings {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            base_branch: "main".to_string(),
            is_draft: true,
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
