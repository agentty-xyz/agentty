use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, OneShotClient, OneShotRequest, PermissionMode, ReasoningLevel, SpeedMode,
};
use ag_session::AgentKind;
use tokio::sync::mpsc;

use crate::agent::submission::RealOneShotClient;
use crate::agent::submission_pool::SubmissionPool;
use crate::app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    AppServerTurnResponse, MockAppServerClient,
};

fn request() -> OneShotRequest {
    OneShotRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        child_pid: None,
        folder: PathBuf::from("."),
        harness: "codex".into(),
        model: "fixture".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "review".into(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::High,
        request_kind: AgentRequestKind::FocusedReview,
        speed_mode: SpeedMode::Normal,
    }
}

#[tokio::test]
async fn pooled_submissions_reuse_runtime_identity_but_request_fresh_conversations() {
    // Arrange
    let ids = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&ids);
    let mut server = MockAppServerClient::new();
    server
        .expect_run_isolated_turn()
        .times(2)
        .returning(move |request, _| {
            assert!(request.provider_conversation_id.is_none());
            assert!(request.replay_transcript.is_none());
            assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
            captured.lock().expect("ids").push(request.session_id);
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"project_impact":[],"suggestions":[]}"#.into(),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 2,
                    pid: Some(42),
                    provider_conversation_id: Some("fresh".into()),
                })
            })
        });
    server
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async {}));
    let client = RealOneShotClient::pooled(Some(Arc::new(server)));

    // Act
    client.submit(request()).await.expect("first review");
    client.submit(request()).await.expect("next review");
    client.close().await;
    client.close().await;
    RealOneShotClient::new(None).close().await;

    // Assert
    let ids = ids.lock().expect("ids");
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
}

#[tokio::test]
async fn leases_separate_concurrent_calls_and_release_slots_on_drop() {
    // Arrange
    let pool = SubmissionPool::default();
    let mut server = MockAppServerClient::new();
    server
        .expect_shutdown_session()
        .times(3)
        .returning(|_| Box::pin(async {}));
    let server = Arc::new(server) as Arc<dyn crate::app_server::AppServerClient>;

    // Act
    let first = pool
        .acquire(AgentKind::Codex, Some(Arc::clone(&server)))
        .expect("first");
    let second = pool
        .acquire(AgentKind::Codex, Some(Arc::clone(&server)))
        .expect("second");
    let gemini = pool
        .acquire(AgentKind::Gemini, Some(Arc::clone(&server)))
        .expect("provider isolation");
    let first_id = first.0.session_id.clone();
    drop(first);
    let reused = pool
        .acquire(AgentKind::Codex, Some(server))
        .expect("returned slot");

    // Assert
    assert_eq!(reused.0.session_id, first_id);
    assert_ne!(second.0.session_id, first_id);
    assert_ne!(gemini.0.session_id, first_id);
    assert!(pool.acquire(AgentKind::Claude, None).is_none());
    drop((reused, second, gemini));
    pool.close().await;
}

#[tokio::test]
async fn pooled_failures_release_runtime_and_invalid_harnesses_fail_without_submission() {
    // Arrange
    let mut server = MockAppServerClient::new();
    server
        .expect_run_isolated_turn()
        .once()
        .returning(|_, _| Box::pin(async { Err(AppServerError::Provider("offline".into())) }));
    server
        .expect_shutdown_session()
        .times(2)
        .returning(|_| Box::pin(async {}));
    let client = RealOneShotClient::pooled(Some(Arc::new(server)));

    // Act
    let failure = client.submit(request()).await.expect_err("provider error");
    let mut invalid = request();
    invalid.harness = "missing".into();
    let invalid = client.submit(invalid).await.expect_err("invalid harness");
    client.close().await;

    // Assert
    assert!(failure.to_string().contains("offline"));
    assert!(invalid.to_string().contains("missing"));
}

struct RestartOnlyServer(MockAppServerClient);

impl AppServerClient for RestartOnlyServer {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        self.0.run_turn(request, stream)
    }

    fn shutdown_session(&self, id: String) -> AppServerFuture<()> {
        self.0.shutdown_session(id)
    }
}

#[tokio::test]
async fn custom_client_fallback_shuts_down_before_starting_an_isolated_turn() {
    // Arrange
    let mut server = MockAppServerClient::new();
    let mut sequence = mockall::Sequence::new();
    server
        .expect_shutdown_session()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async {}));
    server
        .expect_run_turn()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Err(AppServerError::Provider("fixture".into())) }));
    server
        .expect_shutdown_session()
        .times(2)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async {}));
    let client = RealOneShotClient::pooled(Some(Arc::new(RestartOnlyServer(server))));
    // Act
    let result = client.submit(request()).await;
    client.close().await;
    // Assert
    assert!(
        result
            .expect_err("fixture failure")
            .to_string()
            .contains("fixture")
    );
}

#[tokio::test]
async fn custom_client_retained_repair_uses_ordinary_turn_fallback() {
    // Arrange
    let mut server = MockAppServerClient::new();
    let mut malformed = true;
    server.expect_run_turn().times(2).returning(move |_, _| {
        let message = if std::mem::take(&mut malformed) {
            "malformed"
        } else {
            r#"{"project_impact":[],"suggestions":[]}"#
        };
        Box::pin(async move {
            Ok(AppServerTurnResponse {
                assistant_message: message.into(),
                context_reset: false,
                input_tokens: 1,
                output_tokens: 2,
                pid: None,
                provider_conversation_id: None,
            })
        })
    });
    server
        .expect_shutdown_session()
        .times(2)
        .returning(|_| Box::pin(async {}));
    let client = RealOneShotClient::pooled(Some(Arc::new(RestartOnlyServer(server))));

    // Act
    let response = client.submit(request()).await.expect("repair succeeds");
    client.close().await;

    // Assert
    assert_eq!(response.stats.input_tokens, 2);
    assert_eq!(response.stats.output_tokens, 4);
}
