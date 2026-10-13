//! Cancellation-safe output events shared by Bash executors.

use std::future;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::contract::{ExecutionError, OutputStream, ProcessEvent};

/// Reads at most one chunk or EOF, retaining the other stream for the next
/// event. Each read is cancellation-safe when a lifecycle event wins the
/// executor's outer selection.
pub(super) async fn next_event(
    stdout: &mut Option<impl AsyncRead + Unpin>,
    stderr: &mut Option<impl AsyncRead + Unpin>,
    buffer: &mut [u8],
) -> Result<ProcessEvent, ExecutionError> {
    let capacity = buffer.len().min(4096);
    let mut output = [0; 4096];
    let mut errors = [0; 4096];
    let (stream, length) = tokio::select! {
        result = read(stdout, &mut output[..capacity]) => (OutputStream::Stdout, result?),
        result = read(stderr, &mut errors[..capacity]) => (OutputStream::Stderr, result?),
    };
    if length == 0 {
        match stream {
            OutputStream::Stdout => *stdout = None,
            OutputStream::Stderr => *stderr = None,
        }

        return Ok(ProcessEvent::Eof(stream));
    }
    let source = match stream {
        OutputStream::Stdout => &output,
        OutputStream::Stderr => &errors,
    };
    buffer[..length].copy_from_slice(&source[..length]);

    Ok(ProcessEvent::Output(stream, length))
}

/// Missing or closed streams stay pending so only live streams emit events.
async fn read(
    reader: &mut Option<impl AsyncRead + Unpin>,
    buffer: &mut [u8],
) -> Result<usize, ExecutionError> {
    match reader {
        Some(reader) => reader
            .read(buffer)
            .await
            .map_err(|_| ExecutionError::Process),
        None => future::pending().await,
    }
}

#[cfg(test)]
#[path = "output_test.rs"]
mod tests;
