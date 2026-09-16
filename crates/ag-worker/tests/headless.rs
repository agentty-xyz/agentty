//! Public execution contract exercised without Agentty or provider binaries.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ag_protocol::{AgentResponse, TurnPrompt};
use ag_runtime::{
    AgentChannel, AgentError, AgentRequestKind, MockAgentChannel, PermissionMode,
    PersonalityPrompt, ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnRequest,
    TurnResult,
};
use ag_worker::{ScheduledCommand, ScheduledWork, SessionOperationRow, WorkQueue, WorkerHost};
use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

struct Run {
    cancellation: CancellationToken,
    order: u64,
}

impl ScheduledCommand for Run {
    fn order(&self) -> Option<u64> {
        Some(self.order)
    }

    fn can_run_while_paused(&self) -> bool {
        false
    }
}

struct Host {
    results: mpsc::UnboundedSender<Result<TurnResult, AgentError>>,
    runtime: Arc<dyn AgentChannel>,
}

impl WorkQueue for Host {
    type Command = Run;
    type Message = Run;

    fn paused(&self) -> bool {
        false
    }

    fn message_order(&self) -> Option<u64> {
        None
    }

    fn pop_message(&self) -> Option<Run> {
        None
    }
}

#[async_trait]
impl WorkerHost for Host {
    async fn execute(&self, work: ScheduledWork<Run, Run>) {
        let (ScheduledWork::Command(run) | ScheduledWork::Message(run)) = work;
        let request = TurnRequest {
            continuation: TurnContinuation::fresh(),
            folder: ".".into(),
            main_checkout_root: None,
            model: "model-independent-of-harness".into(),
            permission_mode: PermissionMode::default(),
            personality: PersonalityPrompt::default(),
            prompt: TurnPrompt::from("hello"),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            response_style: ResponseStyle::default(),
            speed_mode: SpeedMode::default(),
        };
        let result = ag_worker::run_turn(
            self.runtime.as_ref(),
            "session".into(),
            request,
            mpsc::unbounded_channel().0,
            run.cancellation,
        )
        .await;
        assert!(self.results.send(result).is_ok());
    }

    async fn abandon(&self, _work: ScheduledWork<Run, Run>) {
        assert!(
            self.results
                .send(Err(AgentError::InterruptedByUser(
                    "worker closed before execution".into()
                )))
                .is_ok()
        );
    }

    async fn shutdown(&self) {
        assert!(
            self.runtime
                .shutdown_session("session".into())
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn host_submits_observes_cancels_and_recovers_without_a_frontend() {
    // Arrange
    let mut runtime = MockAgentChannel::new();
    runtime
        .expect_run_turn()
        .times(1)
        .returning(|_, request, _| {
            Box::pin(async move {
                assert_eq!(request.model, "model-independent-of-harness");
                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("done"),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 2,
                    provider_conversation_id: None,
                })
            })
        });
    runtime
        .expect_shutdown_session()
        .times(2)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (sender, receiver) = mpsc::unbounded_channel();
    let (results, mut observed) = mpsc::unbounded_channel();
    let canceled = CancellationToken::new();
    canceled.cancel();
    sender
        .send(Run {
            order: 1,
            cancellation: CancellationToken::new(),
        })
        .expect("submit");
    sender
        .send(Run {
            order: 2,
            cancellation: canceled,
        })
        .expect("submit canceled");
    drop(sender);
    // Act
    ag_worker::run(
        Host {
            runtime: Arc::new(runtime),
            results,
        },
        Arc::new(Notify::new()),
        receiver,
    )
    .await;
    let store = RecoveryStore(AtomicBool::new(false), Mutex::new(None));
    let recovery: Result<(), String> =
        ag_worker::recover(&store, "restart", |operations| async move {
            assert_eq!(operations[0].id, "abandoned");
            Ok(())
        })
        .await;
    // Assert
    assert_eq!(
        observed
            .recv()
            .await
            .expect("first result")
            .expect("completed")
            .assistant_message
            .answer,
        "done"
    );
    assert!(matches!(
        observed.recv().await,
        Some(Err(AgentError::InterruptedByUser(_)))
    ));
    assert!(observed.recv().await.is_none());
    assert!(recovery.is_ok());
    assert!(store.0.load(Ordering::SeqCst));
}

#[tokio::test]
async fn public_lifecycle_records_typed_cancellation_without_changing_the_result() {
    // Arrange
    let store = RecoveryStore(AtomicBool::new(false), Mutex::new(None));
    let errors = Mutex::new(Vec::new());

    // Act
    let result = ag_worker::execute(
        &store,
        &ag_worker::HeartbeatClock,
        "active-turn",
        async {
            Err(AgentError::InterruptedByUser(
                "user stopped the turn".into(),
            ))
        },
        |error| matches!(error, AgentError::InterruptedByUser(_)),
        |error| errors.lock().expect("tracking error lock").push(error),
    )
    .await;

    // Assert
    let error = result.expect_err("canceled");
    assert!(
        matches!(&error, AgentError::InterruptedByUser(reason) if reason == "user stopped the turn")
    );
    assert_eq!(
        *store.1.lock().expect("canceled operation lock"),
        Some(("active-turn".into(), error.to_string()))
    );
    assert!(!store.0.load(Ordering::SeqCst));
    assert_eq!(
        errors.into_inner().expect("tracking error lock"),
        Vec::<String>::new()
    );
}

struct RecoveryStore(AtomicBool, Mutex<Option<(String, String)>>);

#[async_trait]
impl ag_worker::OperationRepository<String> for RecoveryStore {
    async fn load_unfinished_session_operations(&self) -> Result<Vec<SessionOperationRow>, String> {
        Ok(vec![SessionOperationRow {
            id: "abandoned".into(),
            session_id: "session".into(),
            kind: "turn".into(),
            status: "running".into(),
            cancel_requested: false,
            queued_at: 1,
            started_at: Some(2),
            heartbeat_at: Some(3),
            finished_at: None,
            last_error: None,
        }])
    }

    async fn fail_unfinished_session_operations(&self, _reason: &str) -> Result<(), String> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn is_cancel_requested_for_operation(&self, _id: &str) -> Result<bool, String> {
        Err("unexpected call".into())
    }

    async fn is_session_operation_unfinished(&self, _id: &str) -> Result<bool, String> {
        Err("unexpected call".into())
    }

    async fn mark_session_operation_canceled(&self, id: &str, reason: &str) -> Result<(), String> {
        *self.1.lock().map_err(|error| error.to_string())? = Some((id.into(), reason.into()));
        Ok(())
    }

    async fn mark_session_operation_done(&self, _id: &str) -> Result<(), String> {
        Err("unexpected call".into())
    }

    async fn mark_session_operation_failed(&self, _id: &str, _error: &str) -> Result<(), String> {
        Err("unexpected call".into())
    }

    async fn mark_session_operation_running(&self, _id: &str) -> Result<(), String> {
        Err("unexpected call".into())
    }

    async fn heartbeat(&self, _id: &str) -> Result<(), String> {
        Err("unexpected call".into())
    }

    async fn claim_session_operation(
        &self,
        _id: &str,
        _session: &str,
        _kind: &str,
    ) -> Result<bool, String> {
        Err("unexpected call".into())
    }

    async fn insert_session_operation(
        &self,
        _id: &str,
        _session: &str,
        _kind: &str,
    ) -> Result<(), String> {
        Err("unexpected call".into())
    }

    async fn request_cancel_for_session_operations(&self, _session: &str) -> Result<(), String> {
        Err("unexpected call".into())
    }
}
