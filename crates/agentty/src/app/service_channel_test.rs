use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_agent::MockAppServerClient;
use ag_runtime::{
    AgentChannel, AgentRequestKind, MockAgentChannel, PermissionMode, PersonalityPrompt,
    ReasoningLevel, ResponseStyle, SpeedMode, StartSessionRequest, TurnContinuation, TurnRequest,
};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;

use super::test_support::TestSessionChannelFactory;
use super::{MockSessionChannelFactory, RealSessionChannelFactory, SessionChannelFactory};
use crate::domain::agent::AgentKind;
use crate::domain::session::SessionId;
use crate::test_support::{new_test_app, new_test_app_with_clients, test_app_clients};

#[tokio::test]
async fn session_factory_receives_identity_and_provider_through_app_composition() {
    // Arrange
    let channel: Arc<dyn AgentChannel> = Arc::new(MockAgentChannel::new());
    let expected_channel = Arc::clone(&channel);
    let mut factory = MockSessionChannelFactory::new();
    factory
        .expect_create()
        .withf(|session_id, kind| session_id.as_str() == "session" && *kind == AgentKind::Codex)
        .once()
        .returning(move |_, _| Arc::clone(&channel));
    let clients = test_app_clients().with_session_channel_factory(Arc::new(factory));
    let (app, _directory) = new_test_app_with_clients(clients).await;

    // Act
    let actual = app
        .services
        .agent_channel(&SessionId::from("session"), AgentKind::Codex);

    // Assert
    assert!(Arc::ptr_eq(&actual, &expected_channel));
}

#[tokio::test]
async fn real_session_factory_composes_each_provider_without_starting_a_process() {
    // Arrange
    let factory = RealSessionChannelFactory::new(None);
    let session_id = SessionId::from("session");

    for kind in AgentKind::ALL {
        // Act
        let first = factory.create(&session_id, *kind);
        let second = factory.create(&session_id, *kind);
        let session = first
            .start_session(StartSessionRequest {
                folder: PathBuf::new(),
                session_id: session_id.to_string(),
            })
            .await
            .expect("inert session initialization");

        // Assert
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(session.session_id, session_id.as_str());
    }
}

#[tokio::test]
async fn real_session_factory_preserves_injected_transport() {
    // Arrange
    let mut client = MockAppServerClient::new();
    client
        .expect_shutdown_session()
        .withf(|session_id| session_id == "session")
        .once()
        .returning(|_| Box::pin(async {}));
    let factory = RealSessionChannelFactory::new(Some(Arc::new(client)));

    // Act
    let result = factory
        .create(&SessionId::from("session"), AgentKind::Codex)
        .shutdown_session("session".to_string())
        .await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn scripted_session_channel_is_consumed_only_for_its_worker() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let scripted: Arc<dyn AgentChannel> = Arc::new(MockAgentChannel::new());
    let channels = TestSessionChannelFactory::install(&mut app.services);
    channels.register("scripted", Arc::clone(&scripted));

    // Act
    let other = app
        .services
        .agent_channel(&SessionId::from("other"), AgentKind::Codex);
    let actual = app
        .services
        .agent_channel(&SessionId::from("scripted"), AgentKind::Codex);
    let replacement = app
        .services
        .agent_channel(&SessionId::from("scripted"), AgentKind::Codex);

    // Assert
    assert!(!Arc::ptr_eq(&other, &scripted));
    assert!(Arc::ptr_eq(&actual, &scripted));
    assert!(!Arc::ptr_eq(&replacement, &scripted));
    assert!(other.shutdown_session("other".to_string()).await.is_ok());
    assert!(
        replacement
            .shutdown_session("scripted".to_string())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn scripted_factory_retains_multiple_sessions_across_service_clones() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let channels = TestSessionChannelFactory::install(&mut app.services);
    let services = app.services.clone();
    let first: Arc<dyn AgentChannel> = Arc::new(MockAgentChannel::new());
    let second: Arc<dyn AgentChannel> = Arc::new(MockAgentChannel::new());

    // Act
    channels.register("first", Arc::clone(&first));
    channels.register("second", Arc::clone(&second));
    let second_worker = services.agent_channel(&SessionId::from("second"), AgentKind::Claude);
    let first_worker = app
        .services
        .agent_channel(&SessionId::from("first"), AgentKind::Codex);
    let unscripted = services.agent_channel(&SessionId::from("missing"), AgentKind::Codex);

    // Assert
    assert!(Arc::ptr_eq(&first_worker, &first));
    assert!(Arc::ptr_eq(&second_worker, &second));
    assert!(
        unscripted
            .shutdown_session("missing".to_string())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn offline_session_channel_fails_even_in_a_detached_task() {
    // Arrange
    let child_marker = "AGENTTY_TEST_OFFLINE_TURN_CHILD";
    if env::var_os(child_marker).is_some() {
        let factory = TestSessionChannelFactory::default();
        let channel = factory.create(&SessionId::from("unscripted"), AgentKind::Codex);
        let session = channel
            .start_session(StartSessionRequest {
                folder: PathBuf::new(),
                session_id: "unscripted".to_string(),
            })
            .await
            .expect("offline initialization");
        let (events, _receiver) = mpsc::unbounded_channel();
        let request = unexpected_turn_request();

        // Act: ignore the task result, as a detached worker's caller would.
        let _ = tokio::spawn(
            async move { channel.run_turn(session.session_id, request, events).await },
        )
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
    let factory = TestSessionChannelFactory::default();
    let channel = factory.create(&SessionId::from("offline"), AgentKind::Codex);
    let (events, _receiver) = mpsc::unbounded_channel();

    // Act
    let session = channel
        .start_session(StartSessionRequest {
            folder: PathBuf::new(),
            session_id: "offline".to_string(),
        })
        .await
        .expect("offline session");
    drop(channel.run_turn(
        session.session_id.clone(),
        unexpected_turn_request(),
        events,
    ));
    let shutdown = channel.shutdown_session(session.session_id.clone()).await;

    // Assert
    assert_eq!(session.session_id, "offline");
    assert!(shutdown.is_ok());
}

fn unexpected_turn_request() -> TurnRequest {
    TurnRequest {
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
