use crate::AgentKind;

/// Automatic update and version probe state for one locally runnable agent
/// CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCliVersion {
    /// Startup update plus version detection is still running in the
    /// background.
    Loading,
    /// Version detection finished, but the executable did not report a usable
    /// version.
    Unknown,
    /// Version detection finished with a parsed display value.
    Value(String),
}

/// One locally runnable agent CLI and the installed version refreshed at
/// startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliInfo {
    /// Executable name used to launch the provider CLI.
    pub executable_name: &'static str,
    /// Agent provider family backed by the executable.
    pub kind: AgentKind,
    /// Current automatic update and version probe state for this executable.
    pub version: AgentCliVersion,
}

impl AgentCliInfo {
    /// Creates one CLI availability row for a provider and optional version.
    pub fn new(kind: AgentKind, version: Option<String>) -> Self {
        Self {
            executable_name: kind.executable_name(),
            kind,
            version: version.map_or(AgentCliVersion::Unknown, AgentCliVersion::Value),
        }
    }

    /// Creates one CLI availability row whose update/version refresh is still
    /// loading.
    pub fn loading(kind: AgentKind) -> Self {
        Self {
            executable_name: kind.executable_name(),
            kind,
            version: AgentCliVersion::Loading,
        }
    }

    /// Builds unknown-version CLI rows for an existing provider availability
    /// list.
    pub fn from_kinds(agent_kinds: &[AgentKind]) -> Vec<Self> {
        agent_kinds
            .iter()
            .copied()
            .map(|agent_kind| Self::new(agent_kind, None))
            .collect()
    }

    /// Builds loading CLI rows for an existing provider availability list
    /// while the background update/version refresh is running.
    pub fn loading_from_kinds(agent_kinds: &[AgentKind]) -> Vec<Self> {
        agent_kinds.iter().copied().map(Self::loading).collect()
    }
}

/// Detects which provider CLIs are locally runnable on the current machine.
pub trait AgentAvailabilityProbe: Send + Sync {
    /// Returns the agent kinds whose backing CLI executable is available.
    fn available_agent_kinds(&self) -> Vec<AgentKind>;

    /// Returns available agent CLI executables and their refreshed versions.
    fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        AgentCliInfo::from_kinds(&self.available_agent_kinds())
    }
}

/// Availability probe that returns one caller-provided snapshot.
pub struct StaticAgentAvailabilityProbe {
    /// Agent kinds reported as available by the static probe.
    pub available_agent_kinds: Vec<AgentKind>,
}

impl AgentAvailabilityProbe for StaticAgentAvailabilityProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        self.available_agent_kinds.clone()
    }
}

#[cfg(test)]
#[path = "discovery_test.rs"]
mod tests;
