use crate::review::NewSessionReviewCommentResolution;
use crate::{AppRepositories, DbError};

#[tokio::test]
async fn active_review_comment_operation_preserves_original_reply_across_retries() {
    // Arrange
    let repositories = AppRepositories::in_memory()
        .await
        .expect("failed to open in-memory repositories");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    repositories
        .sessions()
        .insert_session("session-id", "codex", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let original = review_comment_resolution("Original reply", "token-1");
    let regenerated = review_comment_resolution("Regenerated reply", "token-2");
    repositories
        .reviews()
        .insert_session_review_comment_resolutions("session-id", std::slice::from_ref(&original))
        .await
        .expect("failed to insert original operation");
    repositories
        .reviews()
        .bind_session_review_comment_resolutions_to_commit(
            "session-id",
            std::slice::from_ref(&original),
            "commit-original",
        )
        .await
        .expect("failed to bind original operation");

    // Act
    repositories
        .reviews()
        .insert_session_review_comment_resolutions("session-id", std::slice::from_ref(&regenerated))
        .await
        .expect("failed to ignore regenerated operation");
    repositories
        .reviews()
        .bind_session_review_comment_resolutions_to_commit(
            "session-id",
            std::slice::from_ref(&regenerated),
            "commit-regenerated",
        )
        .await
        .expect("failed to ignore regenerated binding");
    let active = repositories
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load active operation");
    repositories
        .reviews()
        .mark_session_review_comment_resolution_posting("session-id", "token-1")
        .await
        .expect("failed to mark original operation as posting");
    let posting = repositories
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to reload posting operation");
    repositories
        .reviews()
        .remove_session_review_comment_resolution("session-id", "token-1")
        .await
        .expect("failed to remove original operation");
    repositories
        .reviews()
        .insert_session_review_comment_resolutions("session-id", &[regenerated])
        .await
        .expect("failed to insert later operation");
    let replacement = repositories
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load replacement operation");
    let missing_update_error = repositories
        .reviews()
        .mark_session_review_comment_resolution_posting("session-id", "missing-token")
        .await
        .expect_err("missing operation should reject state update");
    let missing_delete_error = repositories
        .reviews()
        .remove_session_review_comment_resolution("session-id", "missing-token")
        .await
        .expect_err("missing operation should reject deletion");

    // Assert
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].reply, "Original reply");
    assert_eq!(active[0].reply_token, "token-1");
    assert_eq!(active[0].commit_hash.as_deref(), Some("commit-original"));
    assert!(!active[0].is_posting);
    assert!(posting[0].is_posting);
    assert_eq!(replacement.len(), 1);
    assert_eq!(replacement[0].reply, "Regenerated reply");
    assert!(matches!(missing_update_error, DbError::InvalidData { .. }));
    assert!(matches!(missing_delete_error, DbError::InvalidData { .. }));
}

#[tokio::test]
async fn discard_failed_retry_preserves_older_conflicting_operation() {
    // Arrange
    let repositories = AppRepositories::in_memory()
        .await
        .expect("failed to open in-memory repositories");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    repositories
        .sessions()
        .insert_session("session-id", "codex", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let original = review_comment_resolution("Original reply", "token-original");
    let regenerated = review_comment_resolution("Regenerated reply", "token-regenerated");
    let mut unrelated = review_comment_resolution("Unrelated reply", "token-unrelated");
    unrelated.thread_id = "thread-2".to_string();
    let mut inserted = review_comment_resolution("Inserted reply", "token-inserted");
    inserted.thread_id = "thread-3".to_string();
    repositories
        .reviews()
        .insert_session_review_comment_resolutions(
            "session-id",
            &[original.clone(), unrelated.clone()],
        )
        .await
        .expect("failed to insert review operations");
    repositories
        .reviews()
        .bind_session_review_comment_resolutions_to_commit(
            "session-id",
            std::slice::from_ref(&original),
            "commit-original",
        )
        .await
        .expect("failed to bind original operation");
    repositories
        .reviews()
        .insert_session_review_comment_resolutions(
            "session-id",
            &[regenerated.clone(), inserted.clone()],
        )
        .await
        .expect("failed to insert later review operations");

    // Act
    repositories
        .reviews()
        .discard_session_review_comment_resolutions("session-id", &[regenerated, inserted])
        .await
        .expect("failed to discard newly inserted review operations");
    let remaining = repositories
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load remaining review operations");

    // Assert
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].reply_token, original.reply_token);
    assert_eq!(remaining[0].reply, original.reply);
    assert_eq!(remaining[1].reply_token, unrelated.reply_token);
    assert_eq!(remaining[1].reply, unrelated.reply);
}

#[tokio::test]
async fn fresh_retry_replaces_unbound_operation() {
    // Arrange
    let repositories = AppRepositories::in_memory()
        .await
        .expect("failed to open in-memory repositories");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    repositories
        .sessions()
        .insert_session("session-id", "codex", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let original = review_comment_resolution("Original reply", "token-original");
    let regenerated = review_comment_resolution("Regenerated reply", "token-regenerated");
    repositories
        .reviews()
        .insert_session_review_comment_resolutions("session-id", &[original])
        .await
        .expect("failed to insert unbound operation");

    // Act
    repositories
        .reviews()
        .insert_session_review_comment_resolutions("session-id", std::slice::from_ref(&regenerated))
        .await
        .expect("failed to replace unbound operation");
    repositories
        .reviews()
        .bind_session_review_comment_resolutions_to_commit(
            "session-id",
            std::slice::from_ref(&regenerated),
            "commit-regenerated",
        )
        .await
        .expect("failed to bind replacement operation");
    let active = repositories
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load replacement operation");

    // Assert
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].reply, "Regenerated reply");
    assert_eq!(active[0].reply_token, "token-regenerated");
    assert_eq!(active[0].commit_hash.as_deref(), Some("commit-regenerated"));
    assert!(!active[0].is_posting);
}

fn review_comment_resolution(reply: &str, reply_token: &str) -> NewSessionReviewCommentResolution {
    NewSessionReviewCommentResolution {
        commit_hash: None,
        reply: reply.to_string(),
        reply_token: reply_token.to_string(),
        resolution: "fixed".to_string(),
        review_request_display_id: "#42".to_string(),
        thread_id: "thread-1".to_string(),
    }
}
