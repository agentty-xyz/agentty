use ag_session::{ReasoningLevel, SpeedMode};
use sqlx::SqlitePool;

use crate::AppRepositories;

/// Opens a repository with one project and the given sessions inserted with
/// their statuses.
async fn database_with_sessions(sessions: &[(&str, &str)]) -> (AppRepositories, SqlitePool, i64) {
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    for (session_id, status) in sessions {
        database
            .sessions()
            .insert_session(session_id, "gpt-5.6-sol", "main", status, project_id)
            .await
            .expect("failed to insert session");
    }

    (database, pool, project_id)
}

#[tokio::test]
async fn test_session_reasoning_level_and_speed_mode_default_and_round_trip_updates() {
    // Arrange
    let (database, _, _) = database_with_sessions(&[("session-a", "Draft")]).await;
    let sessions = database.sessions();
    let default_reasoning_level = sessions
        .load_session_reasoning_level("session-a")
        .await
        .expect("default reasoning level should load");
    let default_speed_mode = sessions
        .load_session_speed_mode("session-a")
        .await
        .expect("default speed mode should load");

    // Act
    sessions
        .update_session_reasoning_level("session-a", ReasoningLevel::Low)
        .await
        .expect("reasoning level update should persist");
    sessions
        .update_session_speed_mode("session-a", SpeedMode::Fast)
        .await
        .expect("speed mode update should persist");
    let updated_reasoning_level = sessions
        .load_session_reasoning_level("session-a")
        .await
        .expect("updated reasoning level should load");
    let updated_speed_mode = sessions
        .load_session_speed_mode("session-a")
        .await
        .expect("updated speed mode should load");

    // Assert
    assert_eq!(default_reasoning_level, ReasoningLevel::default());
    assert_eq!(default_speed_mode, SpeedMode::Normal);
    assert_eq!(updated_reasoning_level, ReasoningLevel::Low);
    assert_eq!(updated_speed_mode, SpeedMode::Fast);
}

#[tokio::test]
async fn test_session_agent_model_updates_respect_active_status() {
    // Arrange
    let (database, _, _) =
        database_with_sessions(&[("session-active", "Draft"), ("session-done", "Done")]).await;
    let sessions = database.sessions();

    // Act
    sessions
        .update_session_agent_model("session-done", "claude", "claude-opus-5")
        .await
        .expect("model update should persist");
    for session_id in ["session-active", "session-done"] {
        sessions
            .update_active_session_agent_model(session_id, "antigravity", "gemini-4-argon")
            .await
            .expect("active model update should run");
    }
    let active_rows = sessions
        .load_active_session_agent_models()
        .await
        .expect("active models should load");
    let done_session = sessions
        .load_session("session-done")
        .await
        .expect("done session should load")
        .expect("done session should exist");

    // Assert
    assert_eq!(
        active_rows
            .iter()
            .map(|row| (
                row.id.as_str(),
                row.agent.as_str(),
                row.model.as_str(),
                row.status.as_str()
            ))
            .collect::<Vec<_>>(),
        [("session-active", "antigravity", "gemini-4-argon", "Draft")]
    );
    assert_eq!(
        (done_session.agent.as_str(), done_session.model.as_str()),
        ("claude", "claude-opus-5")
    );
}

#[tokio::test]
async fn test_session_parent_diff_and_project_backfill_updates() {
    // Arrange
    let (database, pool, _) =
        database_with_sessions(&[("session-parent", "Review"), ("session-child", "Draft")]).await;
    let other_project_id = database
        .projects()
        .upsert_project("/tmp/other-project", None)
        .await
        .expect("failed to upsert other project");
    sqlx::query(
        "UPDATE session SET parent_session_id = 'session-parent', has_diff = 1, project_id = NULL \
         WHERE id = 'session-child'",
    )
    .execute(&pool)
    .await
    .expect("failed to seed child session");
    let sessions = database.sessions();

    // Act
    let parent_session_id = sessions
        .get_session_parent_session_id("session-child")
        .await
        .expect("parent should load");
    let missing_parent_session_id = sessions
        .get_session_parent_session_id("session-missing")
        .await
        .expect("missing session parent should load");
    sessions
        .mark_session_diff_unknown("session-child")
        .await
        .expect("diff state should reset");
    sessions
        .backfill_session_project(other_project_id)
        .await
        .expect("project backfill should persist");
    let child_session = sessions
        .load_session("session-child")
        .await
        .expect("child session should load")
        .expect("child session should exist");

    // Assert
    assert_eq!(parent_session_id.as_deref(), Some("session-parent"));
    assert_eq!(missing_parent_session_id, None);
    assert_eq!(child_session.has_diff, None);
    assert_eq!(child_session.project_id, Some(other_project_id));
}
