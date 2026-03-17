//! Shared async stdin delivery helpers for spawned agent CLI subprocesses.

use std::io;

use tokio::io::AsyncWriteExt as _;
use tokio::task::JoinHandle;

/// Starts one background stdin writer when the child needs prompt input.
pub(crate) fn spawn_optional_stdin_write<Error>(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Option<Vec<u8>>,
    unavailable_message: &'static str,
    format_error: fn(String) -> Error,
) -> Option<JoinHandle<Result<(), Error>>>
where
    Error: Send + 'static,
{
    stdin_payload.map(|stdin_payload| {
        tokio::spawn(async move {
            write_optional_stdin(
                child_stdin,
                stdin_payload,
                unavailable_message,
                format_error,
            )
            .await
        })
    })
}

/// Waits for one optional background stdin writer to finish.
///
/// # Errors
/// Returns an error when the writer task fails or panics before the full
/// payload is sent.
pub(crate) async fn await_optional_stdin_write<Error>(
    stdin_write_task: Option<JoinHandle<Result<(), Error>>>,
    join_error_prefix: &'static str,
    format_error: fn(String) -> Error,
) -> Result<(), Error>
where
    Error: Send + 'static,
{
    let Some(stdin_write_task) = stdin_write_task else {
        return Ok(());
    };

    stdin_write_task
        .await
        .map_err(|error| format_error(format!("{join_error_prefix}: {error}")))?
}

/// Writes one optional stdin payload into the spawned CLI subprocess.
///
/// # Errors
/// Returns an error when stdin was requested but not available or the write
/// fails before EOF is signaled.
async fn write_optional_stdin<Error>(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Vec<u8>,
    unavailable_message: &'static str,
    format_error: fn(String) -> Error,
) -> Result<(), Error>
where
    Error: Send + 'static,
{
    let mut child_stdin =
        child_stdin.ok_or_else(|| format_error(unavailable_message.to_string()))?;
    if let Err(error) = child_stdin.write_all(&stdin_payload).await
        && !is_broken_pipe_error(&error)
    {
        return Err(format_error(format!(
            "Failed to write stdin payload: {error}"
        )));
    }
    if let Err(error) = child_stdin.shutdown().await
        && !is_broken_pipe_error(&error)
    {
        return Err(format_error(format!(
            "Failed to close stdin payload: {error}"
        )));
    }

    Ok(())
}

/// Returns whether one stdin write error is the expected closed-pipe case.
fn is_broken_pipe_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::BrokenPipe
}
