use super::support::{insert_session_fixture, load_session_row};
use crate::connection::Database;

/// Verifies title candidates are ordered by accepted result rather than
/// request completion time.
#[tokio::test]
async fn test_session_title_candidate_order_preserves_usable_results() {
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
    database
        .sessions()
        .update_session_provisional_title("session-a", "First draft")
        .await
        .expect("failed to persist provisional title");
    let older_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to claim older title generation")
        .expect("provisional title should permit an older candidate");
    let newer_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to claim newer title generation")
        .expect("provisional title should permit a newer candidate");

    // Act
    let older_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", older_generation, "Earlier usable title")
        .await
        .expect("failed to apply older usable title");
    let newer_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", newer_generation, "Newer usable title")
        .await
        .expect("failed to apply newer usable title");
    let repeated_older_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", older_generation, "Repeated older title")
        .await
        .expect("failed to reject older title after newer candidate");

    // Assert
    let session_row = load_session_row(&database, "session-a").await;
    assert!(older_update_applied);
    assert!(newer_update_applied);
    assert!(!repeated_older_update_applied);
    assert_eq!(session_row.title.as_deref(), Some("Newer usable title"));
}

/// Verifies provisional fallbacks and authoritative titles invalidate every
/// outstanding title candidate.
#[tokio::test]
async fn test_session_title_authority_invalidates_outstanding_candidates() {
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
    database
        .sessions()
        .update_session_provisional_title("session-a", "First draft")
        .await
        .expect("failed to persist provisional title");
    let invalidated_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to claim provisional title generation")
        .expect("provisional title should permit generation");

    // Act
    database
        .sessions()
        .update_session_provisional_title("session-a", "New fallback")
        .await
        .expect("failed to replace provisional title");
    let invalidated_update_applied = database
        .sessions()
        .update_session_title_for_generation(
            "session-a",
            invalidated_generation,
            "Invalidated generated title",
        )
        .await
        .expect("failed to reject title invalidated by a newer fallback");
    let stale_generation = database
        .sessions()
        .begin_session_title_generation("session-a", false)
        .await
        .expect("failed to claim title generation before authoritative title")
        .expect("forced title generation should be claimed");
    database
        .sessions()
        .update_session_title("session-a", "Authoritative commit title")
        .await
        .expect("failed to persist authoritative title");
    let stale_update_applied = database
        .sessions()
        .update_session_title_for_generation("session-a", stale_generation, "Stale generated title")
        .await
        .expect("failed to reject stale title generation");
    let provisional_generation = database
        .sessions()
        .begin_session_title_generation("session-a", true)
        .await
        .expect("failed to inspect provisional title state");
    let forced_generation = database
        .sessions()
        .begin_session_title_generation("session-a", false)
        .await
        .expect("failed to claim forced title generation")
        .expect("forced title generation should be claimed");
    let forced_update_applied = database
        .sessions()
        .update_session_title_for_generation(
            "session-a",
            forced_generation,
            "Refine draft workflow title",
        )
        .await
        .expect("failed to apply current title generation");

    // Assert
    let session_row = load_session_row(&database, "session-a").await;
    assert!(!invalidated_update_applied);
    assert!(!stale_update_applied);
    assert_eq!(provisional_generation, None);
    assert!(forced_update_applied);
    assert_eq!(
        session_row.title.as_deref(),
        Some("Refine draft workflow title")
    );
}
