use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use crate::agent::app_server::client::{
    ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime,
};
use crate::app_server;
use crate::app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, BorrowedAppServerFuture,
};
use crate::channel::AgentRequestKind;
use crate::model::agent::ReasoningLevel;
use crate::model::session::SpeedMode;

static RUN_COUNT: AtomicUsize = AtomicUsize::new(0);
static SHUTDOWN_COUNT: AtomicUsize = AtomicUsize::new(0);
static START_COUNT: AtomicUsize = AtomicUsize::new(0);

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
        provider_call_budget: None,
        folder: std::env::temp_dir(),
        live_transcript: None,
        main_checkout_root: None,
        model: "test-model".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        persisted_instruction_conversation_id: None,
        personality: crate::channel::PersonalityPrompt::default(),
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
    RUN_COUNT.store(0, Ordering::SeqCst);
    SHUTDOWN_COUNT.store(0, Ordering::SeqCst);
    START_COUNT.store(0, Ordering::SeqCst);

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
