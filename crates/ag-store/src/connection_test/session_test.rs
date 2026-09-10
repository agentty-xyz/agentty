use ag_agent::{SessionDiffState, SessionStats};
use ag_session::ReviewRequest;

use super::support::{assert_review_request_row, insert_session_fixture, load_session_row};
use crate::connection::Database;
use crate::test_support::review_request_fixture;

/// Verifies `load_sessions()` maps persisted joined session fields.
#[tokio::test]
async fn test_load_sessions_maps_joined_session_fields() {
    // Arrange
    let (database, project_id) = database_with_joined_session_fields().await;

    // Act
    let session_row = load_session_row(&database, "session-a").await;

    // Assert
    assert_eq!(session_row.id, "session-a");
    assert_eq!(session_row.base_branch, "main");
    assert_eq!(session_row.created_at, 100);
    assert_eq!(session_row.updated_at, 200);
    assert_eq!(session_row.agent, "claude");
    assert_eq!(session_row.model, "claude-opus-4.1");
    assert_eq!(session_row.status, "Review");
    assert_eq!(session_row.in_progress_started_at, None);
    assert_eq!(session_row.in_progress_total_seconds, 120);
    assert_eq!(session_row.project_id, Some(project_id));
    assert_eq!(session_row.prompt, "Implement the feature");
    assert_eq!(session_row.added_lines, 14);
    assert_eq!(session_row.deleted_lines, 6);
    assert_eq!(session_row.has_diff, Some(true));
    assert_eq!(session_row.input_tokens, 11);
    assert_eq!(session_row.output_tokens, 29);
    assert_eq!(session_row.parent_session_id, None);
    assert_eq!(session_row.size, "L");
    assert_eq!(session_row.questions.as_deref(), Some("[\"Need logs?\"]"));
    assert_eq!(session_row.title.as_deref(), Some("Feature work"));
    assert_eq!(
        session_row.published_upstream_ref.as_deref(),
        Some("origin/wt/session-a")
    );
    assert_review_request_row(&session_row);
}

/// Verifies timing-aware status transitions accumulate repeated
/// `InProgress` intervals.
#[tokio::test]
async fn test_update_session_status_with_timing_at_accumulates_repeated_intervals() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;

    // Act
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 10)
        .await
        .expect("failed to enter in-progress the first time");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "Review", 70)
        .await
        .expect("failed to leave in-progress the first time");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 100)
        .await
        .expect("failed to enter in-progress the second time");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "Question", 190)
        .await
        .expect("failed to leave in-progress the second time");
    let session_row = load_session_row(&database, "session-a").await;

    // Assert
    assert_eq!(session_row.status, "Question");
    assert_eq!(session_row.in_progress_started_at, None);
    assert_eq!(session_row.in_progress_total_seconds, 150);
}

/// Verifies `load_sessions_for_project()` filters rows by project id.
#[tokio::test]
async fn test_load_sessions_for_project_filters_to_project_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let first_project_id = database
        .projects()
        .upsert_project("/tmp/project-a", Some("main".to_string()))
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project("/tmp/project-b", Some("develop".to_string()))
        .await
        .expect("failed to insert second project");

    insert_session_fixture(&database, "session-a", "main", "Review", first_project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Done", first_project_id).await;
    insert_session_fixture(&database, "session-c", "develop", "Done", second_project_id).await;
    database
        .sessions()
        .update_session_updated_at("session-a", 300)
        .await
        .expect("failed to update session-a updated_at");
    database
        .sessions()
        .update_session_updated_at("session-b", 200)
        .await
        .expect("failed to update session-b updated_at");
    database
        .sessions()
        .update_session_updated_at("session-c", 100)
        .await
        .expect("failed to update session-c updated_at");

    // Act
    let session_rows = database
        .sessions()
        .load_sessions_for_project(first_project_id)
        .await
        .expect("failed to load project sessions");

    // Assert
    assert_eq!(session_rows.len(), 2);
    assert_eq!(session_rows[0].id, "session-a");
    assert_eq!(session_rows[1].id, "session-b");
    assert!(
        session_rows
            .iter()
            .all(|row| row.project_id == Some(first_project_id))
    );
}

/// Verifies `load_session_timestamps()` returns the persisted timestamps.
#[tokio::test]
async fn test_load_session_timestamps_returns_created_and_updated_values() {
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
        .sessions()
        .update_session_created_at("session-a", 111)
        .await
        .expect("failed to update session created_at");
    database
        .sessions()
        .update_session_updated_at("session-a", 222)
        .await
        .expect("failed to update session updated_at");

    // Act
    let session_timestamps = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load session timestamps");

    // Assert
    assert_eq!(session_timestamps, Some((111, 222)));
}

/// Verifies `get_session_base_branch()` returns the persisted branch name.
#[tokio::test]
async fn test_get_session_base_branch_returns_persisted_value() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");

    insert_session_fixture(&database, "session-a", "release", "Done", project_id).await;

    // Act
    let base_branch = database
        .sessions()
        .get_session_base_branch("session-a")
        .await
        .expect("failed to load session base branch");

    // Assert
    assert_eq!(base_branch.as_deref(), Some("release"));
}

#[tokio::test]
async fn test_load_session_project_id_returns_associated_project() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    let loaded_project_id = database
        .sessions()
        .load_session_project_id("session-a")
        .await
        .expect("failed to load session project id");

    // Assert
    assert_eq!(loaded_project_id, Some(project_id));
}

/// Builds an in-memory database with one session covering joined fields
/// returned by `load_sessions()`.
async fn database_with_joined_session_fields() -> (Database, i64) {
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    let review_request = review_request_fixture();

    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    persist_joined_session_metadata(&database, &review_request).await;
    persist_joined_session_state(&database).await;

    (database, project_id)
}

/// Persists metadata fields asserted by the joined-session mapping test.
async fn persist_joined_session_metadata(database: &Database, review_request: &ReviewRequest) {
    database
        .sessions()
        .update_session_created_at("session-a", 100)
        .await
        .expect("failed to update session created_at");
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("failed to update session updated_at");
    database
        .sessions()
        .update_session_diff_stats(14, 6, true, "session-a", "L")
        .await
        .expect("failed to update session diff stats");
    database
        .sessions()
        .update_session_questions("session-a", "[\"Need logs?\"]")
        .await
        .expect("failed to update session questions");
    database
        .sessions()
        .update_session_prompt("session-a", "Implement the feature")
        .await
        .expect("failed to update session prompt");
    database
        .sessions()
        .update_session_title("session-a", "Feature work")
        .await
        .expect("failed to update session title");
    database
        .sessions()
        .update_session_stats(
            "session-a",
            &SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 11,
                output_tokens: 29,
            },
        )
        .await
        .expect("failed to update session stats");
    database
        .sessions()
        .update_session_model("session-a", "claude-opus-4.1")
        .await
        .expect("failed to update session model");
    database
        .sessions()
        .update_session_published_upstream_ref("session-a", Some("origin/wt/session-a".to_string()))
        .await
        .expect("failed to update published upstream ref");
    database
        .reviews()
        .update_session_review_request("session-a", Some(review_request.clone()))
        .await
        .expect("failed to update review request");
}

/// Persists timing fields asserted by the joined-session mapping test.
async fn persist_joined_session_state(database: &Database) {
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "InProgress", 50)
        .await
        .expect("failed to open in-progress timing window");
    database
        .sessions()
        .update_session_status_with_timing_at("session-a", "Review", 170)
        .await
        .expect("failed to close in-progress timing window");
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("failed to update session updated_at");
}
