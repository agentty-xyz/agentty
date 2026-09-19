use std::env;
use std::process::Command;

use ag_session::{AgentAvailabilityProbe, AgentCliInfo, AgentKind};

use crate::{
    RealAgentAvailabilityProbe, cleanup_session_worktree_artifacts, setup_backend,
    uses_persistent_session,
};

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
