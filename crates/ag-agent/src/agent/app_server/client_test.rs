use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_contracts::{AgentRequestKind, ReasoningLevel, SpeedMode};
use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::{Mutex, MutexGuard, mpsc};

use crate::agent::app_server::client::{
    ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime,
};
use crate::app_server;
use crate::app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, BorrowedAppServerFuture,
};

// Counter assertions share one async lock across complete test scenarios.
static COUNTER_LOCK: Mutex<()> = Mutex::const_new(());
static RUN_COUNT: AtomicUsize = AtomicUsize::new(0);
static SHUTDOWN_COUNT: AtomicUsize = AtomicUsize::new(0);
static START_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Holds exclusive access until the caller finishes its runtime assertions.
async fn reset_counters() -> MutexGuard<'static, ()> {
    let guard = COUNTER_LOCK.lock().await;
    RUN_COUNT.store(0, Ordering::SeqCst);
    SHUTDOWN_COUNT.store(0, Ordering::SeqCst);
    START_COUNT.store(0, Ordering::SeqCst);

    guard
}

struct TestProvider;

impl RuntimeClientProvider for TestProvider {
    type Runtime = TestRuntime;

    fn label() -> &'static str {
        "Test"
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        ProtocolSchemaInstructionMode::TransportSchema
    }

    fn retain_runtime_after_turn() -> bool {
        true
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        Box::pin(async move {
            START_COUNT.fetch_add(1, Ordering::SeqCst);

            Ok(TestRuntime {
                on_reset: None,
                panic_on_drop: false,
                folder: request.folder,
                model: request.model,
                pid: 42,
                provider_conversation_id: Some("conversation-1".to_string()),
                restored_context: true,
                shutdown_count: &SHUTDOWN_COUNT,
            })
        })
    }

    fn run_turn<'scope>(
        _runtime: &'scope mut Self::Runtime,
        _prompt: &'scope TurnPrompt,
        _protocol_profile: ProtocolRequestProfile,
        _reasoning_level: ReasoningLevel,
        _speed_mode: SpeedMode,
        _stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        Box::pin(async {
            RUN_COUNT.fetch_add(1, Ordering::SeqCst);

            Ok(("assistant".to_string(), 11, 12))
        })
    }
}

struct TestRuntime {
    on_reset: Option<Box<dyn FnOnce() + Send>>,
    panic_on_drop: bool,
    folder: PathBuf,
    model: String,
    pid: u32,
    provider_conversation_id: Option<String>,
    restored_context: bool,
    shutdown_count: &'static AtomicUsize,
}

impl RuntimeClientRuntime for TestRuntime {
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.folder == request.folder && self.model == request.model
    }

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn provider_conversation_id(&self) -> Option<String> {
        self.provider_conversation_id.clone()
    }

    fn restored_context(&self) -> bool {
        self.restored_context
    }

    fn shutdown_runtime(&mut self) -> BorrowedAppServerFuture<'_, ()> {
        Box::pin(async move {
            self.shutdown_count.fetch_add(1, Ordering::SeqCst);
        })
    }
}

fn make_request() -> AppServerTurnRequest {
    AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder: std::env::temp_dir(),
        live_transcript: None,
        main_checkout_root: None,
        model: "test-model".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        persisted_instruction_conversation_id: None,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: TurnPrompt::from_text("Hello".to_string()),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        request_kind: AgentRequestKind::SessionStart,
        replay_transcript: None,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::default(),
    }
}

