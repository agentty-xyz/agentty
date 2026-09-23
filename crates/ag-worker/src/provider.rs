use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;

use ag_contracts::{AgentError, ExecutionPolicy, McpPolicy, ToolPolicy};
use ag_session::{AgentAvailabilityProbe, AgentCliInfo, AgentKind};

/// Provider configuration used by the worker to compose its runtimes.
#[derive(Clone)]
pub struct RuntimeConfig {
    pub(crate) factory: ag_runtime::RuntimeFactory,
    pub(crate) policies: BTreeMap<String, ExecutionPolicy>,
}

impl RuntimeConfig {
    /// Replaces one harness's execution policy for subsequently constructed
    /// session and utility workers. Existing workers retain their snapshot.
    #[must_use]
    pub fn with_execution_policy(mut self, kind: AgentKind, policy: ExecutionPolicy) -> Self {
        self.policies.insert(kind.to_string(), policy);

        self
    }

    /// Returns the configured controls for one harness.
    pub fn execution_policy(&self, kind: AgentKind) -> ExecutionPolicy {
        self.policies
            .get(&kind.to_string())
            .cloned()
            .unwrap_or_default()
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let subagents = NonZeroUsize::new(2);
        Self {
            factory: ag_runtime::RuntimeFactory::default(),
            policies: BTreeMap::from([
                (
                    AgentKind::Codex.to_string(),
                    ExecutionPolicy {
                        max_concurrent_subagents: subagents,
                        ..ExecutionPolicy::default()
                    },
                ),
                (
                    AgentKind::Claude.to_string(),
                    ExecutionPolicy {
                        max_concurrent_subagents: subagents,
                        mcp: McpPolicy::Disabled,
                        tools: ToolPolicy::AutoApprove(
                            [
                                "Bash",
                                "Edit",
                                "MultiEdit",
                                "Write",
                                "WebSearch",
                                "WebFetch",
                                "EnterPlanMode",
                                "ExitPlanMode",
                            ]
                            .into_iter()
                            .map(String::from)
                            .collect(),
                        ),
                    },
                ),
            ]),
        }
    }
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
