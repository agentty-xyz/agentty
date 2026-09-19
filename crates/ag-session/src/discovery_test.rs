use crate::{AgentAvailabilityProbe, AgentCliVersion, AgentKind, StaticAgentAvailabilityProbe};

#[test]
fn static_discovery_preserves_provider_order_without_inventing_versions() {
    // Arrange
    let probe = StaticAgentAvailabilityProbe {
        available_agent_kinds: vec![AgentKind::Codex, AgentKind::Claude],
    };

    // Act
    let clis = probe.available_agent_clis();

    // Assert
    assert_eq!(clis.len(), 2);
    assert_eq!(clis[0].kind, AgentKind::Codex);
    assert_eq!(clis[0].executable_name, "codex");
    assert_eq!(clis[1].kind, AgentKind::Claude);
    assert_eq!(clis[1].executable_name, "claude");
    assert!(
        clis.iter()
            .all(|cli| cli.version == AgentCliVersion::Unknown)
    );
}
