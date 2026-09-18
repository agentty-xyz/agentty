use std::path::Path;

use ag_contracts::AgentError;
use ag_session::{AgentAvailabilityProbe, AgentCliInfo, AgentKind};

/// Machine-scoped provider discovery through the concrete adapter registry.
pub struct RealAgentAvailabilityProbe;

impl AgentAvailabilityProbe for RealAgentAvailabilityProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        ag_agent::RealAgentAvailabilityProbe.available_agent_kinds()
    }

    fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        ag_agent::RealAgentAvailabilityProbe.available_agent_clis()
    }
}

/// Initializes provider resources in a session workspace.
///
/// # Errors
/// Returns a normalized adapter setup failure.
pub fn setup_backend(kind: AgentKind, folder: &Path) -> Result<(), AgentError> {
    ag_agent::create_backend(kind)
        .setup(folder)
        .map_err(Into::into)
}

/// Whether the selected provider retains a process between turns.
pub fn uses_persistent_session(kind: AgentKind) -> bool {
    ag_agent::transport_mode(kind).uses_app_server()
}

/// Reclaims adapter-owned resources during workspace deletion.
///
/// # Errors
/// Returns the runtime artifact cleanup failure.
pub fn cleanup_session_worktree_artifacts(folder: &Path) -> Result<(), AgentError> {
    ag_agent::cleanup_session_worktree_artifacts(folder).map_err(Into::into)
}
