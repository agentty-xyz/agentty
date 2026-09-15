use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ag_protocol::TurnPrompt;
use ag_runtime::{
    AgentError, AgentRequestKind, MockAgentChannel, PermissionMode, PersonalityPrompt,
    ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnRequest, TurnResult,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::run_turn;

fn request() -> TurnRequest {
    TurnRequest {
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
    let result = run_turn(
        &channel,
        "session".into(),
        request(),
        mpsc::unbounded_channel().0,
        CancellationToken::new(),
    )
    .await
    .expect("test operation succeeds");
    // Assert
    assert_eq!(result.output_tokens, 5);
    channel.checkpoint();
    channel
        .expect_run_turn()
        .times(1)
        .returning(|_, _, _| Box::pin(async { Err(AgentError::Runtime("offline".into())) }));
    assert_eq!(
        run_turn(
            &channel,
            "session".into(),
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
    let result = run_turn(
        &channel,
        "session".into(),
        request(),
        mpsc::unbounded_channel().0,
        cancellation,
    )
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
        let result = run_turn(
            &channel,
            "session".into(),
            request(),
            mpsc::unbounded_channel().0,
            cancellation,
        )
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
    let result = run_turn(
        &channel,
        "session".into(),
        request(),
        mpsc::unbounded_channel().0,
        cancellation,
    )
    .await;

    // Assert
    assert!(matches!(result, Err(AgentError::InterruptedByUser(_))));
    assert!(!polled.load(Ordering::SeqCst));
}
