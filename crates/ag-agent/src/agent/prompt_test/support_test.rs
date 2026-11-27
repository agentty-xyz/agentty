//! Workspace root, turn-request, and prompt normalization helpers shared by
//! prompt tests.

use std::path::{Path, PathBuf};

use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, ContextCompaction, ExecutionPolicy, HostCacheSharing,
    PermissionMode, ReasoningLevel, ResponseStyle, SpeedMode, TaskMilestone, TurnContinuation,
    TurnRequest,
};
use ag_protocol::TurnPrompt;

/// Returns the workspace root used by prompt preparation tests.
pub(super) fn test_workspace_root() -> &'static Path {
    Path::new("/tmp/agentty-wt/session-1")
}

/// Collapses rendered prompt whitespace for semantic assertions.
pub(super) fn normalize_prompt(prompt: &str) -> String {
    prompt.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Returns a session-start request for `prompt` in `response_style`, whose
/// session's task belongs to `task_milestone`.
pub(super) fn session_request(
    prompt: TurnPrompt,
    response_style: ResponseStyle,
    task_milestone: Option<TaskMilestone>,
) -> TurnRequest {
    TurnRequest {
        agent_profile: AgentDefinitionPrompt::default(),
        context_compaction: ContextCompaction::Off,
        continuation: TurnContinuation::fresh(),
        execution_policy: ExecutionPolicy::default(),
        folder: PathBuf::from(test_workspace_root()),
        host_caches: HostCacheSharing::PerSession,
        main_checkout_root: None,
        model: "claude-sonnet-5-5".to_string(),
        permission_mode: PermissionMode::AutoEdit,
        prompt,
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style,
        speed_mode: SpeedMode::default(),
        task_milestone,
    }
}
