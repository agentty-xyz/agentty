//! Fail-closed CLI-path hooks for [`ag_session::AgentKind::Harness`].
//!
//! The harness runs in process through the native transport and never
//! produces subprocess output. When a host has not configured that transport,
//! harness work falls back to these hooks and fails before any process or
//! model call starts.

use std::path::Path;
use std::process::Command;

use ag_contracts::SessionStats;

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::response_parser::ParsedResponse;

/// Error reported for harness work when the native transport is not
/// configured.
pub(super) const HARNESS_UNAVAILABLE_MESSAGE: &str =
    "Harness sessions run only when Agentty starts with `--experimental-harness`.";

/// Backend that rejects every harness command before execution.
pub(super) struct HarnessBackend;

impl AgentBackend for HarnessBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        _request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        Err(AgentBackendError::CommandBuild(
            HARNESS_UNAVAILABLE_MESSAGE.to_string(),
        ))
    }
}

/// Returns an empty response because the harness writes no CLI output.
pub(super) fn parse_response(_stdout: &str, _stderr: &str) -> ParsedResponse {
    ParsedResponse {
        content: String::new(),
        stats: SessionStats::default(),
    }
}

/// Returns no stream content because the harness writes no CLI output.
pub(super) fn parse_stream_output_line(_stdout_line: &str) -> Option<(String, bool)> {
    None
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;
