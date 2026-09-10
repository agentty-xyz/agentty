use std::sync::Arc;

use ag_session::{
    CreateSessionMode, CreateSessionRequest, SessionError as ApiSessionError, SessionId,
    SessionStatus,
};
use tokio::sync::oneshot;

use super::super::api_review_request_from_row;
use super::support::{request_review_request, request_session_creation, session_row};
use crate::app::SessionRuntimeAccess;
use crate::app::branch_publish::{BranchPublishTaskSuccess, review_request_from_publish_result};
use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};

#[tokio::test]
async fn review_request_queue_rejects_stale_in_progress_session_without_worker() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("regular session should be created");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        SessionStatus::InProgress,
    );

    // Act
    let result = request_review_request(&mut app, session_id).await;

    // Assert
    assert!(matches!(
        result,
        Err(ApiSessionError::Operation(message))
            if message.contains("active session worker is unavailable")
    ));
}

#[tokio::test]
async fn review_request_runtime_handler_does_not_wait_for_branch_operation() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = SessionId::from(
        app.create_session()
            .await
            .expect("regular session should be created"),
    );
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let (response_tx, mut response_rx) = oneshot::channel();

    // Act
    let start_result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.start_api_review_request_publish(
            session_id.clone(),
            SessionRuntimeAccess::User,
            response_tx,
        ),
    )
    .await;
    let response_before_unlock = response_rx.try_recv();
    drop(existing_operation_guard);
    let response_after_unlock = response_rx
        .await
        .expect("review-request result should be delivered after the lock is released");

    // Assert
    assert!(
        start_result.is_ok(),
        "runtime command handling should not wait for the branch operation"
    );
    assert!(matches!(
        response_before_unlock,
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(
        matches!(
            &response_after_unlock,
            Err(ApiSessionError::Operation(message))
                if message == "Session must be in review to publish the review request."
        ),
        "unexpected review-request response: {response_after_unlock:?}"
    );
}

#[tokio::test]
async fn review_request_runtime_handler_queues_on_existing_worker() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = SessionId::from(
        app.create_session()
            .await
            .expect("regular session should be created"),
    );
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let (first_response_tx, _first_response_rx) = oneshot::channel();
    app.start_api_review_request_publish(
        session_id.clone(),
        SessionRuntimeAccess::User,
        first_response_tx,
    )
    .await;
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        SessionStatus::InProgress,
    );
    let (queued_response_tx, _queued_response_rx) = oneshot::channel();

    // Act
    app.start_api_review_request_publish(
        session_id.clone(),
        SessionRuntimeAccess::User,
        queued_response_tx,
    )
    .await;
    let publish_body = app.sessions.state().sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::BranchPublish)
        .map(|message| &message.body);

    // Assert
    assert!(matches!(
        publish_body,
        Some(TransientMessageBody::Queued(action))
            if action.order == 0 && action.text == "review request — publish after this turn"
    ));
    drop(existing_operation_guard);
}

#[test]
fn api_review_request_result_requires_a_published_review_request() {
    // Arrange
    let review_request = api_review_request_from_row(
        session_row()
            .review_request
            .expect("review-request fixture should exist"),
    )
    .expect("review-request fixture should parse");
    let published_result = Ok(BranchPublishTaskSuccess::PullRequestPublished {
        branch_name: "wt/session-1".to_string(),
        review_request: review_request.clone(),
        upstream_reference: "origin/wt/session-1".to_string(),
    });
    let pushed_result = Ok(BranchPublishTaskSuccess::Pushed {
        branch_name: "wt/session-1".to_string(),
        review_request_creation: None,
        upstream_reference: "origin/wt/session-1".to_string(),
    });

    // Act
    let published_review_request = review_request_from_publish_result(&published_result);
    let pushed_error = review_request_from_publish_result(&pushed_result)
        .expect_err("a plain branch-push result should not satisfy the API request");

    // Assert
    assert_eq!(published_review_request, Ok(review_request));
    assert_eq!(
        pushed_error,
        "Review-request publishing completed without a review request".to_string()
    );
}
