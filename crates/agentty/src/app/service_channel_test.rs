use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{
    AgentRequestKind, MockAgentChannel, PermissionMode, PersonalityPrompt, ReasoningLevel,
    ResponseStyle, SpeedMode, TurnContinuation, TurnRequest,
};
use ag_worker::test_support::MockAppServerClient;
use ag_worker::{RuntimeConfig, SessionRunClient};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use super::test_support::TestSessionRunFactory;
use super::{MockSessionRunFactory, RealSessionRunFactory, SessionRunFactory};
use crate::domain::agent::AgentKind;
use crate::domain::session::SessionId;
use crate::test_support::{new_test_app, new_test_app_with_clients, test_app_clients};

#[tokio::test]
async fn session_factory_receives_identity_and_provider_through_app_composition() {
    // Arrange
    let mut factory = MockSessionRunFactory::new();
    factory
        .expect_create()
        .withf(|id, kind| id.as_str() == "session" && *kind == AgentKind::Codex)
        .once()
        .returning(|id, _| {
            let mut channel = MockAgentChannel::new();
            channel
                .expect_shutdown_session()
                .withf(|id| id == "session")
                .once()
                .returning(|_| Box::pin(async { Ok(()) }));
            SessionRunClient::from_channel(id.to_string(), Arc::new(channel))
        });
    let clients = test_app_clients().with_session_run_factory(Arc::new(factory));
    let (app, _directory) = new_test_app_with_clients(clients).await;

    // Act
    let worker = app
        .services
        .session_run(&SessionId::from("session"), AgentKind::Codex);

    // Assert
    assert!(worker.shutdown().await.is_ok());
}

#[tokio::test]
async fn real_session_factory_composes_each_provider_without_starting_a_process() {
    // Arrange
    let factory = RealSessionRunFactory::new(RuntimeConfig::default());
    for kind in AgentKind::ALL {
        // Act
        let worker = factory.create(&SessionId::from("session"), *kind);
        // Assert
        assert!(worker.shutdown().await.is_ok());
    }
}

#[tokio::test]
async fn real_session_factory_preserves_injected_transport() {
    // Arrange
    let mut client = MockAppServerClient::new();
    client
        .expect_shutdown_session()
        .withf(|id| id == "session")
        .once()
        .returning(|_| Box::pin(async {}));
    let factory = RealSessionRunFactory::new(RuntimeConfig::with_app_server(Arc::new(client)));
    // Act
    let result = factory
        .create(&SessionId::from("session"), AgentKind::Codex)
        .shutdown()
        .await;
    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn scripted_factory_retains_sessions_across_clones_and_consumes_each_script_once() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let channels = TestSessionRunFactory::install(&mut app.services);
    let services = app.services.clone();
    for id in ["first", "second"] {
        let mut channel = MockAgentChannel::new();
        channel
            .expect_shutdown_session()
            .withf(move |actual| actual == id)
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
        channels.register(id, Arc::new(channel));
    }
    // Act / Assert: both service clones share the registry and consuming one
    // script leaves the other intact. Later workers use the offline fallback.
    for id in ["second", "first", "missing", "first"] {
        let service = if id == "second" {
            &services
        } else {
            &app.services
        };
        assert!(
            service
                .session_run(&SessionId::from(id), AgentKind::Codex)
                .shutdown()
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn offline_session_channel_fails_even_in_a_detached_task() {
    // Arrange
    let child_marker = "AGENTTY_TEST_OFFLINE_TURN_CHILD";
    if env::var_os(child_marker).is_some() {
        let factory = TestSessionRunFactory::default();
        let channel = factory.create(&SessionId::from("unscripted"), AgentKind::Codex);
        let (events, _receiver) = mpsc::unbounded_channel();
        let request = unexpected_turn_request();

        // Act: ignore the task result, as a detached worker's caller would.
        let _ = tokio::spawn(async move {
            channel
                .submit(request, events, CancellationToken::new())
                .await
        })
        .await;

        return;
    }

    // Act
    let output = time::timeout(
        Duration::from_secs(10),
        Command::new(env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "app::service::channel_tests::offline_session_channel_fails_even_in_a_detached_task",
                "--nocapture",
            ])
            .env(child_marker, "1")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("offline child exited")
    .expect("child process");

    // Assert
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("unexpected model execution for session unscripted")
    );
}

#[tokio::test]
async fn offline_session_channel_allows_shutdown_without_polling_a_turn() {
    // Arrange
    let factory = TestSessionRunFactory::default();
    let channel = factory.create(&SessionId::from("offline"), AgentKind::Codex);
    let (events, _receiver) = mpsc::unbounded_channel();

    // Act
    drop(channel.submit(unexpected_turn_request(), events, CancellationToken::new()));
    let shutdown = channel.shutdown().await;

    // Assert
    assert!(shutdown.is_ok());
}

fn unexpected_turn_request() -> TurnRequest {
    TurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        continuation: TurnContinuation::fresh(),
        folder: PathBuf::new(),
        main_checkout_root: None,
        model: "unused".to_string(),
        permission_mode: PermissionMode::AutoEdit,
        personality: PersonalityPrompt::default(),
        prompt: "unexpected turn".into(),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: ResponseStyle::default(),
        speed_mode: SpeedMode::default(),
    }
}
