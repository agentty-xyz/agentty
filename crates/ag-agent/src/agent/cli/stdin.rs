//! Shared async stdin delivery helpers for spawned agent CLI subprocesses.

use std::io;
use std::sync::Arc;

use ag_protocol::TurnPrompt;
use tokio::io::{AsyncWrite, AsyncWriteExt as _};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentBackendError;
use crate::app_server::AppServerError;
use crate::app_server::steering::{self, TurnSteeringInbox};

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

/// Starts one background stdin writer that keeps the pipe open for steering.
///
/// The writer sends `stdin_payload`, then writes each steering prompt from
/// `steering` until `turn_ended` fires, and finally closes the inbox and
/// stdin so the provider exits after answering everything it read.
pub(crate) fn spawn_steering_stdin_write<Error>(
    child_stdin: Option<tokio::process::ChildStdin>,
    stdin_payload: Vec<u8>,
    steering: StdinSteering,
    turn_ended: CancellationToken,
    unavailable_message: &'static str,
    format_error: fn(String) -> Error,
) -> JoinHandle<Result<(), Error>>
where
    Error: Send + 'static,
{
    tokio::spawn(async move {
        let result = match child_stdin {
            Some(child_stdin) => {
                write_steering_stdin(
                    child_stdin,
                    stdin_payload,
                    &steering,
                    &turn_ended,
                    format_error,
                )
                .await
            }
            None => Err(format_error(unavailable_message.to_string())),
        };
        steering.inbox.close();

        result
    })
}

/// Steering prompts written into one running provider turn through stdin.
pub(crate) struct StdinSteering {
    /// Renders one steering prompt as one provider stdin message.
    pub(crate) encode_prompt: fn(&TurnPrompt) -> Result<Vec<u8>, AgentBackendError>,
    /// Inbox drained while the provider turn runs.
    pub(crate) inbox: Arc<TurnSteeringInbox>,
}

/// Writes the initial prompt and steering prompts until the turn ends.
///
/// Generic over the writer so delivery and failure handling can be exercised
/// without a subprocess. A steering prompt the pipe refuses is rejected back
/// to its caller and ends steering without failing the turn.
///
/// # Errors
/// Returns an error when the initial prompt or the final close fails for a
/// reason other than an already-closed pipe.
async fn write_steering_stdin<Writer, Error>(
    mut child_stdin: Writer,
    stdin_payload: Vec<u8>,
    steering: &StdinSteering,
    turn_ended: &CancellationToken,
    format_error: fn(String) -> Error,
) -> Result<(), Error>
where
    Writer: AsyncWrite + Unpin,
    Error: Send + 'static,
{
    if let Err(error) = child_stdin.write_all(&stdin_payload).await {
        if is_broken_pipe_error(&error) {
            return Ok(());
        }

        return Err(format_error(format!(
            "Failed to write stdin payload: {error}"
        )));
    }

    loop {
        let steer = tokio::select! {
            biased;
            () = turn_ended.cancelled() => break,
            steer = steering.inbox.next() => steer,
        };
        let message = match (steering.encode_prompt)(&steer.prompt) {
            Ok(message) => message,
            Err(error) => {
                steer.acknowledge(Err(AppServerError::Provider(error.to_string())));

                continue;
            }
        };
        if child_stdin.write_all(&message).await.is_err() {
            steer.acknowledge(Err(steering::turn_not_steerable_error()));

            break;
        }
        steer.acknowledge(Ok(()));
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
/// Generic over the writer so the closed-pipe tolerance and error mapping can
/// be exercised without a subprocess.
///
/// # Errors
/// Returns an error when stdin was requested but not available or the write
/// fails before EOF is signaled.
async fn write_optional_stdin<Writer, Error>(
    child_stdin: Option<Writer>,
    stdin_payload: Vec<u8>,
    unavailable_message: &'static str,
    format_error: fn(String) -> Error,
) -> Result<(), Error>
where
    Writer: AsyncWrite + Unpin,
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

#[cfg(test)]
#[path = "stdin_test.rs"]
mod tests;
