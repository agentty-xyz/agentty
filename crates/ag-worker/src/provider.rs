use std::path::Path;

use ag_contracts::AgentError;
use ag_session::{AgentAvailabilityProbe, AgentCliInfo, AgentKind};

/// Provider configuration used by the worker to compose its runtimes.
#[derive(Clone, Default)]
pub struct RuntimeConfig {
    pub(crate) factory: ag_runtime::RuntimeFactory,
}

/// Discovery boundary for applications; provider details stay in the runtime.
pub struct RealAgentAvailabilityProbe;

impl AgentAvailabilityProbe for RealAgentAvailabilityProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        ag_runtime::RealAgentAvailabilityProbe.available_agent_kinds()
    }

    fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        ag_runtime::RealAgentAvailabilityProbe.available_agent_clis()
    }
}

/// Prepares a provider's workspace before work is admitted.
///
/// # Errors
/// Returns the runtime's normalized setup error.
pub fn setup_backend(kind: AgentKind, folder: &Path) -> Result<(), AgentError> {
    ag_runtime::setup_backend(kind, folder)
}

/// Whether resource accounting follows a retained provider process.
pub fn uses_persistent_session(kind: AgentKind) -> bool {
    ag_runtime::uses_persistent_session(kind)
}

/// Reclaims runtime artifacts while deleting a settled session workspace.
///
/// # Errors
/// Returns the runtime artifact cleanup failure.
pub fn cleanup_session_worktree_artifacts(folder: &Path) -> Result<(), AgentError> {
    ag_runtime::cleanup_session_worktree_artifacts(folder)
}

#[cfg(test)]
#[path = "provider_test.rs"]
mod tests;
