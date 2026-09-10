use tokio::io::AsyncBufReadExt;

use super::*;

/// Spawns a simple echo process that mirrors stdin to stdout for transport
/// write tests.
fn spawn_cat_process() -> (
    AppServerRuntimeChild,
    tokio::process::ChildStdin,
    tokio::process::ChildStdout,
) {
    let command = std::process::Command::new("cat");

    spawn_runtime_command(command, "cat").expect("failed to spawn `cat`")
}

#[test]
fn response_id_matches_returns_true_for_matching_string_id() {
    // Arrange
    let response_value = serde_json::json!({"id": "init-123", "result": {}});

    // Act / Assert
    assert!(response_id_matches(&response_value, "init-123"));
}

#[tokio::test]
async fn runtime_disables_optional_git_locks_for_tools() {
    // Arrange
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", "sh -c 'printf \"%s\\n\" \"$GIT_OPTIONAL_LOCKS\"'"])
        .env("GIT_OPTIONAL_LOCKS", "1");

    // Act
    let (mut child, stdin, stdout) =
        spawn_runtime_command(command, "test").expect("runtime should start");
    drop(stdin);
    let line = BufReader::new(stdout)
        .lines()
        .next_line()
        .await
        .expect("stdout should be readable");
    shutdown_child(&mut child).await;

    // Assert
    assert_eq!(line.as_deref(), Some("0"));
}

#[test]
fn response_id_matches_returns_false_for_different_id() {
    // Arrange
    let response_value = serde_json::json!({"id": "init-123", "result": {}});

    // Act / Assert
    assert!(!response_id_matches(&response_value, "init-456"));
}

#[test]
fn response_id_matches_returns_false_when_id_is_missing() {
    // Arrange
    let response_value = serde_json::json!({"method": "session/update", "params": {}});

    // Act / Assert
    assert!(!response_id_matches(&response_value, "init-123"));
}

#[test]
fn response_id_matches_returns_false_for_integer_id() {
    // Arrange
    let response_value = serde_json::json!({"id": 1, "result": {}});

    // Act / Assert
    assert!(!response_id_matches(&response_value, "1"));
}

#[test]
fn extract_json_error_message_returns_message_string() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "req-1",
        "error": {"code": -32600, "message": "Invalid request"}
    });

    // Act
    let message = extract_json_error_message(&response_value);

    // Assert
    assert_eq!(message, Some("Invalid request".to_string()));
}

#[test]
fn extract_json_error_message_returns_none_without_error() {
    // Arrange
    let response_value = serde_json::json!({"id": "req-1", "result": {}});

    // Act
    let message = extract_json_error_message(&response_value);

    // Assert
    assert_eq!(message, None);
}

#[test]
fn extract_json_error_message_returns_none_without_message_field() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "req-1",
        "error": {"code": -32600}
    });

    // Act
    let message = extract_json_error_message(&response_value);

    // Assert
    assert_eq!(message, None);
}

/// Verifies `write_json_line()` serializes one compact JSON line followed
/// by a newline.
#[tokio::test]
async fn write_json_line_writes_serialized_payload_with_newline() {
    // Arrange
    let (mut child, mut stdin, stdout) = spawn_cat_process();
    let payload = serde_json::json!({
        "id": "req-1",
        "method": "initialize",
        "params": {"value": 1}
    });

    // Act
    write_json_line(&mut stdin, &payload)
        .await
        .expect("write should succeed");
    drop(stdin);
    let echoed_line = BufReader::new(stdout)
        .lines()
        .next_line()
        .await
        .expect("stdout read should succeed")
        .expect("echoed payload line should exist");

    // Assert
    assert_eq!(echoed_line, payload.to_string());
    shutdown_child(&mut child).await;
}

/// Verifies `wait_for_response_line()` skips unrelated or invalid lines
/// until the matching response id arrives.
#[tokio::test]
async fn wait_for_response_line_skips_invalid_and_non_matching_lines() {
    // Arrange
    let (reader, mut writer) = tokio::io::duplex(512);
    let writer_task = tokio::spawn(async move {
        writer
            .write_all(
                b"not-json\n{\"id\":\"other\",\"result\":{}}\n{\"id\":\"req-1\",\"result\":{\"ok\":true}}\n",
            )
            .await
            .expect("test writer should succeed");
    });
    let mut stdout_lines = BufReader::new(reader).lines();

    // Act
    let response_line = wait_for_response_line(&mut stdout_lines, "req-1")
        .await
        .expect("matching response should be returned");

    // Assert
    assert_eq!(response_line, "{\"id\":\"req-1\",\"result\":{\"ok\":true}}");
    writer_task.await.expect("writer task should finish");
}

