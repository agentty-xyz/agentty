//! Codex app-server transport boundary.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

use crate::infra::app_server_transport::{self, AppServerTransportError, write_json_line};

/// Boxed async result used by [`CodexRuntimeTransport`] methods.
pub(super) type CodexTransportFuture<'scope, T> = Pin<Box<dyn Future<Output = T> + Send + 'scope>>;

/// Async stdio transport boundary for one running Codex app-server runtime.
///
/// Production uses [`CodexStdioTransport`] backed by child process stdio,
/// while tests can inject `MockCodexRuntimeTransport` to validate higher-level
/// lifecycle and turn flows without scripted shell processes.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait CodexRuntimeTransport: Send {
    /// Writes one JSON-RPC payload to runtime stdin.
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> CodexTransportFuture<'_, Result<(), AppServerTransportError>>;

    /// Waits for one JSON-RPC response line matching `response_id`.
    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> CodexTransportFuture<'_, Result<String, AppServerTransportError>>;

    /// Reads the next raw stdout line from the runtime.
    fn next_stdout(
        &mut self,
    ) -> CodexTransportFuture<'_, Result<Option<String>, AppServerTransportError>>;
}

/// Production transport backed by Codex child-process stdio.
pub(super) struct CodexStdioTransport {
    stdin: Option<tokio::process::ChildStdin>,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl CodexStdioTransport {
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

impl CodexRuntimeTransport for CodexStdioTransport {
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> CodexTransportFuture<'_, Result<(), AppServerTransportError>> {
        Box::pin(async move {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| AppServerTransportError::Io {
                    context: "Codex app-server stdin is unavailable".to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::NotConnected, "stdin closed"),
                })?;

            write_json_line(stdin, &payload).await
        })
    }

    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> CodexTransportFuture<'_, Result<String, AppServerTransportError>> {
        Box::pin(async move {
            app_server_transport::wait_for_response_line(&mut self.stdout_lines, &response_id).await
        })
    }

    fn next_stdout(
        &mut self,
    ) -> CodexTransportFuture<'_, Result<Option<String>, AppServerTransportError>> {
        Box::pin(async move {
            self.stdout_lines
                .next_line()
                .await
                .map_err(|source| AppServerTransportError::Io {
                    context: "Failed reading Codex app-server stdout".to_string(),
                    source,
                })
        })
    }
}
