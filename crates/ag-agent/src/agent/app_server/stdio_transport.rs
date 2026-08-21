//! Shared primitives for stdio-backed app-server transports.
//!
//! Both Codex (`codex app-server`) and Gemini (`gemini --acp`) use the same
//! wire protocol framing and stdio loop logic. This module centralizes the
//! transport lifetime helpers behind one mockable boundary.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

use crate::app_server_transport::{self, AppServerTransportError, write_json_line};

/// Shared boxed future alias for async stdio transport operations.
pub(super) type AppServerTransportFuture<'scope, T> =
    Pin<Box<dyn Future<Output = T> + Send + 'scope>>;

/// Async stdio transport boundary for one running app-server runtime.
///
/// Production uses [`AppServerStdioTransport`] backed by child process stdio,
/// while tests can inject `MockAppServerRuntimeTransport` to validate
/// higher-level lifecycle and turn flows without scripted shell processes.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub(crate) trait AppServerRuntimeTransport: Send {
    /// Writes one JSON-RPC payload to runtime stdin.
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> AppServerTransportFuture<'_, Result<(), AppServerTransportError>>;

    /// Waits for one JSON-RPC response line matching `response_id`.
    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> AppServerTransportFuture<'_, Result<String, AppServerTransportError>>;

    /// Waits for one matching response using a provider-selected timeout.
    fn wait_for_response_line_with_timeout(
        &mut self,
        response_id: String,
        response_timeout: Duration,
    ) -> AppServerTransportFuture<'_, Result<String, AppServerTransportError>>;

    /// Reads the next raw stdout line from the runtime.
    fn next_stdout(
        &mut self,
    ) -> AppServerTransportFuture<'_, Result<Option<String>, AppServerTransportError>>;
}

/// Shared stdio transport used by app-server runtimes.
pub(super) struct AppServerStdioTransport {
    read_stdout_context: &'static str,
    stdin: Option<tokio::process::ChildStdin>,
    stdin_unavailable_context: &'static str,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl AppServerStdioTransport {
    /// Creates a shared transport over the child process stdin/stdout streams.
    pub(super) fn new(
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
        stdin_unavailable_context: &'static str,
        read_stdout_context: &'static str,
    ) -> Self {
        Self {
            read_stdout_context,
            stdin: Some(stdin),
            stdin_unavailable_context,
            stdout_lines: BufReader::new(stdout).lines(),
        }
    }

    /// Closes the stdin handle so runtime shutdown can signal EOF.
    pub(super) fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }
}

impl AppServerRuntimeTransport for AppServerStdioTransport {
    /// Writes one JSON-RPC payload to runtime stdin.
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> AppServerTransportFuture<'_, Result<(), AppServerTransportError>> {
        let stdin_unavailable_context = self.stdin_unavailable_context;

        Box::pin(async move {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| AppServerTransportError::Io {
                    context: stdin_unavailable_context.to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::NotConnected, "stdin closed"),
                })?;

            write_json_line(stdin, &payload).await
        })
    }

    /// Waits for one matching response line for a request id.
    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> AppServerTransportFuture<'_, Result<String, AppServerTransportError>> {
        Box::pin(async move {
            app_server_transport::wait_for_response_line(&mut self.stdout_lines, &response_id).await
        })
    }

    /// Waits for one matching response line using an explicit deadline.
    fn wait_for_response_line_with_timeout(
        &mut self,
        response_id: String,
        response_timeout: Duration,
    ) -> AppServerTransportFuture<'_, Result<String, AppServerTransportError>> {
        Box::pin(async move {
            app_server_transport::wait_for_response_line_with_timeout(
                &mut self.stdout_lines,
                &response_id,
                response_timeout,
            )
            .await
        })
    }

    /// Reads one raw JSON line from runtime stdout.
    fn next_stdout(
        &mut self,
    ) -> AppServerTransportFuture<'_, Result<Option<String>, AppServerTransportError>> {
        let read_stdout_context = self.read_stdout_context;

        Box::pin(async move {
            self.stdout_lines
                .next_line()
                .await
                .map_err(|source| AppServerTransportError::Io {
                    context: read_stdout_context.to_string(),
                    source,
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_response_timeout_round_trips_matching_line() {
        // Arrange
        let command = std::process::Command::new("cat");
        let (mut child, stdin, stdout) =
            app_server_transport::spawn_runtime_command(command, "cat")
                .expect("`cat` transport fixture should spawn");
        let mut transport = AppServerStdioTransport::new(
            stdin,
            stdout,
            "test stdin is unavailable",
            "failed reading test stdout",
        );
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "response-1",
            "result": {},
        });
        transport
            .write_json_line(payload.clone())
            .await
            .expect("fixture request should be written");

        // Act
        let response_line = transport
            .wait_for_response_line_with_timeout("response-1".to_string(), Duration::from_secs(1))
            .await;

        // Assert
        assert_eq!(
            response_line.expect("matching response should arrive"),
            payload.to_string()
        );
        transport.close_stdin();
        app_server_transport::shutdown_child(&mut child).await;
    }
}
