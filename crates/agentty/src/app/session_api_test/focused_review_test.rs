//! Session API focused-review tests covering what a session snapshot reports
//! while a review runs, after it finishes or fails, and outside review.

use ag_session::{
    CreateSessionMode, CreateSessionRequest, SessionFocusedReview, SessionId, SessionStatus,
};

use super::support::{request_session, request_session_creation};
use crate::app::App;
use crate::app::review::{self, ReviewCacheEntry};

/// Creates one regular session persisted and shown in `status`.
async fn session_in(app: &mut App, status: SessionStatus) -> SessionId {
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("regular session should be created");
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &status.to_string(), 0)
        .await
        .expect("status should persist");
    crate::test_support::set_session_status_for_test(app, &session_id, status);

    session_id
}

#[tokio::test]
async fn runtime_backend_reports_the_raw_text_of_a_finished_focused_review() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = session_in(&mut app, SessionStatus::Review).await;
    app.set_review_ready_output(&session_id, 42, "### Suggestions\n- Add a test".to_string());

    // Act
    let session = request_session(&mut app, session_id)
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    assert_eq!(session.status, SessionStatus::Review);
    assert_eq!(
        session.focused_review,
        Some(SessionFocusedReview::Ready {
            text: "### Suggestions\n- Add a test".to_string(),
        })
    );
}

#[tokio::test]
async fn runtime_backend_reports_a_failed_focused_review_as_its_message() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = session_in(&mut app, SessionStatus::Review).await;
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Failed {
            diff_hash: 42,
            error: "timeout".to_string(),
            request_id: uuid::Uuid::new_v4(),
        },
    );

    // Act
    let session = request_session(&mut app, session_id)
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    assert_eq!(
        session.focused_review,
        Some(SessionFocusedReview::Failed {
            message: "Review assist unavailable: timeout".to_string(),
        })
    );
}

#[tokio::test]
async fn runtime_backend_reports_a_running_review_by_its_live_status_and_progress() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = session_in(&mut app, SessionStatus::Review).await;
    app.set_review_ready_output(&session_id, 42, "older review".to_string());
    let review_agent = app.review_agent();
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Loading {
            diff_hash: 43,
            progress: None,
            request_id: uuid::Uuid::new_v4(),
            review_agent,
        },
    );
    review::hydrate_review_transient(&app.review_cache, app.sessions.state_mut(), &session_id);

    // Act
    let session = request_session(&mut app, session_id)
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    // The row still says `Review`; the snapshot reports what the list does.
    let row = app
        .services
        .db()
        .sessions()
        .load_session(&session.id)
        .await
        .expect("row should load")
        .expect("row should exist");
    assert_eq!(row.status, SessionStatus::Review.to_string());
    assert_eq!(session.status, SessionStatus::AgentReview);
    assert_eq!(session.focused_review, None);
    assert!(
        session
            .progress
            .as_deref()
            .is_some_and(|progress| progress.starts_with("Reviewing changes")),
        "unexpected progress: {:?}",
        session.progress
    );
}

#[tokio::test]
async fn runtime_backend_hides_a_focused_review_outside_review_statuses() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = session_in(&mut app, SessionStatus::Done).await;
    app.set_review_ready_output(&session_id, 42, "review".to_string());

    // Act
    let session = request_session(&mut app, session_id)
        .await
        .expect("session should load")
        .expect("session should exist");

    // Assert
    assert_eq!(session.status, SessionStatus::Done);
    assert_eq!(session.focused_review, None);
}
