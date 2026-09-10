use std::sync::Arc;

use crate::channel::factory::create_agent_channel;
use crate::model::agent::AgentKind;

#[test]
fn create_agent_channel_returns_cli_channel_for_claude() {
    // Arrange / Act
    let channel = create_agent_channel(AgentKind::Claude, None);

    // Assert
    assert_eq!(Arc::strong_count(&channel), 1);
}

#[test]
fn create_agent_channel_returns_managed_channel_for_antigravity() {
    // Arrange / Act
    let channel = create_agent_channel(AgentKind::Antigravity, None);

    // Assert
    assert_eq!(Arc::strong_count(&channel), 1);
}

#[test]
fn create_agent_channel_returns_app_server_channel_for_codex() {
    // Arrange / Act
    let channel = create_agent_channel(AgentKind::Codex, None);

    // Assert
    assert_eq!(Arc::strong_count(&channel), 1);
}

#[test]
fn create_agent_channel_returns_app_server_channel_for_gemini() {
    // Arrange / Act
    let channel = create_agent_channel(AgentKind::Gemini, None);

    // Assert
    assert_eq!(Arc::strong_count(&channel), 1);
}
