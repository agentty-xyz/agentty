use crate::DurableMessageVisibility;
use crate::task::SubmitTaskRequest;

#[test]
fn submit_task_request_reserves_an_attempt_scoped_operation_id() {
    // Arrange
    let request = SubmitTaskRequest {
        attempt_id: "attempt-1".to_string(),
        message: "Implement search".to_string(),
        objective: "Search".to_string(),
        task_id: "task-1".to_string(),
        visibility: DurableMessageVisibility::Hidden,
    };

    // Act
    let operation_id = request.operation_id();

    // Assert
    assert_eq!(operation_id, "task-attempt:attempt-1");
}
