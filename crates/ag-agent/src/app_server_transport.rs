//! Shared stdio JSON-RPC transport utilities for app-server protocols.
//!
//! Provides low-level helpers for NDJSON-over-stdio communication used by
//! persistent app-server backends such as Codex app-server. Each helper is
//! protocol-agnostic — it operates on raw JSON values and async stdio handles
//! without knowledge of specific method names or event shapes.

#![cfg(unix)]

use std::os::unix::process::CommandExt as _;
use std::time::Duration;

use rustix::process::{self, Pid, Signal};
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader, Lines};

use crate::app_server::AppServerError;

/// Typed error returned by shared app-server transport operations.
///
/// Covers the low-level stdio communication failures that can occur when
/// writing JSON-RPC payloads to a child process or reading responses from
/// its stdout stream.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AppServerTransportError {
    /// An IO error occurred during app-server stdio communication.
    #[error("{context}: {source}")]
    Io {
        /// Human-readable description of the operation that failed.
        context: String,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// The app-server process terminated before sending the expected response.
    #[error("App-server terminated before sending expected response")]
    ProcessTerminated,

    /// Timed out waiting for a JSON-RPC response from the app-server.
    #[error(
        "Timed out waiting for app-server response `{response_id}` after {timeout_seconds} seconds"
    )]
    Timeout {
        /// The JSON-RPC request identifier that was being awaited.
        response_id: String,
        /// Number of seconds elapsed before the timeout fired.
        timeout_seconds: u64,
    },
}

/// Default timeout for initialization handshakes and session creation.
///
/// App-server cold starts can take materially longer than a typical
/// request/response round trip while the runtime initializes tools and model
/// state, so the shared startup window stays measured in minutes rather than
/// seconds to avoid aborting healthy app-server bootstraps.
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_mins(5);

/// Default timeout for a single prompt turn.
///
/// App-server turns may legitimately run for long periods while agents plan,
/// execute tools, and compact context, so the shared turn window is aligned
/// with the long-running Codex behavior instead of the shorter bootstrap
/// timeout.
pub(crate) const TURN_TIMEOUT: Duration = Duration::from_hours(4);

/// App-server child isolated in its own Unix process group.
///
/// The process-group handle ensures abandoning a runtime terminates both the
/// direct CLI process and any tool or MCP descendants that it spawned.
pub(crate) struct AppServerRuntimeChild {
    child: tokio::process::Child,
    process_group_id: Option<Pid>,
}

impl AppServerRuntimeChild {
    /// Returns the direct app-server process id while it is running.
    pub(crate) fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Sends `signal` to every process in the isolated runtime group.
    fn signal_process_group(&self, signal: Signal) {
        if let Some(process_group_id) = self.process_group_id {
            let _ = process::kill_process_group(process_group_id, signal);
        }
    }
}

impl Drop for AppServerRuntimeChild {
    fn drop(&mut self) {
        self.signal_process_group(Signal::KILL);
    }
}

/// Writes one JSON-RPC payload as a newline-delimited line to `stdin`.
///
/// # Errors
///
/// Returns an error when the write or flush to stdin fails.
pub(crate) async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    payload: &Value,
) -> Result<(), AppServerTransportError> {
    let serialized_payload = payload.to_string();

    stdin
        .write_all(serialized_payload.as_bytes())
        .await
        .map_err(|source| AppServerTransportError::Io {
            context: "Failed writing to app-server stdin".to_string(),
            source,
        })?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|source| AppServerTransportError::Io {
            context: "Failed writing newline to app-server stdin".to_string(),
            source,
        })?;
    stdin
        .flush()
        .await
        .map_err(|source| AppServerTransportError::Io {
            context: "Failed flushing app-server stdin".to_string(),
            source,
        })
}

/// Reads stdout lines until a JSON-RPC response carrying `response_id` arrives.
///
/// Non-matching lines (notifications, other responses) are silently skipped.
/// Times out after [`STARTUP_TIMEOUT`].
///
/// # Errors
///
/// Returns an error when the read times out or the child process terminates
/// before a matching response is received.
pub(crate) async fn wait_for_response_line<R>(
    stdout_lines: &mut Lines<BufReader<R>>,
    response_id: &str,
) -> Result<String, AppServerTransportError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    wait_for_response_line_with_timeout(stdout_lines, response_id, STARTUP_TIMEOUT).await
}

