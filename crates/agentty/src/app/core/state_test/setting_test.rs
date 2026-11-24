//! `App` subagent limit and `tmux` launch configuration setting unit tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mockall::predicate::eq;

use super::super::AppClients;
use super::support::new_test_app_with_selected_session;
use crate::domain::agent::AgentKind;
use crate::domain::subagent::SubagentLimit;
use crate::infra::tmux::MockTmuxClient;

#[test]
/// Keeps a trusted Cargo cache only for a launch that names its Agentty
/// home.
fn trusted_cargo_cache_is_kept_only_when_composed() {
    // Arrange
    let clients = AppClients::new();
    let without_cache = clients.cargo_cache_client.is_none();

    // Act
    let clients = clients.with_trusted_cargo_cache(Path::new("/agentty-home"));

    // Assert
    assert!(without_cache);
    assert!(clients.cargo_cache_client.is_some());
}

#[test]
fn max_concurrent_subagents_caps_every_harness_that_enforces_a_limit() {
    // Arrange
    let clients = AppClients::new();

    // Act
    let clients = clients.with_max_concurrent_subagents(SubagentLimit::Eight);

    // Assert
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        assert_eq!(
            clients
                .runtime_config
                .execution_policy(kind)
                .max_concurrent_subagents,
            Some(SubagentLimit::Eight.limit())
        );
    }
    assert_eq!(
        clients
            .runtime_config
            .execution_policy(AgentKind::Antigravity)
            .max_concurrent_subagents,
        None
    );
}

#[tokio::test]
async fn open_session_worktree_in_tmux_with_command_skips_blank_command() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-empty-launch-configuration");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-empty-launch-configuration"),
        "",
        Arc::new(mock_tmux_client),
    )
    .await;

    // Act
    app.open_session_worktree_in_tmux_with_command(Some("   "))
        .await;

    // Assert
    // Expectations are validated by `mockall`.
}
