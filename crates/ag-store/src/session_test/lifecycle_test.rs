use ag_agent::ResponseStyle;

use crate::{AppRepositories, DbError};

#[tokio::test]
async fn test_load_session_rejects_unknown_status() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/invalid-session", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query("UPDATE session SET status = 'Unknown' WHERE id = 'session-a'")
        .execute(&pool)
        .await
        .expect("failed to corrupt session status");

    // Act
    let result = database.sessions().load_session("session-a").await;

    // Assert
    assert!(matches!(
        result,
        Err(DbError::InvalidStatus {
            entity: "session",
            value,
        }) if value == "Unknown"
    ));
}

#[tokio::test]
async fn test_load_session_collections_skip_unknown_status() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/invalid-session-list", None)
        .await
        .expect("failed to upsert project");
    for session_id in ["session-valid", "session-invalid"] {
        database
            .sessions()
            .insert_session(session_id, "gpt-5.6-sol", "main", "Draft", project_id)
            .await
            .expect("failed to insert session");
    }
    sqlx::query("UPDATE session SET status = 'Unknown' WHERE id = 'session-invalid'")
        .execute(&pool)
        .await
        .expect("failed to corrupt session status");

    // Act
    let all_sessions = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load all sessions");
    let project_sessions = database
        .sessions()
        .load_sessions_for_project(project_id)
        .await
        .expect("failed to load project sessions");

    // Assert
    assert_eq!(
        all_sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["session-valid"]
    );
    assert_eq!(
        project_sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["session-valid"]
    );
}

#[tokio::test]
async fn test_insert_session_starts_with_unknown_diff() {
    // Arrange
    let (database, _) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");

    // Act
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("failed to insert session");
    let session = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .next()
        .expect("missing inserted session");

    // Assert
    assert_eq!(session.has_diff, None);
}

#[tokio::test]
async fn test_session_response_style_defaults_and_round_trips_updates() {
    // Arrange
    let (database, _) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("failed to insert session");
    let default_style = database
        .sessions()
        .load_session_response_style("session-a")
        .await
        .expect("default style should load");

    // Act
    database
        .sessions()
        .update_session_response_style("session-a", ResponseStyle::Detailed)
        .await
        .expect("style update should persist");
    let updated_style = database
        .sessions()
        .load_session_response_style("session-a")
        .await
        .expect("updated style should load");

    // Assert
    assert_eq!(default_style, ResponseStyle::Balanced);
    assert_eq!(updated_style, ResponseStyle::Detailed);
}

#[tokio::test]
async fn test_load_sessions_uses_created_at_to_break_updated_at_ties() {
    // Arrange
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    for session_id in ["a-older", "z-newer"] {
        database
            .sessions()
            .insert_session(session_id, "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
    }
    sqlx::query!(
        r"
UPDATE session
SET created_at = CASE id WHEN 'a-older' THEN 100 ELSE 200 END,
    updated_at = 300
WHERE id IN ('a-older', 'z-newer')
"
    )
    .execute(&pool)
    .await
    .expect("failed to set session timestamps");

    // Act
    let all_session_ids = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .map(|session| session.id)
        .collect::<Vec<_>>();
    let project_session_ids = database
        .sessions()
        .load_sessions_for_project(project_id)
        .await
        .expect("failed to load project sessions")
        .into_iter()
        .map(|session| session.id)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(all_session_ids, ["z-newer", "a-older"]);
    assert_eq!(project_session_ids, ["z-newer", "a-older"]);
}

#[tokio::test]
async fn test_clear_session_draft_flag_marks_draft_session_live() {
    // Arrange
    let (database, _pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_draft_session("draft-session", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("failed to insert draft session");

    // Act
    database
        .sessions()
        .clear_session_draft_flag("draft-session")
        .await
        .expect("failed to clear session draft flag");

    // Assert
    let session_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|session_row| session_row.id == "draft-session")
        .expect("missing draft session row");
    assert!(!session_row.is_draft);
}