/// Reads stdout lines until a matching response arrives or `response_timeout`
/// elapses.
///
/// Provider lifecycles whose documented bootstrap work includes long-running
/// initialization may select a wider deadline without weakening the default
/// startup bound for every app-server request.
pub(crate) async fn wait_for_response_line_with_timeout<R>(
    stdout_lines: &mut Lines<BufReader<R>>,
    response_id: &str,
    response_timeout: Duration,
) -> Result<String, AppServerTransportError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(response_timeout, async {
        loop {
            let stdout_line = stdout_lines
                .next_line()
                .await
                .map_err(|source| AppServerTransportError::Io {
                    context: "Failed reading app-server stdout".to_string(),
                    source,
                })?
                .ok_or(AppServerTransportError::ProcessTerminated)?;

            let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                continue;
            };
            if response_id_matches(&response_value, response_id) {
                return Ok(stdout_line);
            }
        }
    })
    .await
    .map_err(|_| AppServerTransportError::Timeout {
        response_id: response_id.to_string(),
        timeout_seconds: response_timeout.as_secs(),
    })?
}

/// Returns whether a JSON-RPC response line carries the expected `id`.
pub(crate) fn response_id_matches(response_value: &Value, response_id: &str) -> bool {
    response_value
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|line_id| line_id == response_id)
}

/// Extracts a top-level `error.message` string from a JSON-RPC error response.
pub(crate) fn extract_json_error_message(response_value: &Value) -> Option<String> {
    response_value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Gracefully shuts down a child process by closing stdin, waiting briefly,
/// then killing if the process has not exited.
pub(crate) async fn shutdown_child(child: &mut AppServerRuntimeChild) {
    // Closing stdin signals the child to exit cleanly.
    drop(child.child.stdin.take());

    if tokio::time::timeout(Duration::from_secs(1), child.child.wait())
        .await
        .is_err()
    {
        child.signal_process_group(Signal::KILL);
        // Best-effort fallback for a runtime that failed to enter its process
        // group.
        let _ = child.child.kill().await;
        // Best-effort: process may have already exited.
        let _ = child.child.wait().await;
    }

    // A cooperative parent may still have left tool processes in its group.
    // Kill any stragglers before relinquishing the group identifier.
    child.signal_process_group(Signal::KILL);
    child.process_group_id = None;
}

/// Spawns one app-server child process with piped stdin/stdout and hidden
/// stderr, returning the child plus owned stdio handles.
///
/// Runtime bootstraps require line-delimited JSON-RPC over stdin/stdout, no
/// interactive stderr stream, and `kill_on_drop(true)` so abandoned runtimes
/// do not leak.
///
/// # Errors
///
/// Returns a provider error when the command cannot be spawned or either
/// required stdio pipe is unavailable.
pub(crate) fn spawn_runtime_command(
    command: std::process::Command,
    runtime_name: &str,
) -> Result<
    (
        AppServerRuntimeChild,
        tokio::process::ChildStdin,
        tokio::process::ChildStdout,
    ),
    AppServerError,
> {
    let mut command = command;
    command.process_group(0);
    let mut command = tokio::process::Command::from(command);
    // Descendant Git reads must not compete with Agentty's index writers or
    // leave optional index locks behind when the runtime is terminated.
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    let child = command.spawn().map_err(|error| {
        AppServerError::Provider(format!("Failed to spawn `{runtime_name}`: {error}"))
    })?;
    let process_group_id = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(Pid::from_raw);
    let mut child = AppServerRuntimeChild {
        child,
        process_group_id,
    };
    let stdin =
        child.child.stdin.take().ok_or_else(|| {
            AppServerError::Provider(format!("{runtime_name} stdin is unavailable"))
        })?;
    let stdout =
        child.child.stdout.take().ok_or_else(|| {
            AppServerError::Provider(format!("{runtime_name} stdout is unavailable"))
        })?;

    Ok((child, stdin, stdout))
}

#[cfg(test)]
#[path = "app_server_transport_test.rs"]
mod tests;
