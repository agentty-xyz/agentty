use std::sync::Arc;

use super::support::FakeBackend;
use crate::{DurableMessageVisibility, SessionError, SessionId, SessionService, SubmitTaskRequest};

#[tokio::test]
async fn existing_hosts_report_unsupported_task_capability() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));
    let request = SubmitTaskRequest {
        attempt_id: "attempt".to_string(),
        message: "Continue".to_string(),
        objective: "Desired outcome".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Hidden,
    };

    // Act
    let submitted = service
        .submit_task(&SessionId::from("session"), request)
        .await;
    let loaded = service.get_task("task").await;
    let completed = service.complete_task("task").await;

    // Assert
    let expected = SessionError::Operation("Host does not support tasks".to_string());
    assert_eq!(submitted, Err(expected.clone()));
    assert_eq!(loaded, Err(expected.clone()));
    assert_eq!(completed, Err(expected));
}
