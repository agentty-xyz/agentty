use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ag_contracts::{
    AgentError, AgentRequestKind, MockAgentChannel, PermissionMode, PersonalityPrompt,
    ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnRequest, TurnResult,
};
use ag_protocol::TurnPrompt;
use ag_telemetry::{KeyValue, Span, TraceContextExt as _};
use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::test_support::{AppServerTurnResponse, MockAppServerClient};
use crate::{RuntimeConfig, SessionRunClient};

#[tokio::test]
async fn configured_session_policy_replaces_request_policy_and_survives_repair() {
    // Arrange
    let policy = ag_contracts::ExecutionPolicy {
        max_concurrent_subagents: std::num::NonZeroUsize::new(4),
        ..ag_contracts::ExecutionPolicy::default()
    };
    let expected = policy.clone();
    let mut server = MockAppServerClient::new();
    let mut attempts = 0;
    server
        .expect_run_turn()
        .times(2)
        .returning(move |request, _| {
            assert_eq!(request.execution_policy, expected);
            attempts += 1;
            let output = if attempts == 1 {
                "invalid output"
            } else {
                r#"{"answer":"done","questions":[]}"#
            };
            Box::pin(async move {
                Ok(AppServerTurnResponse {
                    assistant_message: output.into(),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 1,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    server
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async {}));
    let config = RuntimeConfig::with_app_server(Arc::new(server))
        .with_execution_policy(ag_session::AgentKind::Codex, policy);
    let worker = SessionRunClient::new(
        "policy-session".into(),
        ag_session::AgentKind::Codex,
        &config,
    );
    // Act
    let result = worker
        .submit(
            request(),
            mpsc::unbounded_channel().0,
            CancellationToken::new(),
        )
        .await;
    worker.shutdown().await.expect("shutdown");
    // Assert
    assert_eq!(
        result.expect("repaired turn").assistant_message.answer,
        "done"
    );
}

fn request() -> TurnRequest {
    TurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        continuation: TurnContinuation::fresh(),
        folder: ".".into(),
        main_checkout_root: None,
        model: "test-model".into(),
        permission_mode: PermissionMode::default(),
        personality: PersonalityPrompt::default(),
        prompt: TurnPrompt::from("test"),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: ResponseStyle::default(),
        speed_mode: SpeedMode::default(),
    }
}

#[tokio::test]
async fn returns_runtime_output_and_errors() {
    // Arrange
    let mut channel = MockAgentChannel::new();
    channel.expect_run_turn().times(1).returning(|_, _, _| {
        Box::pin(async {
            Ok(TurnResult {
                assistant_message: ag_protocol::AgentResponse::plain("done"),
                context_reset: false,
                input_tokens: 3,
                output_tokens: 5,
                provider_conversation_id: None,
            })
        })
    });
    // Act
    let result = SessionRunClient::from_channel("session".into(), Arc::new(channel))
        .submit(
            request(),
            mpsc::unbounded_channel().0,
            CancellationToken::new(),
        )
        .await
        .expect("test operation succeeds");
    // Assert
    assert_eq!(result.output_tokens, 5);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .times(1)
        .returning(|_, _, _| Box::pin(async { Err(AgentError::Runtime("offline".into())) }));
    assert_eq!(
        SessionRunClient::from_channel("session".into(), Arc::new(channel))
            .submit(
                request(),
                mpsc::unbounded_channel().0,
                CancellationToken::new()
            )
            .await
            .expect_err("runtime fails")
            .to_string(),
        "offline"
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_submission_bounds_unresponsive_shutdown() {
    // Arrange
    let mut channel = MockAgentChannel::new();
    channel
        .expect_shutdown_session()
        .times(1)
        .returning(|_| Box::pin(std::future::pending()));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    // Act
    let result = SessionRunClient::from_channel("session".into(), Arc::new(channel))
        .submit(request(), mpsc::unbounded_channel().0, cancellation)
        .await;
    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
}

#[tokio::test]
async fn failed_cleanup_before_submission_preserves_the_interruption_and_records_failure() {
    // Arrange
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let mut channel = MockAgentChannel::new();
    channel.expect_run_turn().never();
    channel.expect_shutdown_session().once().returning(|_| {
        Box::pin(async { Err(AgentError::Runtime("private cleanup failure".into())) })
    });
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let client = SessionRunClient::from_channel("session".into(), Arc::new(channel));
    let root = Span::root("test.cancellation.cleanup", Vec::new());
    let trace_id = root.context().span().span_context().trace_id();

    // Act
    let result = root
        .scope(client.submit(request(), mpsc::unbounded_channel().0, cancellation))
        .await;

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
    let spans = exporter.get_finished_spans().expect("finished spans");
    let cleanup = spans
        .iter()
        .find(|span| span.span_context.trace_id() == trace_id && span.name == "cleanup")
        .expect("failed cleanup span");
    assert!(
        cleanup
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "failed"))
    );
    assert!(
        !spans
            .iter()
            .any(|span| span.span_context.trace_id() == trace_id && span.name == "agent.run")
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_submission_waits_for_cleanup_capacity_before_timeout() {
    // Arrange
    let admission = ag_scheduler::SessionAdmission::new(NonZeroUsize::MIN);
    let occupied_cleanup = admission.acquire_cleanup().await.expect("cleanup slot");
    let shutdown_started = Arc::new(AtomicBool::new(false));
    let shutdown_started_for_channel = Arc::clone(&shutdown_started);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_shutdown_session()
        .once()
        .returning(move |_| {
            shutdown_started_for_channel.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
    let client = session_client_with_admission("waiting", channel, admission);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut submission = tokio::spawn(async move {
        client
            .submit(request(), mpsc::unbounded_channel().0, cancellation)
            .await
    });

    // Act
    let before_capacity = tokio::time::timeout(Duration::from_secs(6), &mut submission).await;
    let shutdown_ran_early = shutdown_started.load(Ordering::SeqCst);
    drop(occupied_cleanup);
    let result = submission.await.expect("submission task");

    // Assert
    assert!(before_capacity.is_err());
    assert!(!shutdown_ran_early);
    assert!(shutdown_started.load(Ordering::SeqCst));
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
}

#[tokio::test]
async fn cancellation_still_attempts_shutdown_after_admission_closes() {
    // Arrange
    let admission = ag_scheduler::SessionAdmission::new(NonZeroUsize::MIN);
    admission.close();
    let mut channel = MockAgentChannel::new();
    channel
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    let client = session_client_with_admission("closed", channel, admission);
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    // Act
    let result = client
        .submit(request(), mpsc::unbounded_channel().0, cancellation)
        .await;

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
}

/// Records when a polled turn relinquishes its owned execution resources.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_drops_started_turn_before_bounded_shutdown() {
    for stuck in [false, true] {
        // Arrange
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let turn_dropped = Arc::clone(&dropped);
        let shutdown_dropped = Arc::clone(&dropped);
        let mut channel = MockAgentChannel::new();
        channel
            .expect_run_turn()
            .once()
            .return_once(move |_, _, _| {
                Box::pin(async move {
                    let _guard = DropFlag(turn_dropped);
                    cancel.cancel();
                    std::future::pending().await
                })
            });
        channel
            .expect_shutdown_session()
            .once()
            .return_once(move |_| {
                Box::pin(async move {
                    assert!(shutdown_dropped.load(Ordering::SeqCst));
                    if stuck {
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                })
            });
        let started = tokio::time::Instant::now();

        // Act
        let result = SessionRunClient::from_channel("session".into(), Arc::new(channel))
            .submit(request(), mpsc::unbounded_channel().0, cancellation)
            .await;

        // Assert
        assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(if stuck { 5 } else { 0 })
        );
    }
}

#[tokio::test]
async fn cancellation_of_running_turn_shuts_down_without_a_cleanup_slot() {
    // Arrange
    let admission = ag_scheduler::SessionAdmission::new(NonZeroUsize::MIN);
    let occupied_cleanup = admission.acquire_cleanup().await.expect("cleanup slot");
    let (started_tx, started_rx) = oneshot::channel();
    let shutdown_started = Arc::new(AtomicBool::new(false));
    let shutdown_started_for_channel = Arc::clone(&shutdown_started);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .return_once(move |_, _, _| {
            Box::pin(async move {
                let _ = started_tx.send(());
                std::future::pending().await
            })
        });
    channel
        .expect_shutdown_session()
        .once()
        .returning(move |_| {
            shutdown_started_for_channel.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
    let client = session_client_with_admission("running", channel, admission);
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let submission = tokio::spawn(async move {
        client
            .submit(request(), mpsc::unbounded_channel().0, cancellation)
            .await
    });
    started_rx.await.expect("turn started");

    // Act
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), submission)
        .await
        .expect("running turn does not wait for cleanup capacity")
        .expect("submission task");

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
    assert!(shutdown_started.load(Ordering::SeqCst));
    drop(occupied_cleanup);
}

#[tokio::test]
async fn cancellation_between_construction_and_first_poll_never_starts_turn() {
    // Arrange
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let polled = Arc::new(AtomicBool::new(false));
    let turn_polled = Arc::clone(&polled);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .return_once(move |_, _, _| {
            cancel.cancel();
            Box::pin(async move {
                turn_polled.store(true, Ordering::SeqCst);
                Err(AgentError::Runtime("canceled request started".into()))
            })
        });
    channel
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));

    // Act
    let result = SessionRunClient::from_channel("session".into(), Arc::new(channel))
        .submit(request(), mpsc::unbounded_channel().0, cancellation)
        .await;

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn session_client_keeps_identity_across_clones_and_propagates_cleanup_failure() {
    // Arrange
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .withf(|id, request, _| id == "owned-session" && request.model == "test-model")
        .once()
        .returning(|_, _, _| {
            Box::pin(async { Err(AgentError::Runtime("provider failure".into())) })
        });
    channel
        .expect_shutdown_session()
        .withf(|id| id == "owned-session")
        .once()
        .returning(|_| Box::pin(async { Err(AgentError::Runtime("cleanup failure".into())) }));
    let client = SessionRunClient::from_channel("owned-session".into(), Arc::new(channel));
    let cloned = client.clone();

    // Act
    let result = cloned
        .submit(
            request(),
            mpsc::unbounded_channel().0,
            CancellationToken::new(),
        )
        .await;
    let cleanup = client.shutdown().await;

    // Assert
    assert_eq!(
        result.expect_err("provider error").to_string(),
        "provider failure"
    );
    assert_eq!(
        cleanup.expect_err("cleanup error").to_string(),
        "cleanup failure"
    );
}

#[tokio::test]
async fn session_clients_share_global_admission_and_cancel_waiting_turns() {
    // Arrange
    let config = RuntimeConfig::default().with_session_parallelism(NonZeroUsize::MIN);
    let admission = config.session_admission.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(Notify::new());
    let release_turn = Arc::clone(&release);
    let mut first_channel = MockAgentChannel::new();
    first_channel
        .expect_run_turn()
        .once()
        .return_once(move |_, _, _| {
            Box::pin(async move {
                let _ = started_tx.send(());
                release_turn.notified().await;
                Ok(completed_turn())
            })
        });
    let second_started = Arc::new(AtomicBool::new(false));
    let second_started_turn = Arc::clone(&second_started);
    let mut second_channel = MockAgentChannel::new();
    second_channel
        .expect_run_turn()
        .once()
        .returning(move |_, _, _| {
            second_started_turn.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(completed_turn()) })
        });
    let cleanup_started = Arc::new(AtomicBool::new(false));
    let cleanup_started_for_channel = Arc::clone(&cleanup_started);
    let mut canceled_channel = MockAgentChannel::new();
    canceled_channel
        .expect_shutdown_session()
        .once()
        .returning(move |_| {
            cleanup_started_for_channel.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
    let first = session_client_with_admission("regular", first_channel, admission.clone());
    let second = session_client_with_admission("managed", second_channel, admission.clone());
    let canceled = session_client_with_admission("canceled", canceled_channel, admission);
    let cancellation = CancellationToken::new();

    // Act
    let first_turn = tokio::spawn(async move {
        first
            .submit(
                request(),
                mpsc::unbounded_channel().0,
                CancellationToken::new(),
            )
            .await
    });
    started_rx.await.expect("first turn started");
    let second_turn = tokio::spawn(async move {
        second
            .submit(
                request(),
                mpsc::unbounded_channel().0,
                CancellationToken::new(),
            )
            .await
    });
    let cancellation_for_waiter = cancellation.clone();
    let canceled_turn = tokio::spawn(async move {
        canceled
            .submit(
                request(),
                mpsc::unbounded_channel().0,
                cancellation_for_waiter,
            )
            .await
    });
    tokio::task::yield_now().await;
    let second_waited = !second_started.load(Ordering::SeqCst);
    cancellation.cancel();
    let canceled_result = canceled_turn.await.expect("canceled task");
    let cleanup_ran_while_turn_occupied = cleanup_started.load(Ordering::SeqCst);
    release.notify_one();
    let first_result = first_turn.await.expect("regular task");
    let second_result = second_turn.await.expect("managed task");

    // Assert
    assert!(second_waited);
    assert!(matches!(
        canceled_result,
        Err(AgentError::InterruptedByUser(_))
    ));
    assert!(cleanup_ran_while_turn_occupied);
    assert_eq!(first_result.expect("regular result").output_tokens, 5);
    assert_eq!(second_result.expect("managed result").output_tokens, 5);
    assert!(second_started.load(Ordering::SeqCst));
}

#[tokio::test]
async fn normal_shutdown_waits_for_shared_cleanup_capacity() {
    // Arrange
    let admission = ag_scheduler::SessionAdmission::new(NonZeroUsize::MIN);
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(Notify::new());
    let release_first = Arc::clone(&release);
    let mut first_channel = MockAgentChannel::new();
    first_channel
        .expect_shutdown_session()
        .once()
        .return_once(move |_| {
            Box::pin(async move {
                let _ = started_tx.send(());
                release_first.notified().await;
                Ok(())
            })
        });
    let second_started = Arc::new(AtomicBool::new(false));
    let second_started_for_channel = Arc::clone(&second_started);
    let mut second_channel = MockAgentChannel::new();
    second_channel
        .expect_shutdown_session()
        .once()
        .returning(move |_| {
            second_started_for_channel.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
    let first = session_client_with_admission("first", first_channel, admission.clone());
    let second = session_client_with_admission("second", second_channel, admission);

    // Act
    let first_shutdown = tokio::spawn(async move { first.shutdown().await });
    started_rx.await.expect("first cleanup started");
    let second_shutdown = tokio::spawn(async move { second.shutdown().await });
    tokio::task::yield_now().await;
    let second_waited = !second_started.load(Ordering::SeqCst);
    release.notify_one();
    let first_result = first_shutdown.await.expect("first shutdown task");
    let second_result = second_shutdown.await.expect("second shutdown task");

    // Assert
    assert!(second_waited);
    assert!(first_result.is_ok());
    assert!(second_result.is_ok());
    assert!(second_started.load(Ordering::SeqCst));
}

#[tokio::test]
async fn closed_admission_rejects_a_turn_before_harness_dispatch() {
    // Arrange
    let admission = ag_scheduler::SessionAdmission::new(NonZeroUsize::MIN);
    admission.close();
    let channel = MockAgentChannel::new();
    let client = session_client_with_admission("closed", channel, admission);

    // Act
    let result = client
        .submit(
            request(),
            mpsc::unbounded_channel().0,
            CancellationToken::new(),
        )
        .await;
    let cleanup = client.shutdown().await;

    // Assert
    assert!(
        matches!(result, Err(AgentError::Runtime(message)) if message.contains("Session admission closed"))
    );
    assert!(
        matches!(cleanup, Err(AgentError::Runtime(message)) if message.contains("Session cleanup admission closed"))
    );
}

fn session_client_with_admission(
    session_id: &str,
    channel: MockAgentChannel,
    admission: ag_scheduler::SessionAdmission,
) -> SessionRunClient {
    let mut client = SessionRunClient::from_channel(session_id.into(), Arc::new(channel));
    client.session_admission = admission;

    client
}

fn completed_turn() -> TurnResult {
    TurnResult {
        assistant_message: ag_protocol::AgentResponse::plain("done"),
        context_reset: false,
        input_tokens: 3,
        output_tokens: 5,
        provider_conversation_id: None,
    }
}
