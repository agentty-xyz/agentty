//! Public contract coverage for scoped utility runtime cleanup.

use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use ag_agent::{
    AgentRequestKind, AppServerClient, AppServerError, AppServerFuture, AppServerStreamEvent,
    AppServerTurnRequest, AppServerTurnResponse, OneShotClient, OneShotRequest, PermissionMode,
    RealOneShotClient, ReasoningLevel, SpeedMode,
};
use tokio::sync::{Notify, mpsc};

#[derive(Default)]
struct RestartOnlyServer {
    events: Arc<Mutex<Vec<String>>>,
    malformed_initial: bool,
    shutdown_release: Option<Arc<Notify>>,
    shutdown_started: Arc<Notify>,
}

impl AppServerClient for RestartOnlyServer {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        _stream: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let malformed = {
            let mut events = self
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            events.push(format!("turn:{}", request.session_id));
            self.malformed_initial && events.len() == 2
        };
        Box::pin(async move {
            assert!(request.provider_conversation_id.is_none());
            Ok(AppServerTurnResponse {
                assistant_message: if malformed {
                    "malformed"
                } else {
                    r#"{"project_impact":[],"suggestions":[]}"#
                }
                .into(),
                context_reset: false,
                input_tokens: 0,
                output_tokens: 0,
                pid: None,
                provider_conversation_id: None,
            })
        })
    }

    fn shutdown_session(&self, id: String) -> AppServerFuture<()> {
        let events = Arc::clone(&self.events);
        let shutdown_release = self.shutdown_release.clone();
        let shutdown_started = Arc::clone(&self.shutdown_started);
        Box::pin(async move {
            if let Some(shutdown_release) = shutdown_release {
                events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("closing:{id}"));
                shutdown_started.notify_one();
                shutdown_release.notified().await;
            }
            events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("close:{id}"));
        })
    }
}

#[tokio::test]
async fn pooling_preserves_isolation_for_custom_clients_and_closes_the_scope() {
    // Arrange
    let server = Arc::new(RestartOnlyServer::default());
    let events = Arc::clone(&server.events);
    let client = RealOneShotClient::pooled(Some(server));
    let request = review_request();
    // Act
    client.submit(request.clone()).await.expect("first");
    client.submit(request).await.expect("second");
    client.close().await;
    // Assert
    let events = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(events.len(), 5);
    assert!(events[0].starts_with("close:"));
    assert!(events[1].starts_with("turn:"));
    assert_eq!(events[0], events[2]);
    assert_eq!(events[1], events[3]);
    assert_eq!(events[0], events[4]);
}

#[tokio::test]
async fn isolated_turn_waits_for_shutdown_before_invoking_custom_client() {
    // Arrange
    let shutdown_release = Arc::new(Notify::new());
    let server = Arc::new(RestartOnlyServer {
        events: Arc::default(),
        shutdown_release: Some(Arc::clone(&shutdown_release)),
        malformed_initial: false,
        shutdown_started: Arc::default(),
    });
    let events = Arc::clone(&server.events);
    let shutdown_started = Arc::clone(&server.shutdown_started);
    let client = RealOneShotClient::pooled(Some(server));
    let mut submission = Box::pin(client.submit(review_request()));

    // Act
    let pending = poll_fn(|context| Poll::Ready(submission.as_mut().poll(context))).await;
    tokio::time::timeout(Duration::from_secs(5), shutdown_started.notified())
        .await
        .expect("shutdown starts in the owned submission task");

    // Assert
    assert!(pending.is_pending());
    {
        let events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(events.len(), 1);
        assert!(events[0].starts_with("closing:"));
    }
    shutdown_release.notify_one();
    submission.await.expect("submission after shutdown");
    {
        let events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(events.len(), 3);
        assert!(events[1].starts_with("close:"));
        assert!(events[2].starts_with("turn:"));
    }
    shutdown_release.notify_one();
    client.close().await;
}

#[tokio::test]
async fn pooled_repair_falls_back_to_custom_clients_without_an_intermediate_shutdown() {
    // Arrange
    let server = Arc::new(RestartOnlyServer {
        malformed_initial: true,
        ..Default::default()
    });
    let events = Arc::clone(&server.events);
    let client = RealOneShotClient::pooled(Some(server));

    // Act
    client
        .submit(review_request())
        .await
        .expect("repair succeeds");
    client.close().await;

    // Assert
    let events = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(events.len(), 4);
    assert!(events[0].starts_with("close:"));
    assert!(events[1].starts_with("turn:"));
    assert_eq!(events[1], events[2]);
    assert_eq!(events[0], events[3]);
}

fn review_request() -> OneShotRequest {
    OneShotRequest {
        child_pid: None,
        folder: ".".into(),
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