/// Verifies `wait_for_response_line()` reports early process termination
/// when the stream ends before the expected response arrives.
#[tokio::test]
async fn wait_for_response_line_returns_error_when_stream_ends() {
    // Arrange
    let (reader, mut writer) = tokio::io::duplex(256);
    let writer_task = tokio::spawn(async move {
        writer
            .write_all(b"{\"id\":\"other\",\"result\":{}}\n")
            .await
            .expect("test writer should succeed");
        drop(writer);
    });
    let mut stdout_lines = BufReader::new(reader).lines();

    // Act
    let response_result = wait_for_response_line(&mut stdout_lines, "req-1").await;

    // Assert
    assert!(
        matches!(
            response_result,
            Err(AppServerTransportError::ProcessTerminated)
        ),
        "expected ProcessTerminated, got: {response_result:?}"
    );
    writer_task.await.expect("writer task should finish");
}

/// Verifies provider-selected response deadlines are preserved in timeout
/// diagnostics instead of falling back to the shared startup window.
#[tokio::test]
async fn wait_for_response_line_with_timeout_uses_selected_deadline() {
    // Arrange
    let (reader, _writer) = tokio::io::duplex(256);
    let mut stdout_lines = BufReader::new(reader).lines();
    let response_timeout = Duration::from_millis(1);

    // Act
    let response_result =
        wait_for_response_line_with_timeout(&mut stdout_lines, "req-1", response_timeout).await;

    // Assert
    assert!(matches!(
        response_result,
        Err(AppServerTransportError::Timeout {
            ref response_id,
            timeout_seconds: 0,
        }) if response_id == "req-1"
    ));
}

#[test]
fn io_error_display_includes_context_and_source() {
    // Arrange
    let error = AppServerTransportError::Io {
        context: "Failed writing to app-server stdin".to_string(),
        source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed"),
    };

    // Act
    let display = error.to_string();

    // Assert
    assert_eq!(display, "Failed writing to app-server stdin: pipe closed");
}

#[test]
fn process_terminated_display_message() {
    // Arrange
    let error = AppServerTransportError::ProcessTerminated;

    // Act / Assert
    assert_eq!(
        error.to_string(),
        "App-server terminated before sending expected response"
    );
}

#[test]
fn timeout_display_includes_response_id_and_seconds() {
    // Arrange
    let error = AppServerTransportError::Timeout {
        response_id: "init-123".to_string(),
        timeout_seconds: 300,
    };

    // Act / Assert
    assert_eq!(
        error.to_string(),
        "Timed out waiting for app-server response `init-123` after 300 seconds"
    );
}

/// Verifies `shutdown_child()` closes stdin and reaps a cooperative child.
#[tokio::test]
async fn shutdown_child_reaps_process_after_closing_stdin() {
    // Arrange
    let (mut child, stdin, _stdout) = spawn_cat_process();

    // Act
    drop(stdin);
    shutdown_child(&mut child).await;

    // Assert
    assert!(child.id().is_none());
}

/// Verifies forced shutdown reaches descendants spawned by an app-server.
#[tokio::test]
async fn shutdown_child_terminates_runtime_process_group() {
    // Arrange
    let mut command = std::process::Command::new("sh");
    command.args([
        "-c",
        "trap '' TERM; sleep 60 & echo ready; cat >/dev/null; wait",
    ]);
    let (mut child, stdin, stdout) =
        spawn_runtime_command(command, "process-group fixture").expect("fixture should spawn");
    let mut stdout_lines = BufReader::new(stdout).lines();
    let readiness_line = stdout_lines
        .next_line()
        .await
        .expect("fixture readiness read should succeed")
        .expect("fixture readiness line should be present");
    assert_eq!(readiness_line, "ready");

    // Act
    drop(stdin);
    shutdown_child(&mut child).await;
    let stdout_closed =
        tokio::time::timeout(Duration::from_secs(2), stdout_lines.next_line()).await;

    // Assert
    assert!(
        matches!(stdout_closed, Ok(Ok(None))),
        "runtime descendant should release inherited stdout when its process group terminates"
    );
}
