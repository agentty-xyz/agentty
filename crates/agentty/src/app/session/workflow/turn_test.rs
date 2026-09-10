use super::{
    READ_ONLY_CHAT_PROMPT, load_session_permission_mode, load_session_response_style,
    load_title_reasoning_level,
};
use crate::app::session::SessionError;
use crate::domain::agent::{ReasoningLevel, ResponseStyle};
use crate::domain::permission::PermissionMode;
use crate::infra::db::{AppRepositories, DbError, PersistedSessionCreation};

#[test]
fn read_only_chat_prompt_redirects_write_access_requests_to_mode_shortcut() {
    // Arrange, Act
    let prompt = READ_ONLY_CHAT_PROMPT;

    // Assert
    assert!(prompt.contains("Do not ask a clarification question requesting write access"));
    assert!(prompt.contains("switching the session to `Auto Edit` with `Shift+Tab`"));
}

#[tokio::test]
async fn persisted_research_role_selects_read_only_permission_mode() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    repositories
        .sessions()
        .insert_session("worker", "gpt-5.6-sol", "main", "InProgress", project_id)
        .await
        .expect("failed to insert worker session");
    repositories
        .sessions()
        .insert_session(
            "read-only-worker",
            "gpt-5.6-sol",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert read-only worker session");
    repositories
        .sessions()
        .update_session_permission_mode("read-only-worker", PermissionMode::ReadOnly)
        .await
        .expect("failed to set read-only permission mode");
    repositories
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "researcher",
            is_draft: false,
            model: "gpt-5.6-sol",
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("OrchestrationResearcher"),
            speed_mode: crate::domain::agent::SpeedMode::Normal,
            status: "InProgress",
        })
        .await
        .expect("failed to insert research session");

    // Act
    let worker_mode = load_session_permission_mode(&repositories, "worker")
        .await
        .expect("worker mode should load");
    let read_only_worker_mode = load_session_permission_mode(&repositories, "read-only-worker")
        .await
        .expect("read-only worker mode should load");
    let research_mode = load_session_permission_mode(&repositories, "researcher")
        .await
        .expect("research mode should load");
    let missing_error = load_session_permission_mode(&repositories, "missing")
        .await
        .expect_err("missing session should fail");

    // Assert
    assert_eq!(worker_mode, PermissionMode::AutoEdit);
    assert_eq!(read_only_worker_mode, PermissionMode::ReadOnly);
    assert_eq!(research_mode, PermissionMode::ReadOnly);
    assert!(matches!(missing_error, SessionError::NotFound));
}

#[tokio::test]
async fn permission_mode_load_propagates_query_errors() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;

    // Act
    let result = load_session_permission_mode(&repositories, "worker").await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::Db(DbError::Query(sqlx::Error::PoolClosed)))
    ));
}

#[tokio::test]
async fn response_style_load_returns_persisted_value_and_defaults_missing_sessions() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    repositories
        .sessions()
        .insert_session("worker", "gpt-5.6-sol", "main", "InProgress", project_id)
        .await
        .expect("failed to insert worker session");
    repositories
        .sessions()
        .update_session_response_style("worker", ResponseStyle::Concise)
        .await
        .expect("failed to persist response style");

    // Act
    let persisted_style = load_session_response_style(&repositories, "worker").await;
    let missing_style = load_session_response_style(&repositories, "missing").await;

    // Assert
    assert_eq!(persisted_style, ResponseStyle::Concise);
    assert_eq!(missing_style, ResponseStyle::Balanced);
}

#[tokio::test]
async fn permission_mode_load_rejects_invalid_persisted_values() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    repositories
        .sessions()
        .insert_session("worker", "gpt-5.6-sol", "main", "InProgress", project_id)
        .await
        .expect("failed to insert worker session");
    sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'worker'")
        .execute(&pool)
        .await
        .expect("failed to corrupt permission mode");

    // Act
    let permission_result = load_session_permission_mode(&repositories, "worker").await;
    sqlx::query(
        "UPDATE session SET permission_mode = 'auto_edit', role = 'invalid' WHERE id = 'worker'",
    )
    .execute(&pool)
    .await
    .expect("failed to corrupt session role");
    let role_result = load_session_permission_mode(&repositories, "worker").await;

    // Assert
    assert!(matches!(
        permission_result,
        Err(SessionError::Db(DbError::InvalidData {
            entity: "session permission mode",
            ..
        }))
    ));
    assert!(matches!(
        role_result,
        Err(SessionError::Db(DbError::InvalidData {
            entity: "session role",
            ..
        }))
    ));
}

#[tokio::test]
async fn title_reasoning_level_defaults_without_project() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");

    // Act
    let reasoning_level = load_title_reasoning_level(&repositories, None).await;

    // Assert
    assert_eq!(reasoning_level, ReasoningLevel::High);
}