#[tokio::test]
async fn retry_shutdown_clears_pid_before_delayed_replacement_startup() {
    // Arrange
    static RETRY_SHUTDOWN_COUNT: AtomicUsize = AtomicUsize::new(0);
    let sessions = AppServerSessionRegistry::new("Test");
    let request = make_request();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut release_rx = Some(release_rx);
    let starts = std::sync::Arc::new(AtomicUsize::new(0));
    let start_count = std::sync::Arc::clone(&starts);
    let shutdown_tx = stream_tx.clone();

    // Act
    let turn = tokio::spawn(async move {
        app_server::run_turn_with_restart_retry(
            &sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: TestRuntime::matches_request,
                pid: TestRuntime::pid,
                provider_conversation_id: TestRuntime::provider_conversation_id,
                retain_runtime_after_turn: true,
                restored_context: TestRuntime::restored_context,
            },
            ProtocolSchemaInstructionMode::TransportSchema,
            move |request| {
                let attempt = start_count.fetch_add(1, Ordering::SeqCst);
                let release = if attempt == 0 {
                    None
                } else {
                    release_rx.take()
                };
                let runtime = TestRuntime {
                    on_reset: None,
                    panic_on_drop: false,
                    folder: request.folder.clone(),
                    model: request.model.clone(),
                    pid: if attempt == 0 { 42 } else { 84 },
                    provider_conversation_id: None,
                    restored_context: true,
                    shutdown_count: &RETRY_SHUTDOWN_COUNT,
                };

                Box::pin(async move {
                    if let Some(release) = release {
                        release.await.expect("release replacement startup");
                    }

                    Ok(runtime)
                })
            },
            move |runtime, _| {
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(runtime.pid()));
                let failed = runtime.pid == 42;

                Box::pin(async move {
                    if failed {
                        return Err(AppServerError::Provider("first runtime exited".to_string()));
                    }

                    Ok(("recovered".to_string(), 1, 1))
                })
            },
            move |runtime| {
                ProviderRuntimeClient::<TestProvider>::shutdown_runtime(runtime, &shutdown_tx)
            },
        )
        .await
    });
    let first_pid = stream_rx.recv().await.expect("first runtime published");
    let cleared_pid = stream_rx.recv().await.expect("shutdown clears runtime");

    // Assert
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(first_pid, AppServerStreamEvent::PidUpdate(Some(42)));
    assert_eq!(cleared_pid, AppServerStreamEvent::PidUpdate(None));
    assert!(!turn.is_finished());
    assert!(stream_rx.try_recv().is_err());
    release_tx.send(()).expect("finish replacement startup");
    let response = turn.await.expect("join turn").expect("retry succeeds");
    assert_eq!(
        stream_rx.recv().await,
        Some(AppServerStreamEvent::PidUpdate(Some(84)))
    );
    assert_eq!(response.pid, Some(84));
}

#[tokio::test]
async fn runtime_client_retains_and_replaces_runtime_with_pid_updates() {
    // Arrange
    let _counter_guard = reset_counters().await;

    let client = ProviderRuntimeClient::<TestProvider>::new();
    let request = make_request();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    // Act
    let response = client
        .run_turn(request, stream_tx)
        .await
        .expect("turn should succeed");
    let mut replacement_request = make_request();
    replacement_request.model = "replacement-model".to_string();
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    client
        .run_turn(replacement_request, replacement_tx)
        .await
        .expect("replacement turn should succeed");
    client.shutdown_session("session-1".to_string()).await;

    // Assert
    assert_eq!(response.assistant_message, "assistant");
    assert!(!response.context_reset);
    assert_eq!(response.input_tokens, 11);
    assert_eq!(response.output_tokens, 12);
    assert_eq!(response.pid, Some(42));
    assert_eq!(
        stream_rx.try_recv().expect("runtime PID before turn"),
        AppServerStreamEvent::PidUpdate(Some(42))
    );
    assert_eq!(
        response.provider_conversation_id,
        Some("conversation-1".to_string())
    );
    assert_eq!(
        replacement_rx.try_recv().expect("retired PID cleared"),
        AppServerStreamEvent::PidUpdate(None)
    );
    assert_eq!(
        replacement_rx
            .try_recv()
            .expect("replacement PID published"),
        AppServerStreamEvent::PidUpdate(Some(42))
    );
    assert_eq!(START_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(RUN_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(SHUTDOWN_COUNT.load(Ordering::SeqCst), 2);
}

struct ResetProvider;

impl RuntimeClientProvider for ResetProvider {
    type Runtime = TestRuntime;

    fn label() -> &'static str {
        "Reset fixture"
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        TestProvider::schema_instruction_mode()
    }

    fn retain_runtime_after_turn() -> bool {
        false
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        TestProvider::start_runtime(request)
    }

    fn reset_context<'scope>(
        runtime: &'scope mut TestRuntime,
        request: &'scope AppServerTurnRequest,
    ) -> BorrowedAppServerFuture<'scope, Result<bool, AppServerError>> {
        Box::pin(async move {
            if request.prompt.text == "fail reset" {
                return Err(AppServerError::Provider("reset failed".into()));
            }
            if let Some(on_reset) = runtime.on_reset.take() {
                on_reset();
            }
            runtime.provider_conversation_id = Some("fresh-conversation".into());
            runtime.restored_context = false;
            Ok(true)
        })
    }

    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        profile: ProtocolRequestProfile,
        reasoning: ReasoningLevel,
        speed: SpeedMode,
        stream: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        TestProvider::run_turn(runtime, prompt, profile, reasoning, speed, stream)
    }
}

