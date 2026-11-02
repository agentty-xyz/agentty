use std::sync::{Arc, Mutex};

use crate::{
    AgentAvailabilityProbe, AgentCliVersion, AgentKind, HarnessModelOption,
    StaticAgentAvailabilityProbe,
};

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

#[tokio::test]
async fn probes_without_harness_discovery_report_no_harness_models() {
    // Arrange
    let probe = StaticAgentAvailabilityProbe {
        available_agent_kinds: AgentKind::ALL.to_vec(),
    };

    // Act
    let models = probe.native_harness_models().await;

    // Assert
    assert_eq!(models, Vec::<HarnessModelOption>::new());
}

#[tokio::test]
async fn probes_without_harness_discovery_reject_chatgpt_sign_in_without_a_url() {
    // Arrange
    let probe = StaticAgentAvailabilityProbe {
        available_agent_kinds: AgentKind::ALL.to_vec(),
    };
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = Arc::clone(&opened);

    // Act
    let result = probe
        .sign_in_chatgpt(Box::new(move |url| {
            recorder.lock().expect("opened lock").push(url);
        }))
        .await;

    // Assert
    assert_eq!(
        result,
        Err("ChatGPT sign-in is unavailable in this launch.".to_string())
    );
    assert!(opened.lock().expect("opened lock").is_empty());
}
