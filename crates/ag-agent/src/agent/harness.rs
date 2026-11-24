//! Fail-closed CLI-path hooks for [`ag_session::AgentKind::Harness`].
//!
//! The harness runs in process through the native transport and never
//! produces subprocess output. When a host has not configured that transport,
//! harness work falls back to these hooks and fails before any process or
//! model call starts.

use std::path::Path;
use std::process::Command;

use ag_contracts::ModelUsage;

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::response_parser::ParsedResponse;
use super::usage::Billing;

/// Error reported for harness work when the native transport is not
/// configured.
pub(super) const HARNESS_UNAVAILABLE_MESSAGE: &str =
    "Harness sessions need the in-process harness transport, which this host has not configured.";

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
        billing: Billing::Metered,
        content: String::new(),
        follow_up_contents: Vec::new(),
        totals: None,
        usage: ModelUsage::new(""),
    }
}

/// Returns no stream content because the harness writes no CLI output.
pub(super) fn parse_stream_payload(_payload: &serde_json::Value) -> Option<(String, bool)> {
    None
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;