#[tokio::test]
async fn isolated_turns_reuse_process_with_fresh_context_and_discard_failed_resets() {
    // Arrange
    let _counter_guard = reset_counters().await;
    let client = ProviderRuntimeClient::<ResetProvider>::new();
    let (stream, _) = mpsc::unbounded_channel();
    let request = make_request();
    // Act
    let first = client
        .run_isolated_turn(request.clone(), stream.clone())
        .await
        .expect("first");
    let second = client
        .run_isolated_turn(request.clone(), stream.clone())
        .await
        .expect("second");
    let mut failed = request.clone();
    failed.prompt = TurnPrompt::from("fail reset");
    let recovered = client
        .run_isolated_turn(failed, stream.clone())
        .await
        .expect("restart after failed reset");
    let third = client
        .run_isolated_turn(request, stream)
        .await
        .expect("restart after failure");
    client.shutdown_session("session-1".into()).await;
    // Assert
    assert_eq!(first.pid, second.pid);
    assert_eq!(
        second.provider_conversation_id.as_deref(),
        Some("fresh-conversation")
    );
    assert_eq!(
        recovered.provider_conversation_id,
        first.provider_conversation_id
    );
    assert_eq!(
        third.provider_conversation_id,
        second.provider_conversation_id
    );
    assert_eq!(START_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(SHUTDOWN_COUNT.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn isolated_turns_restart_unsupported_or_incompatible_runtimes() {
    // Arrange
    let _counter_guard = reset_counters().await;
    let client = ProviderRuntimeClient::<TestProvider>::new();
    let (stream, _) = mpsc::unbounded_channel();
    let mut request = make_request();
    // Act
    client
        .run_isolated_turn(request.clone(), stream.clone())
        .await
        .expect("first");
    client
        .run_isolated_turn(request.clone(), stream.clone())
        .await
        .expect("unsupported reset");
    request.model = "different".into();
    client
        .run_isolated_turn(request, stream)
        .await
        .expect("incompatible process");
    client.shutdown_session("session-1".into()).await;
    // Assert
    assert_eq!(START_COUNT.load(Ordering::SeqCst), 3);
    assert_eq!(SHUTDOWN_COUNT.load(Ordering::SeqCst), 3);
}

impl Drop for TestRuntime {
    fn drop(&mut self) {
        assert!(
            !self.panic_on_drop,
            "fixture panic while registry lock is held"
        );
    }
}

#[tokio::test]
async fn failed_registry_store_after_context_reset_shuts_down_the_runtime() {
    // Arrange
    let _counter_guard = reset_counters().await;
    let client = ProviderRuntimeClient::<ResetProvider>::new();
    let request = make_request();
    let mut runtime = TestProvider::start_runtime(request.clone())
        .await
        .expect("runtime");
    let mut poisoning = TestProvider::start_runtime(request.clone())
        .await
        .expect("poison fixture");
    poisoning.panic_on_drop = true;
    let replacement = TestProvider::start_runtime(request.clone())
        .await
        .expect("replacement fixture");
    let sessions = client.sessions.clone();
    runtime.on_reset = Some(Box::new(move || {
        assert!(
            sessions
                .store_session_or_recover("poison".into(), poisoning)
                .is_ok()
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = sessions.store_session_or_recover("poison".into(), replacement);
        }));
        assert!(result.is_err());
    }));
    assert!(
        client
            .sessions
            .store_session_or_recover(request.session_id.clone(), runtime)
            .is_ok()
    );
    let (stream, _) = mpsc::unbounded_channel();
    // Act
    let result = client.run_isolated_turn(request, stream).await;
    // Assert
    assert!(matches!(result, Err(AppServerError::LockPoisoned { .. })));
    assert_eq!(SHUTDOWN_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(RUN_COUNT.load(Ordering::SeqCst), 0);
}

/// Exercise the shared-counter scenarios together even under process-isolated
/// runners such as nextest.
#[test]
fn counter_scenarios_are_isolated_when_run_in_one_process() {
    // Arrange
    let scenarios: [fn(); 4] = [
        runtime_client_retains_and_replaces_runtime_with_pid_updates,
        isolated_turns_reuse_process_with_fresh_context_and_discard_failed_resets,
        isolated_turns_restart_unsupported_or_incompatible_runtimes,
        failed_registry_store_after_context_reset_shuts_down_the_runtime,
    ];

    // Act / Assert: scoped threads propagate any scenario assertion failures.
    std::thread::scope(|scope| {
        for scenario in scenarios {
            scope.spawn(scenario);
        }
    });
}
