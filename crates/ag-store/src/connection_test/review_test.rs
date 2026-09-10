use ag_agent::{ReasoningLevel, SpeedMode};
use tempfile::tempdir;

use super::support::assert_review_request_row;
use crate::connection::Database;
use crate::test_support::review_request_fixture;
use crate::{PersistedSessionCreation, SessionFocusedReviewRow};

#[tokio::test]
async fn session_archived_diff_round_trips_empty_and_nonempty_values() {
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
        .sessions()
        .update_session_archived_diff("session-a", Some("diff --git a/a b/a".to_string()))
        .await
        .expect("failed to store archived diff");
    let stored_diff = database
        .sessions()
        .load_session_archived_diff("session-a")
        .await
        .expect("failed to load archived diff");
    database
        .sessions()
        .update_session_archived_diff("session-a", Some(String::new()))
        .await
        .expect("failed to store empty archived diff");
    let empty_diff = database
        .sessions()
        .load_session_archived_diff("session-a")
        .await
        .expect("failed to load empty archived diff");

    // Assert
    assert_eq!(stored_diff.as_deref(), Some("diff --git a/a b/a"));
    assert_eq!(empty_diff.as_deref(), Some(""));
}

#[tokio::test]
async fn test_session_review_request_round_trip_and_clear() {
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
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let review_request = review_request_fixture();

    // Act
    database
        .reviews()
        .update_session_review_request("session-a", Some(review_request.clone()))
        .await
        .expect("failed to persist session review request");
    let persisted_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing persisted session row");
    database
        .reviews()
        .update_session_review_request("session-a", None)
        .await
        .expect("failed to clear session review request");
    let cleared_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions after clearing")
        .into_iter()
        .find(|row| row.id == "session-a")
        .expect("missing cleared session row");

    // Assert
    assert_review_request_row(&persisted_row);
    assert_eq!(cleared_row.review_request, None);
}

#[tokio::test]
async fn test_load_session_focused_reviews_for_project_returns_persisted_review() {
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
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_focused_review(
            "session-a",
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("## Review\nPersisted".to_string()),
        )
        .await
        .expect("failed to update focused review");

    // Act
    let focused_reviews = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load focused reviews");

    // Assert
    assert_eq!(
        focused_reviews,
        vec![SessionFocusedReviewRow {
            diff_hash: "42".to_string(),
            session_id: "session-a".to_string(),
            text: "## Review\nPersisted".to_string(),
        }]
    );
}

#[tokio::test]
async fn review_diff_baseline_survives_output_clear_and_database_reopen() {
    // Arrange
    let directory = tempdir().expect("temporary directory should exist");
    let path = directory.path().join("review.db");
    let database = Database::open(&path).await.expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("review-project", None)
        .await
        .expect("project should persist");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("session should persist");

    // Act
    let first = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("empty baseline should load");
    database
        .sessions()
        .update_session_review_diff_hash("session-a", "42", false)
        .await
        .expect("baseline should persist");
    database
        .sessions()
        .update_session_focused_review("session-a", None, None, None)
        .await
        .expect("output should clear");
    database.pool().close().await;
    let database = Database::open(&path).await.expect("database should reopen");
    let recovered = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("baseline should survive restart");
    database
        .sessions()
        .update_session_review_diff_hash("session-a", "43", true)
        .await
        .expect("baseline and review claim should persist");
    database.pool().close().await;
    let database = Database::open(&path)
        .await
        .expect("claimed review should reopen");
    let pending = database
        .sessions()
        .load_pending_focused_review_session_ids(project_id)
        .await
        .expect("review claim should survive restart");
    let missing = database
        .sessions()
        .load_session_review_diff_hash("missing")
        .await
        .expect("missing session should be harmless");
    let unfinished = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("unfinished review should remain recoverable");
    database
        .sessions()
        .defer_session_focused_review("session-a")
        .await
        .expect("deferred review should persist");
    let deferred = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("deferred turn should retain its baseline");

    // Assert
    assert_eq!(first, None);
    assert_eq!(recovered.as_deref(), Some("42"));
    assert_eq!(pending, vec!["session-a".to_string()]);
    assert_eq!(missing, None);
    assert_eq!(unfinished, None);
    assert_eq!(deferred.as_deref(), Some("43"));
}

#[tokio::test]
async fn review_diff_claim_failure_rolls_back_baseline() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("review-project", None)
        .await
        .expect("project should persist");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("session should persist");
    database
        .sessions()
        .update_session_review_diff_hash("session-a", "42", false)
        .await
        .expect("old baseline should persist");
    sqlx::raw_sql(
        "CREATE TRIGGER reject_review_claim BEFORE UPDATE OF focused_review_status ON session
         WHEN NEW.focused_review_status = 'Pending' BEGIN SELECT RAISE(ABORT, 'claim failed'); END;",
    )
    .execute(database.pool())
    .await
    .expect("claim failure should be injected");

    // Act
    let result = database
        .sessions()
        .update_session_review_diff_hash("session-a", "43", true)
        .await;
    let baseline = database
        .sessions()
        .load_session_review_diff_hash("session-a")
        .await
        .expect("baseline should load after rollback");
    let pending = database
        .sessions()
        .load_pending_focused_review_session_ids(project_id)
        .await
        .expect("pending reviews should load");

    // Assert
    assert!(result.is_err());
    assert_eq!(baseline.as_deref(), Some("42"));
    assert_eq!(pending, [] as [String; 0]);
}

#[tokio::test]
async fn test_update_session_focused_review_clears_persisted_review() {
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
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_focused_review(
            "session-a",
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("## Review\nPersisted".to_string()),
        )
        .await
        .expect("failed to update focused review");

    // Act
    database
        .sessions()
        .update_session_focused_review("session-a", None, None, None)
        .await
        .expect("failed to clear focused review");
    let focused_reviews = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load focused reviews");

    // Assert
    assert_eq!(focused_reviews, [] as [crate::SessionFocusedReviewRow; 0]);
}

#[tokio::test]
async fn test_defer_session_focused_review_requires_eligible_existing_session() {
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
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    database
        .sessions()
        .update_session_focused_review(
            "session-a",
            Some(ag_session::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("## Review\nOutdated".to_string()),
        )
        .await
        .expect("failed to seed focused review");
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "orchestrator",
            is_draft: false,
            model: "gpt-5.6-sol",
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: ag_session::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("Orchestrator"),
            speed_mode: SpeedMode::Normal,
            status: "Review",
        })
        .await
        .expect("failed to insert orchestrator session");

    // Act
    let deferred = database
        .sessions()
        .defer_session_focused_review("session-a")
        .await
        .expect("failed to defer focused review");
    let missing_deferred = database
        .sessions()
        .defer_session_focused_review("missing-session")
        .await
        .expect("failed to check missing session");
    let orchestrator_deferred = database
        .sessions()
        .defer_session_focused_review("orchestrator")
        .await
        .expect("failed to check orchestrator session");
    let pending_session_ids = database
        .sessions()
        .load_pending_focused_review_session_ids(project_id)
        .await
        .expect("failed to load pending focused reviews");
    let focused_reviews = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load focused reviews");

    // Assert
    assert!(deferred);
    assert!(!missing_deferred);
    assert!(!orchestrator_deferred);
    assert_eq!(pending_session_ids, ["session-a"]);
    assert_eq!(focused_reviews, [] as [crate::SessionFocusedReviewRow; 0]);
}
