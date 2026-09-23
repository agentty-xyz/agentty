use std::num::NonZeroUsize;

/// Worker-selected harness controls, retained across retries and repairs.
/// Defaults inherit the harness configuration. Adapters must reject explicit
/// controls they cannot enforce rather than silently ignoring them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutionPolicy {
    /// Concurrent provider-native children, excluding the parent. This is
    /// distinct from the worker's concurrent runs and orchestration sessions.
    pub max_concurrent_subagents: Option<NonZeroUsize>,
    /// Whether the harness may load its configured MCP servers.
    pub mcp: McpPolicy,
    /// Built-in tool availability and unattended approval policy.
    pub tools: ToolPolicy,
}

/// MCP configuration inheritance at harness startup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum McpPolicy {
    /// Do not load configured MCP servers.
    Disabled,
    /// Retain the harness's own MCP configuration.
    #[default]
    Inherit,
}

/// Provider-native built-in tool names. Filesystem permission modes remain
/// an independent restriction; these settings cannot relax a read-only turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ToolPolicy {
    /// Expose only these built-ins; an empty list disables all built-ins.
    /// MCP access is controlled separately by [`McpPolicy`].
    AllowOnly(Vec<String>),
    /// Preapprove these tools without restricting other tool availability.
    AutoApprove(Vec<String>),
    /// Retain the harness's own tool configuration.
    #[default]
    Inherit,
}
