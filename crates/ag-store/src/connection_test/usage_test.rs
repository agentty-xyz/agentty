use ag_agent::{SessionDiffState, SessionStats};

use super::support::insert_session_fixture;
use crate::connection::Database;

/// Verifies `delete_session()` removes the session row and nulls
/// `session_usage.session_id`.
#[tokio::test]
async fn test_delete_session_removes_row_and_nulls_usage_foreign_key() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            "claude-opus-4.1",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 11,
                output_tokens: 29,
            },
        )
        .await
        .expect("failed to insert usage row");

    // Act
    database
        .sessions()
        .delete_session("session-a")
        .await
        .expect("failed to delete session");
    let deleted_session = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load deleted session timestamps");
    let retained_usage_row = sqlx::query_as!(
        SessionUsageSessionIdRow,
        r#"
SELECT session_id AS "session_id: _"
FROM session_usage
WHERE model = ?
"#,
        "claude-opus-4.1"
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load retained usage row");

    // Assert
    assert_eq!(deleted_session, None);
    assert_eq!(retained_usage_row.session_id, None,);
}

/// Verifies `upsert_session_usage()` accumulates per-model token totals and
/// invocation counts.
#[tokio::test]
async fn test_upsert_session_usage_accumulates_counts_per_model() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            "claude-opus-4.1",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 11,
                output_tokens: 29,
            },
        )
        .await
        .expect("failed to insert first usage row");
    database
        .usage()
        .upsert_session_usage(
            "session-a",
            "claude-opus-4.1",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 3,
                output_tokens: 5,
            },
        )
        .await
        .expect("failed to update existing usage row");
    database
        .usage()
        .upsert_session_usage("session-a", "ignored-model", &SessionStats::default())
        .await
        .expect("failed to ignore zero-usage update");

    // Act
    let usage_rows = database
        .usage()
        .load_session_usage("session-a")
        .await
        .expect("failed to load session usage");

    // Assert
    assert_eq!(usage_rows.len(), 1);
    assert_eq!(usage_rows[0].model, "claude-opus-4.1");
    assert_eq!(usage_rows[0].input_tokens, 14);
    assert_eq!(usage_rows[0].invocation_count, 2);
    assert_eq!(usage_rows[0].output_tokens, 34);
    assert_eq!(usage_rows[0].session_id.as_deref(), Some("session-a"));
}

/// Typed helper row used to verify nullable session references.
struct SessionUsageSessionIdRow {
    session_id: Option<String>,
}
