//! Agent CLI stdin payload writer unit tests.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use ag_protocol::TurnPrompt;
use tokio::io::{AsyncReadExt as _, AsyncWrite};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentBackendError;
use crate::agent::cli::stdin::{
    StdinSteering, await_optional_stdin_write, is_broken_pipe_error, spawn_optional_stdin_write,
    spawn_steering_stdin_write, write_optional_stdin, write_steering_stdin,
};
use crate::app_server::TurnSteeringInbox;

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

// --- write_steering_stdin ---

/// Encodes one steering prompt as its text followed by a newline.
const ENCODE_LINE: fn(&TurnPrompt) -> Result<Vec<u8>, AgentBackendError> =
    |prompt| Ok(format!("{}\n", prompt.text).into_bytes());

/// Rejects every steering prompt as unencodable.
fn reject_prompt(_prompt: &TurnPrompt) -> Result<Vec<u8>, AgentBackendError> {
    Err(AgentBackendError::command_build("unencodable prompt"))
}

/// Builds steering over a fresh inbox with the given encoder.
fn stdin_steering(
    encode_prompt: fn(&TurnPrompt) -> Result<Vec<u8>, AgentBackendError>,
) -> StdinSteering {
    StdinSteering {
        encode_prompt,
        inbox: Arc::new(TurnSteeringInbox::new()),
    }
}

#[tokio::test]
async fn steering_writer_delivers_prompts_until_the_turn_ends() {
    // Arrange
    let steering = stdin_steering(ENCODE_LINE);
    let turn_ended = CancellationToken::new();
    let acknowledgement = steering
        .inbox
        .submit("also add tests".into())
        .expect("open inbox");
    let (writer, mut reader) = tokio::io::duplex(1024);
    let writing = tokio::spawn({
        let turn_ended = turn_ended.clone();

        async move {
            write_steering_stdin(
                writer,
                b"initial\n".to_vec(),
                &steering,
                &turn_ended,
                make_test_error,
            )
            .await
        }
    });

    // Act
    let delivered = acknowledgement.await.expect("reply");
    turn_ended.cancel();
    let result = writing.await.expect("writer task");
    let mut written = String::new();
    reader
        .read_to_string(&mut written)
        .await
        .expect("closed stdin");

    // Assert
    assert!(delivered.is_ok());
    assert_eq!(result, Ok(()));
    assert_eq!(written, "initial\nalso add tests\n");
}

#[tokio::test]
async fn steering_writer_rejects_unencodable_and_refused_prompts() {
    // Arrange
    let unencodable = stdin_steering(reject_prompt);
    let unencodable_ack = unencodable.inbox.submit("image".into()).expect("open");
    let refused = stdin_steering(ENCODE_LINE);
    let refused_ack = refused.inbox.submit("late".into()).expect("open");
    let turn_ended = CancellationToken::new();
    let (writer, _reader) = tokio::io::duplex(1024);
    let unencodable_writing = tokio::spawn({
        let turn_ended = turn_ended.clone();

        async move {
            write_steering_stdin(
                writer,
                Vec::new(),
                &unencodable,
                &turn_ended,
                make_test_error,
            )
            .await
        }
    });
    let refusing_stdin = FailingStdin {
        shutdown_error: None,
        write_error: None,
    };
    let refusing_writer = RefuseAfterFirstWrite {
        inner: refusing_stdin,
        writes: 0,
    };

    // Act
    let unencodable_reply = unencodable_ack.await.expect("reply");
    turn_ended.cancel();
    let unencodable_result = unencodable_writing.await.expect("writer task");
    let refused_result = write_steering_stdin(
        refusing_writer,
        b"initial\n".to_vec(),
        &refused,
        &CancellationToken::new(),
        make_test_error,
    )
    .await;
    let refused_reply = refused_ack.await.expect("reply");

    // Assert
    assert_eq!(
        unencodable_reply.expect_err("unencodable").to_string(),
        "unencodable prompt"
    );
    assert_eq!(unencodable_result, Ok(()));
    assert!(refused_reply.is_err());
    assert_eq!(refused_result, Ok(()));
}

#[tokio::test]
async fn steering_writer_tolerates_closed_pipes_and_reports_other_failures() {
    // Arrange
    let cases = [
        (Some(io::ErrorKind::BrokenPipe), None, None),
        (
            Some(io::ErrorKind::PermissionDenied),
            None,
            Some("Failed to write stdin payload: permission denied"),
        ),
        (None, Some(io::ErrorKind::BrokenPipe), None),
        (
            None,
            Some(io::ErrorKind::PermissionDenied),
            Some("Failed to close stdin payload: permission denied"),
        ),
    ];
    let turn_ended = CancellationToken::new();
    turn_ended.cancel();

    for (write_error, shutdown_error, expected_error) in cases {
        let child_stdin = FailingStdin {
            shutdown_error,
            write_error,
        };

        // Act
        let result = write_steering_stdin(
            child_stdin,
            b"prompt".to_vec(),
            &stdin_steering(ENCODE_LINE),
            &turn_ended,
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

#[tokio::test]
async fn spawned_steering_writer_closes_the_inbox_when_stdin_is_unavailable() {
    // Arrange
    let steering = stdin_steering(ENCODE_LINE);
    let inbox = Arc::clone(&steering.inbox);

    // Act
    let result = spawn_steering_stdin_write::<TestError>(
        None,
        b"prompt".to_vec(),
        steering,
        CancellationToken::new(),
        "stdin unavailable",
        make_test_error,
    )
    .await
    .expect("writer task");

    // Assert
    assert_eq!(result, Err(TestError("stdin unavailable".to_string())));
    assert!(inbox.submit("late".into()).is_err());
}

/// Stdin stand-in that accepts the first write and refuses later ones.
struct RefuseAfterFirstWrite {
    inner: FailingStdin,
    writes: usize,
}

impl AsyncWrite for RefuseAfterFirstWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.writes += 1;
        if self.writes > 1 {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }

        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}
