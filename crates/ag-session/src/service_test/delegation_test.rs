//! Session service delegation tests for backend reads and mutating operations.

use std::collections::VecDeque;
use std::sync::Arc;

use super::support::{FakeBackend, FakeBackendState, review_request_fixture, session_fixture};
use crate::error::SessionError;
use crate::model::SessionId;
use crate::service::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, QuestionAnswer, SessionOperation, SessionService,
};

#[tokio::test]
async fn service_delegates_create_and_get() {
    // Arrange
    let expected_session = session_fixture();
    let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
        create_results: VecDeque::from([Ok(SessionId::from("session-1"))]),
        get_result: Some(Ok(Some(expected_session.clone()))),
        ..FakeBackendState::default()
    }));
    let service = SessionService::new(backend.clone());

    // Act
    let session_id = service
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: 7,
        })
        .await
        .expect("session should be created");
    let loaded_session = service
        .get_session(&session_id)
        .await
        .expect("session should load");

    // Assert
    assert_eq!(loaded_session, Some(expected_session));
    assert_eq!(backend.calls(), ["create:Regular"]);
}

#[tokio::test]
async fn service_delegates_mutating_operations_through_clones() {
    // Arrange
    let expected_review_request = review_request_fixture();
    let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
        review_result: Some(Ok(expected_review_request.clone())),
        unit_results: VecDeque::from([Ok(()), Ok(()), Ok(()), Ok(()), Ok(())]),
        ..FakeBackendState::default()
    }));
    let session_id = SessionId::from("session-1");
    let service = SessionService::new(backend.clone());
    let cloned_service = service.clone();
    let answers = AnswerQuestionsRequest {
        answers: vec![QuestionAnswer {
            answer: "main".to_string(),
            question: "Which branch?".to_string(),
        }],
        attachments: Vec::new(),
    };

    // Act
    service
        .send_message(&session_id, "continue")
        .await
        .expect("message should be sent");
    service
        .submit_durable_message(
            &session_id,
            DurableMessageRequest {
                message: "roll up".to_string(),
                operation_id: "rollup-7".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await
        .expect("coordinator message should be submitted");
    cloned_service
        .answer_questions(&session_id, answers)
        .await
        .expect("questions should be answered");
    service
        .cancel_session(&session_id)
        .await
        .expect("cancel should be requested");
    cloned_service
        .merge_session(&session_id)
        .await
        .expect("merge should be requested");
    let review_request = service
        .create_review_request(&session_id)
        .await
        .expect("review request should be created");

    // Assert
    assert_eq!(review_request, expected_review_request);
    assert_eq!(
        backend.calls(),
        [
            "send:session-1:continue",
            "submit-coordinator:session-1:rollup-7:roll up",
            "answer:session-1:1",
            "cancel:session-1",
            "merge:session-1",
            "review:session-1"
        ]
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_session_listing() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let listed = service.list_sessions().await;

    // Assert
    assert_eq!(
        listed,
        Err(SessionError::Operation(
            "Host does not support session listing".to_string()
        ))
    );
}

#[test]
fn existing_hosts_serve_no_optional_operations() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let supports_merge = service.supports(SessionOperation::MergeSession);

    // Assert
    assert!(!supports_merge);
}

#[tokio::test]
async fn existing_hosts_report_unsupported_host_context() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let host = service.host_context().await;
    let supports_host = service.supports(SessionOperation::HostContext);

    // Assert
    assert_eq!(
        host,
        Err(SessionError::Operation(
            "Host does not describe its project".to_string()
        ))
    );
    assert!(!supports_host);
}
