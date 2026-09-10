use ag_agent::{AgentKind, ReasoningLevel, SpeedMode};
use sqlx::SqlitePool;

use crate::orchestration::PersistedOrchestrationTask;
use crate::{AppRepositories, PersistedSessionCreation};

/// Inserts one project plus one controller session and returns the
/// repository bundle ready for orchestration persistence assertions.
pub(super) async fn controller_fixture() -> AppRepositories {
    controller_fixture_with_pool().await.0
}

pub(super) async fn controller_fixture_with_pool() -> (AppRepositories, SqlitePool) {
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "controller",
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("Orchestrator"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert controller session");

    (database, pool)
}

/// Builds one planned task payload for `orchestration_id`.
pub(super) fn planned_task(
    session_orchestration_id: i64,
    task_key: &str,
) -> PersistedOrchestrationTask {
    PersistedOrchestrationTask {
        acceptance_criteria: format!(r#"["Complete {task_key}"]"#),
        kind: "Implementation".to_string(),
        merge_position: 0,
        prompt: format!("Complete {task_key}"),
        session_orchestration_id,
        task_key: task_key.to_string(),
        title: format!("Task {task_key}"),
        touched_areas: format!(r#"["crates/{task_key}/"]"#),
    }
}

/// Persists one worker session carrying the durable reverse task link.
pub(super) async fn insert_orchestration_child(
    database: &AppRepositories,
    project_id: i64,
    session_id: &str,
    task_id: i64,
) {
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: session_id,
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
            orchestration_task_id: Some(task_id),
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("OrchestrationWorker"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert orchestration child");
}
