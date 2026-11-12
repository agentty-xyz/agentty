use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;
use tokio::task::JoinHandle;

use crate::agent::cli::stdin::{
    await_optional_stdin_write, is_broken_pipe_error, spawn_optional_stdin_write,
    write_optional_stdin,
};

/// Test-only error type used to verify generic error formatting.
#[derive(Debug, PartialEq)]
struct TestError(String);

/// Converts a string message into `TestError` for use as the
/// `format_error` callback.
fn make_test_error(message: String) -> TestError {
    TestError(message)
}

/// Stdin stand-in whose writes and shutdown fail with configured error kinds.
struct FailingStdin {
    shutdown_error: Option<io::ErrorKind>,
    write_error: Option<io::ErrorKind>,
}

impl AsyncWrite for FailingStdin {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(
            self.write_error
                .map_or(Ok(buffer.len()), |kind| Err(kind.into())),
        )
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.shutdown_error.map_or(Ok(()), |kind| Err(kind.into())))
    }
}

// --- write_optional_stdin ---

#[tokio::test]
async fn test_write_tolerates_closed_pipes_and_reports_other_failures() {
    // Arrange
    let write_cases = [
        (
            Some(io::ErrorKind::BrokenPipe),
            Some(io::ErrorKind::BrokenPipe),
            None,
        ),
        (
            Some(io::ErrorKind::PermissionDenied),
            None,
            Some("Failed to write stdin payload: permission denied"),
        ),
        (
            None,
            Some(io::ErrorKind::PermissionDenied),
            Some("Failed to close stdin payload: permission denied"),
        ),
    ];

    for (write_error, shutdown_error, expected_error) in write_cases {
        let child_stdin = FailingStdin {
            shutdown_error,
            write_error,
        };

        // Act
        let result = write_optional_stdin(
            Some(child_stdin),
            b"prompt".to_vec(),
            "stdin unavailable",
            make_test_error,
        )
        .await;

        // Assert
        assert_eq!(
            result,
            expected_error.map_or(Ok(()), |message| Err(TestError(message.to_string())))
        );
    }
}

// --- spawn_optional_stdin_write ---

#[test]
fn test_spawn_returns_none_when_no_payload() {
    // Arrange / Act
    let result =
        spawn_optional_stdin_write::<TestError>(None, None, "stdin unavailable", make_test_error);

    // Assert
    assert!(result.is_none());
}

#[tokio::test]
async fn test_spawn_returns_error_when_stdin_unavailable() {
    // Arrange
    let payload = Some(b"prompt".to_vec());

    // Act
    let handle = spawn_optional_stdin_write::<TestError>(
        None,
        payload,
        "stdin unavailable",
        make_test_error,
    );
    let join_result = handle
        .expect("expected a task handle")
        .await
        .expect("task should not panic");

    // Assert
    assert_eq!(join_result, Err(TestError("stdin unavailable".to_string())));
}

#[tokio::test]
async fn test_spawn_writes_payload_to_child_stdin() {
    // Arrange
    let mut child_command = tokio::process::Command::new("cat");
    child_command.stdin(std::process::Stdio::piped());
    child_command.stdout(std::process::Stdio::piped());
    let mut child = child_command.spawn().expect("failed to spawn cat");
    let child_stdin = child.stdin.take();
    let payload = Some(b"hello world".to_vec());

    // Act
    let handle = spawn_optional_stdin_write::<TestError>(
        child_stdin,
        payload,
        "stdin unavailable",
        make_test_error,
    );
    let write_result = handle
        .expect("expected a task handle")
        .await
        .expect("task should not panic");

    // Assert
    assert!(write_result.is_ok());
    let output = child
        .wait_with_output()
        .await
        .expect("failed to read cat output");
    assert_eq!(output.stdout, b"hello world");
}

// --- await_optional_stdin_write ---

#[tokio::test]
async fn test_await_returns_ok_when_no_task() {
    // Arrange / Act
    let result =
        await_optional_stdin_write::<TestError>(None, "join failed", make_test_error).await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_await_propagates_inner_error() {
    // Arrange
    let handle = tokio::spawn(async { Err(TestError("write failed".to_string())) });

    // Act
    let result = await_optional_stdin_write(Some(handle), "join failed", make_test_error).await;

    // Assert
    assert_eq!(result, Err(TestError("write failed".to_string())));
}

#[tokio::test]
async fn test_await_returns_join_error_when_task_is_aborted() {
    // Arrange
    let handle: JoinHandle<Result<(), TestError>> = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_mins(1)).await;

        Ok(())
    });
    handle.abort();

    // Act
    let result = await_optional_stdin_write(Some(handle), "join failed", make_test_error).await;

    // Assert
    let error = result.expect_err("expected a join error");
    assert!(error.0.starts_with("join failed:"));
}

// --- is_broken_pipe_error ---

#[test]
fn test_broken_pipe_returns_true() {
    // Arrange
    let error = io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe");

    // Act / Assert
    assert!(is_broken_pipe_error(&error));
}

#[test]
fn test_non_broken_pipe_returns_false() {
    // Arrange
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "denied");

    // Act / Assert
    assert!(!is_broken_pipe_error(&error));
}
