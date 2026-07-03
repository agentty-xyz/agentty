//! Codex app-server transport boundary.

use serde_json::Value;

use super::super::stdio_transport::{AppServerTransportFuture, SharedAppServerStdioTransport};
use crate::infra::app_server_transport::AppServerTransportError;

/// Boxed async result used by [`CodexRuntimeTransport`] methods.
pub(super) type CodexTransportFuture<'scope, T> = AppServerTransportFuture<'scope, T>;

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
    inner: SharedAppServerStdioTransport,
}

impl CodexStdioTransport {
    /// Creates a stdio transport over the provided child pipes.
    pub(super) fn new(
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
    ) -> Self {
        Self {
            inner: SharedAppServerStdioTransport::new(
                stdin,
                stdout,
                "Codex app-server stdin is unavailable",
                "Failed reading Codex app-server stdout",
            ),
        }
    }

    /// Closes the runtime stdin handle so shutdown can signal EOF.
    pub(super) fn close_stdin(&mut self) {
        self.inner.close_stdin();
    }
}

impl CodexRuntimeTransport for CodexStdioTransport {
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> CodexTransportFuture<'_, Result<(), AppServerTransportError>> {
        self.inner.write_json_line(payload)
    }

    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> CodexTransportFuture<'_, Result<String, AppServerTransportError>> {
        self.inner.wait_for_response_line(response_id)
    }

    fn next_stdout(
        &mut self,
    ) -> CodexTransportFuture<'_, Result<Option<String>, AppServerTransportError>> {
        self.inner.next_stdout()
    }
}
