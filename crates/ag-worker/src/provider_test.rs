use std::env;
use std::num::NonZeroUsize;
use std::process::Command;

use ag_contracts::{ExecutionPolicy, McpPolicy, ToolPolicy};
use ag_session::{AgentAvailabilityProbe, AgentCliInfo, AgentKind};

use crate::{
    RealAgentAvailabilityProbe, RuntimeConfig, cleanup_session_worktree_artifacts, setup_backend,
    uses_persistent_session,
};

#[test]
fn worker_defaults_preserve_provider_policy_and_overrides_are_isolated() {
    // Arrange
    let original = RuntimeConfig::default();
    let selected = ExecutionPolicy {
        max_concurrent_subagents: NonZeroUsize::new(5),
        mcp: McpPolicy::Inherit,
        tools: ToolPolicy::AllowOnly(vec!["Read".into()]),
    };
    // Act
    let changed = original
        .clone()
        .with_execution_policy(AgentKind::Claude, selected.clone());
    // Assert
    assert_eq!(changed.execution_policy(AgentKind::Claude), selected);
    let claude = original.execution_policy(AgentKind::Claude);
    assert_eq!(claude.max_concurrent_subagents, NonZeroUsize::new(2));
    assert_eq!(claude.mcp, McpPolicy::Disabled);
    assert_eq!(
        claude.tools,
        ToolPolicy::AutoApprove(
            [
                "Bash",
                "Edit",
                "MultiEdit",
                "Write",
                "WebSearch",
                "WebFetch",
                "EnterPlanMode",
                "ExitPlanMode"
            ]
            .into_iter()
            .map(String::from)
            .collect()
        )
    );
    let codex = original.execution_policy(AgentKind::Codex);
    assert_eq!(codex.max_concurrent_subagents, NonZeroUsize::new(2));
    assert_eq!(changed.execution_policy(AgentKind::Codex), codex);
    assert_eq!(
        original.execution_policy(AgentKind::Gemini),
        ExecutionPolicy::default()
    );
    assert_eq!(
        original.execution_policy(AgentKind::Antigravity),
        ExecutionPolicy::default()
    );
}

#[test]
fn provider_discovery_uses_the_runtime_with_an_isolated_empty_search_path() {
    // Arrange: isolate PATH in a child so discovery never updates installed
    // CLIs.
    const MARKER: &str = "AG_WORKER_EMPTY_PROVIDER_PATH";
    if env::var_os(MARKER).is_some() {
        // Act / Assert
        assert_eq!(
            RealAgentAvailabilityProbe.available_agent_kinds(),
            [] as [AgentKind; 0]
        );
        assert_eq!(
            RealAgentAvailabilityProbe.available_agent_clis(),
            [] as [AgentCliInfo; 0]
        );
        assert!(matches!(
            setup_backend(AgentKind::Antigravity, std::path::Path::new(".")),
            Err(ag_contracts::AgentError::Backend(_))
        ));
        return;
    }
    // Act
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "provider::tests::provider_discovery_uses_the_runtime_with_an_isolated_empty_search_path"])
        .env(MARKER, "1").env("PATH", "").output().expect("isolated discovery");
    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn workspace_services_preserve_provider_capabilities_and_cleanup() {
    // Arrange
    let directory = tempfile::tempdir().expect("workspace");
    for kind in AgentKind::ALL {
        // Act
        let persistent = uses_persistent_session(*kind);
        // Assert: Antigravity readiness requires a discovered executable and
        // is tested separately with an isolated empty PATH.
        if *kind != AgentKind::Antigravity {
            assert!(setup_backend(*kind, directory.path()).is_ok());
        }
        assert_eq!(persistent, *kind != AgentKind::Claude);
    }
    // Act / Assert
    assert!(cleanup_session_worktree_artifacts(directory.path()).is_ok());
}
