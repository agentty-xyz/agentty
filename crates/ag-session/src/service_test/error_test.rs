use std::collections::VecDeque;
use std::sync::Arc;

use super::support::{FakeBackend, FakeBackendState};
use crate::error::SessionError;
use crate::model::SessionId;
use crate::service::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CoordinatorMessageVisibility,
    CreateSessionMode, CreateSessionRequest, SessionService,
};

#[tokio::test]
async fn service_preserves_backend_errors() {
    // Arrange
    let expected_error = SessionError::Operation("cannot create".to_string());
    let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
        create_results: VecDeque::from([Err(expected_error.clone())]),
        ..FakeBackendState::default()
    }));
    let service = SessionService::new(backend);

    // Act
    let error = service
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id: 7,
        })
        .await
        .expect_err("backend error should be preserved");

    // Assert
    assert_eq!(error, expected_error);
}

#[tokio::test]
async fn fake_backend_requires_explicit_results() {
    // Arrange
    let backend = Arc::new(FakeBackend::default());
    let session_id = SessionId::from("session-1");
    let service = SessionService::new(backend);

    // Act
    let create_error = service
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: 7,
        })
        .await
        .expect_err("create should require a result");
    let get_error = service
        .get_session(&session_id)
        .await
        .expect_err("get should require a result");
    let send_error = service
        .send_message(&session_id, "continue")
        .await
        .expect_err("send should require a result");
    let coordinator_error = service
        .submit_coordinator_message(
            &session_id,
            CoordinatorMessageRequest {
                message: "roll up".to_string(),
                operation_id: "rollup-1".to_string(),
                visibility: CoordinatorMessageVisibility::Hidden,
            },
        )
        .await
        .expect_err("coordinator submission should require a result");
    let answer_error = service
        .answer_questions(
            &session_id,
            AnswerQuestionsRequest {
                answers: Vec::new(),
            },
        )
        .await
        .expect_err("answers should require a result");
    let cancel_error = service
        .cancel_session(&session_id)
        .await
        .expect_err("cancel should require a result");
    let merge_error = service
        .merge_session(&session_id)
        .await
        .expect_err("merge should require a result");
    let review_error = service
        .create_review_request(&session_id)
        .await
        .expect_err("review should require a result");
    let errors = [
        create_error,
        get_error,
        send_error,
        coordinator_error,
        answer_error,
        cancel_error,
        merge_error,
        review_error,
    ];

    // Assert
    assert!(
        errors
            .into_iter()
            .all(|error| error == SessionError::Operation("missing result".to_string()))
    );
}
