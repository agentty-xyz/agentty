use crate::connection::Database;

#[tokio::test]
async fn test_insert_session_creation_activity_at_persists_timestamp() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 123)
        .await
        .expect("failed to persist activity event");
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load activity timestamps");

    // Assert
    assert_eq!(activity_timestamps, vec![123]);
}

#[tokio::test]
async fn test_insert_session_creation_activity_at_ignores_duplicates_per_session() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    // Act
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 100)
        .await
        .expect("failed to persist first activity event");
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 200)
        .await
        .expect("failed to persist duplicate activity event");
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load activity timestamps");

    // Assert
    assert_eq!(activity_timestamps, vec![100]);
}

#[tokio::test]
async fn test_load_session_activity_timestamps_keeps_deleted_session_history() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert first session");
    database
        .activity()
        .insert_session_creation_activity_at("session-a", 100)
        .await
        .expect("failed to persist first activity event");
    database
        .sessions()
        .insert_session("session-b", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert second session");
    database
        .activity()
        .insert_session_creation_activity_at("session-b", 200)
        .await
        .expect("failed to persist second activity event");
    database
        .sessions()
        .delete_session("session-a")
        .await
        .expect("failed to delete first session");

    // Act
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load activity timestamps");

    // Assert
    assert_eq!(activity_timestamps, vec![100, 200]);
}

#[tokio::test]
async fn test_load_session_activity_timestamps_preserves_event_order() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert first session");
    database
        .sessions()
        .insert_session("session-b", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert second session");
    database
        .sessions()
        .insert_session("session-c", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert third session");

    let first_day_timestamp = 10 * 86_400 + 10;
    let second_timestamp_same_day = 10 * 86_400 + 600;
    let second_day_timestamp = 11 * 86_400 + 50;

    database
        .activity()
        .clear_session_activity()
        .await
        .expect("failed to clear session activity");
    database
        .activity()
        .insert_session_creation_activity_at("session-a", first_day_timestamp)
        .await
        .expect("failed to persist first activity event");
    database
        .activity()
        .insert_session_creation_activity_at("session-b", second_timestamp_same_day)
        .await
        .expect("failed to persist second activity event");
    database
        .activity()
        .insert_session_creation_activity_at("session-c", second_day_timestamp)
        .await
        .expect("failed to persist third activity event");

    // Act
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");

    // Assert
    assert_eq!(
        activity_timestamps,
        vec![
            first_day_timestamp,
            second_timestamp_same_day,
            second_day_timestamp,
        ]
    );
}
