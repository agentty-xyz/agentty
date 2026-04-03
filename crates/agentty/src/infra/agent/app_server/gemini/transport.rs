//! Gemini ACP transport boundary.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

use crate::infra::app_server_transport::{self, AppServerTransportError, write_json_line};

/// Boxed async result used by [`GeminiRuntimeTransport`] methods.
pub(super) type GeminiTransportFuture<'scope, T> = Pin<Box<dyn Future<Output = T> + Send + 'scope>>;

/// Async ACP transport boundary for one running Gemini runtime.
///
/// Production uses [`GeminiStdioTransport`] backed by child process stdio,
/// while tests can inject `MockGeminiRuntimeTransport` to validate high-level
/// protocol workflows without spawning external commands.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait GeminiRuntimeTransport: Send {
    /// Writes one JSON-RPC payload to runtime stdin.
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> GeminiTransportFuture<'_, Result<(), AppServerTransportError>>;

    /// Waits for one JSON-RPC response line matching `response_id`.
    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> GeminiTransportFuture<'_, Result<String, AppServerTransportError>>;

    /// Reads the next raw stdout line from the runtime.
    fn next_stdout(
        &mut self,
    ) -> GeminiTransportFuture<'_, Result<Option<String>, AppServerTransportError>>;
}

/// Production ACP transport backed by Gemini child process stdio streams.
pub(super) struct GeminiStdioTransport {
    stdin: Option<tokio::process::ChildStdin>,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl GeminiStdioTransport {
    /// Creates a stdio transport over the provided child pipes.
    pub(super) fn new(
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
    ) -> Self {
        Self {
            stdin: Some(stdin),
            stdout_lines: BufReader::new(stdout).lines(),
        }
    }

    /// Closes the runtime stdin handle so shutdown can signal EOF.
    pub(super) fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }
}

impl GeminiRuntimeTransport for GeminiStdioTransport {
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> GeminiTransportFuture<'_, Result<(), AppServerTransportError>> {
        Box::pin(async move {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| AppServerTransportError::Io {
                    context: "Gemini ACP stdin is unavailable".to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::NotConnected, "stdin closed"),
                })?;

            write_json_line(stdin, &payload).await
        })
    }

    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> GeminiTransportFuture<'_, Result<String, AppServerTransportError>> {
        Box::pin(async move {
            app_server_transport::wait_for_response_line(&mut self.stdout_lines, &response_id).await
        })
    }

    fn next_stdout(
        &mut self,
    ) -> GeminiTransportFuture<'_, Result<Option<String>, AppServerTransportError>> {
        Box::pin(async move {
            self.stdout_lines
                .next_line()
                .await
                .map_err(|source| AppServerTransportError::Io {
                    context: "Failed reading Gemini ACP stdout".to_string(),
                    source,
                })
        })
    }
}
