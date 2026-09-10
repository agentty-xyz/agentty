use std::time::Duration;

use ag_session::{SessionError, SessionId};

use super::{
    SESSION_RUNTIME_COMMAND_CAPACITY, SESSION_RUNTIME_UNAVAILABLE, SessionRuntime,
    SessionRuntimeHandle,
};

fn assert_clone_send_sync<T: Clone + Send + Sync>() {}

#[test]
fn runtime_handle_is_cloneable_send_and_sync() {
    // Arrange / Act / Assert
    assert_clone_send_sync::<SessionRuntimeHandle>();
}

#[tokio::test]
async fn handle_reports_unavailable_after_runtime_drops() {
    // Arrange
    let runtime = SessionRuntime::new(crate::test_support::session_manager_with_handles(
        Vec::new(),
        std::collections::HashMap::new(),
    ));
    let handle = runtime.handle();
    let _consumer = runtime.foreground_consumer();
    drop(runtime);

    // Act
    let error = handle
        .get_session(&SessionId::from("session-id"))
        .await
        .expect_err("closed runtime should reject commands");

    // Assert
    assert_eq!(
        error,
        SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
    );
}

#[tokio::test]
async fn handle_reports_unavailable_when_command_response_drops() {
    // Arrange
    let mut runtime = SessionRuntime::new(crate::test_support::session_manager_with_handles(
        Vec::new(),
        std::collections::HashMap::new(),
    ));
    let handle = runtime.handle();
    let _consumer = runtime.foreground_consumer();
    let request =
        tokio::spawn(async move { handle.get_session(&SessionId::from("session-id")).await });
    let command = runtime.next_command().await;
    drop(command);

    // Act
    let error = request
        .await
        .expect("request task should complete")
        .expect_err("dropped response should fail");

    // Assert
    assert_eq!(
        error,
        SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
    );
}

#[tokio::test]
async fn live_undriven_runtime_rejects_requests_without_waiting() {
    // Arrange
    let runtime = SessionRuntime::new(crate::test_support::session_manager_with_handles(
        Vec::new(),
        std::collections::HashMap::new(),
    ));
    let handle = runtime.handle();

    // Act
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        handle.get_session(&SessionId::from("session-id")),
    )
    .await
    .expect("undriven runtime request should not hang")
    .expect_err("undriven runtime should reject requests");

    // Assert
    assert_eq!(
        error,
        SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
    );
}

#[tokio::test]
async fn pending_response_stops_waiting_when_consumer_stops() {
    // Arrange
    let mut runtime = SessionRuntime::new(crate::test_support::session_manager_with_handles(
        Vec::new(),
        std::collections::HashMap::new(),
    ));
    let handle = runtime.handle();
    let consumer = runtime.foreground_consumer();
    let request =
        tokio::spawn(async move { handle.get_session(&SessionId::from("session-id")).await });
    let command = runtime.next_command().await;

    // Act
    drop(consumer);
    let error = request
        .await
        .expect("request task should complete")
        .expect_err("stopped consumer should fail the pending response");
    drop(command);

    // Assert
    assert_eq!(
        error,
        SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
    );
}

#[tokio::test]
async fn pending_send_stops_waiting_when_consumer_stops() {
    // Arrange
    let runtime = SessionRuntime::new(crate::test_support::session_manager_with_handles(
        Vec::new(),
        std::collections::HashMap::new(),
    ));
    let consumer = runtime.foreground_consumer();
    let mut queued_requests = Vec::new();
    for request_index in 0..SESSION_RUNTIME_COMMAND_CAPACITY {
        let handle = runtime.handle();
        queued_requests.push(tokio::spawn(async move {
            handle
                .get_session(&SessionId::from(format!("session-{request_index}")))
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.command_rx.len() < SESSION_RUNTIME_COMMAND_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runtime mailbox should fill");
    let blocked_handle = runtime.handle();
    let blocked_request = tokio::spawn(async move {
        blocked_handle
            .get_session(&SessionId::from("blocked-session"))
            .await
    });
    tokio::task::yield_now().await;
    assert!(!blocked_request.is_finished());

    // Act
    drop(consumer);
    let blocked_error = blocked_request
        .await
        .expect("blocked request task should complete")
        .expect_err("stopped consumer should fail the pending send");
    for queued_request in queued_requests {
        queued_request
            .await
            .expect("queued request task should complete")
            .expect_err("stopped consumer should fail queued responses");
    }

    // Assert
    assert_eq!(
        blocked_error,
        SessionError::Operation(SESSION_RUNTIME_UNAVAILABLE.to_string())
    );
}
