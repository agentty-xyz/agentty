//! Gemini ACP transport boundary.

use serde_json::Value;

use super::super::stdio_transport::{AppServerTransportFuture, SharedAppServerStdioTransport};
use crate::infra::app_server_transport::AppServerTransportError;

/// Boxed async result used by [`GeminiRuntimeTransport`] methods.
pub(super) type GeminiTransportFuture<'scope, T> = AppServerTransportFuture<'scope, T>;

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
    inner: SharedAppServerStdioTransport,
}

impl GeminiStdioTransport {
    /// Creates a stdio transport over the provided child pipes.
    pub(super) fn new(
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
    ) -> Self {
        Self {
            inner: SharedAppServerStdioTransport::new(
                stdin,
                stdout,
                "Gemini ACP stdin is unavailable",
                "Failed reading Gemini ACP stdout",
            ),
        }
    }

    /// Closes the runtime stdin handle so shutdown can signal EOF.
    pub(super) fn close_stdin(&mut self) {
        self.inner.close_stdin();
    }
}

impl GeminiRuntimeTransport for GeminiStdioTransport {
    fn write_json_line(
        &mut self,
        payload: Value,
    ) -> GeminiTransportFuture<'_, Result<(), AppServerTransportError>> {
        self.inner.write_json_line(payload)
    }

    fn wait_for_response_line(
        &mut self,
        response_id: String,
    ) -> GeminiTransportFuture<'_, Result<String, AppServerTransportError>> {
        self.inner.wait_for_response_line(response_id)
    }

    fn next_stdout(
        &mut self,
    ) -> GeminiTransportFuture<'_, Result<Option<String>, AppServerTransportError>> {
        self.inner.next_stdout()
    }
}
