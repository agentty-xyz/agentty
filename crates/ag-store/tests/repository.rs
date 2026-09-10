//! Public repository contracts remain available without `test-utils`.

use std::sync::Arc;

use ag_store::Database;

#[tokio::test]
async fn maintenance_operations_use_the_same_public_repository_contract() {
    // Arrange
    let database = Database::open_in_memory_with_timestamp_source(Arc::new(|| 123))
        .await
        .expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("repository-contract", None)
        .await
        .expect("project should persist");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Draft", project_id)
        .await
        .expect("session should persist");

    // Act
    database
        .sessions()
        .update_session_created_at("session-a", 100)
        .await
        .expect("creation timestamp should update");
    database
        .sessions()
        .update_session_updated_at("session-a", 200)
        .await
        .expect("modification timestamp should update");
    database
        .activity()
        .clear_session_activity()
        .await
        .expect("activity should clear");
    database
        .activity()
        .backfill_session_activity_from_sessions()
        .await
        .expect("activity should rebuild from session timestamps");
    let sessions = database.sessions().load_sessions().await.expect("sessions");
    let activity = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("activity timestamps");

    // Assert
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].created_at, 100);
    assert_eq!(sessions[0].updated_at, 200);
    assert_eq!(activity, vec![100]);
}
