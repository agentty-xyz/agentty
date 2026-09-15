pub use ag_runtime::ReasoningLevel;
pub use ag_session::{
    AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata,
    parse_persisted_session_agent_model, resolve_agent_kind_for_model,
    resolve_agent_selection_for_model, resolve_model_for_available_agent_kinds,
    resolve_prompt_model_agent_kind, selectable_models_for_agent_kinds,
};

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
