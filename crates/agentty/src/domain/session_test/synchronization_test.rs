//! Session review-request sync and rebase gate tests.

use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionStatus as Status,
};

use super::support::test_session;

#[test]
fn test_forge_indicator_returns_merged_symbol_with_display_id() {
    // Arrange
    let mut session = test_session(None);
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#99".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Merged,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    });

    // Act
    let indicator = session.forge_indicator();

    // Assert
    assert_eq!(indicator, "✓ #99");
}

#[test]
fn test_can_sync_review_request_true_for_review_with_published_ref() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Review;
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());

    // Act / Assert
    assert!(session.can_sync_review_request());
}

#[test]
fn test_can_sync_review_request_true_for_agent_review_with_review_request() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::AgentReview;
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#1".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    });

    // Act / Assert
    assert!(session.can_sync_review_request());
}

#[test]
fn test_can_sync_review_request_false_for_question_with_published_ref() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Question;
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());

    // Act / Assert
    assert!(!session.can_sync_review_request());
}

#[test]
fn test_can_sync_review_request_false_for_in_progress() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::InProgress;
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());

    // Act / Assert
    assert!(!session.can_sync_review_request());
}

#[test]
fn test_can_sync_review_request_false_for_done() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Done;
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());

    // Act / Assert
    assert!(!session.can_sync_review_request());
}

#[test]
fn test_can_sync_review_request_false_without_forge_context() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Review;

    // Act / Assert
    assert!(!session.can_sync_review_request());
}

#[test]
fn test_status_allows_rebase_action_for_review_ready_and_in_progress_states() {
    // Arrange
    let expected_statuses = [
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
        Status::Watching,
    ];

    // Act
    let allowed_statuses: Vec<Status> = Status::ALL
        .into_iter()
        .filter(|status| status.allows_rebase_action())
        .collect();

    // Assert
    assert_eq!(allowed_statuses, expected_statuses);
}

#[test]
fn test_review_request_state_display_merged() {
    // Arrange
    let review_request_state = ReviewRequestState::Merged;

    // Act
    let displayed_state = review_request_state.to_string();

    // Assert
    assert_eq!(displayed_state, "Merged");
}

#[test]
fn merged_status_is_read_only_and_only_transitions_to_done() {
    // Arrange
    let merged_status = Status::Merged;
    let review_status = Status::Review;

    // Act
    let merged_is_read_only = merged_status.is_read_only();
    let review_is_read_only = review_status.is_read_only();
    let review_can_merge = review_status.can_transition_to(merged_status);
    let merged_can_finish = merged_status.can_transition_to(Status::Done);
    let merged_can_repeat = merged_status.can_transition_to(Status::Merged);
    let merged_can_reopen = merged_status.can_transition_to(Status::Review);

    // Assert
    assert!(merged_is_read_only);
    assert!(!review_is_read_only);
    assert!(review_can_merge);
    assert!(merged_can_finish);
    assert!(merged_can_repeat);
    assert!(!merged_can_reopen);
}
