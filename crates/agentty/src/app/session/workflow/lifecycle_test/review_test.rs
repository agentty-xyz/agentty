use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::support::{
    database_with_session, review_request_summary, session_manager_with_one_session, test_services,
    test_session,
};
use crate::domain::session::{ReviewRequest, Status};

#[tokio::test]
async fn test_review_request_web_url_returns_linked_review_request_url() {
    // Arrange
    let mut session = test_session(
        "Implement forge review support",
        Status::Done,
        Some("Add forge review support"),
        "",
    );
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 42,
        summary: review_request_summary("#11"),
    });
    let session_manager = session_manager_with_one_session(session);
    let database = database_with_session(
        session_manager
            .state
            .sessions
            .first()
            .expect("fixture session should exist"),
    )
    .await;
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_review_request_web_url()
        .times(1)
        .returning(|summary| Ok(summary.web_url.clone()));
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(mock_review_request_client),
    );

    // Act
    let review_request_url = session_manager
        .review_request_web_url(&services, "session-id")
        .expect("linked review request URL should be returned");

    // Assert
    assert_eq!(
        review_request_url,
        "https://github.com/agentty-xyz/agentty/pull/11"
    );
}

#[tokio::test]
async fn test_review_request_web_url_reports_missing_link_and_forge_failures() {
    for linked in [false, true] {
        // Arrange
        let mut session = test_session("Prompt", Status::Review, Some("Title"), "");
        if linked {
            session.review_request = Some(ReviewRequest {
                last_refreshed_at: 42,
                summary: review_request_summary("#11"),
            });
        }
        let database = database_with_session(&session).await;
        let session_manager = session_manager_with_one_session(session);
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_review_request_web_url()
            .times(usize::from(linked))
            .returning(|_| {
                Err(forge::ReviewRequestError::UnsupportedRemote {
                    repo_url: "file:///tmp/project".to_string(),
                })
            });
        let services = test_services(
            &database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(mock_review_request_client),
        );

        // Act
        let error = session_manager
            .review_request_web_url(&services, "session-id")
            .expect_err("URL lookup should fail");

        // Assert
        let expected = if linked {
            "file:///tmp/project"
        } else {
            "Session has no linked review request"
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
}

/// Ensures the review-comment reply entry point rejects stale session
/// identifiers before attempting to enqueue worker commands.
#[tokio::test]
async fn test_reply_to_review_comments_returns_false_for_missing_session() {
    // Arrange
    let session = test_session("Initial prompt", Status::Review, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let enqueued = session_manager
        .reply_to_review_comments(
            &services,
            "missing-session",
            "Resolve this thread",
            vec!["thread-42".to_string()],
        )
        .await;

    // Assert
    assert!(!enqueued);
}
