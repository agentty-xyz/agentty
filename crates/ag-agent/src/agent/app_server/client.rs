//! Shared app-server runtime client scaffold.

use std::marker::PhantomData;

use tokio::sync::mpsc;

use crate::agent::ProtocolSchemaInstructionMode;
use crate::app_server::contract::BorrowedAppServerFuture;
use crate::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::model::agent::ReasoningLevel;
use crate::model::turn_prompt::TurnPrompt;

/// Provider hook surface for the shared app-server client lifecycle.
pub(crate) trait RuntimeClientProvider: Send + Sync + 'static {
    /// Provider-specific runtime state stored per Agentty session.
    type Runtime: RuntimeClientRuntime + 'static;

    /// User-facing provider label used by retry and lock errors.
    fn label() -> &'static str;

    /// Returns whether prompts should include transport-level schema text.
    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode;

    /// Starts and bootstraps one provider runtime for a request.
    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>>;

    /// Runs one turn against an already-started provider runtime.
    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        reasoning_level: ReasoningLevel,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>>;
}

/// Runtime query and shutdown hooks shared by provider clients.
pub(crate) trait RuntimeClientRuntime: Send {
    /// Returns whether the runtime can serve one incoming request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool;

    /// Returns the runtime OS process id, when available.
    fn pid(&self) -> Option<u32>;

    /// Returns the active provider-native conversation id, when available.
    fn provider_conversation_id(&self) -> Option<String>;

    /// Returns whether runtime startup restored provider-native context.
    fn restored_context(&self) -> bool;

    /// Terminates the runtime and waits for process exit.
    fn shutdown_runtime(&mut self) -> BorrowedAppServerFuture<'_, ()>;
}

/// Generic app-server client backed by provider-specific runtime hooks.
pub(crate) struct ProviderRuntimeClient<Provider: RuntimeClientProvider> {
    provider: PhantomData<Provider>,
    sessions: AppServerSessionRegistry<Provider::Runtime>,
}

impl<Provider: RuntimeClientProvider> ProviderRuntimeClient<Provider> {
    /// Creates an empty runtime registry for one provider.
    pub(crate) fn new() -> Self {
        Self {
            provider: PhantomData,
            sessions: AppServerSessionRegistry::new(Provider::label()),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<Provider::Runtime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, AppServerError> {
        let stream_tx = stream_tx.clone();
        let reasoning_level = request.reasoning_level;

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: Self::matches_request,
                pid: Self::pid,
                provider_conversation_id: Self::provider_conversation_id,
                restored_context: Self::restored_context,
            },
            Provider::schema_instruction_mode(),
            |request| {
                let request = request.clone();

                Provider::start_runtime(request)
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Provider::run_turn(runtime, prompt, reasoning_level, stream_tx)
            },
            RuntimeClientRuntime::shutdown_runtime,
        )
        .await
    }

    /// Returns whether the runtime can serve one incoming request.
    fn matches_request(runtime: &Provider::Runtime, request: &AppServerTurnRequest) -> bool {
        runtime.matches_request(request)
    }

    /// Returns the runtime OS process id, when available.
    fn pid(runtime: &Provider::Runtime) -> Option<u32> {
        runtime.pid()
    }

    /// Returns the active provider-native conversation id, when available.
    fn provider_conversation_id(runtime: &Provider::Runtime) -> Option<String> {
        runtime.provider_conversation_id()
    }

    /// Returns whether runtime startup restored provider-native context.
    fn restored_context(runtime: &Provider::Runtime) -> bool {
        runtime.restored_context()
    }
}

impl<Provider: RuntimeClientProvider> Default for ProviderRuntimeClient<Provider> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Provider: RuntimeClientProvider> AppServerClient for ProviderRuntimeClient<Provider> {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let sessions = self.sessions.clone();

        Box::pin(async move { Self::run_turn_internal(&sessions, request, &stream_tx).await })
    }

    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()> {
        let sessions = self.sessions.clone();

        Box::pin(async move {
            let _ = sessions.cancel_active_turn(&session_id);

            let Ok(Some(mut session_runtime)) = sessions.take_session(&session_id) else {
                return;
            };

            session_runtime.shutdown_runtime().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::agent::ProtocolSchemaInstructionMode;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;
    use crate::model::turn_prompt::TurnPrompt;

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

        fn start_runtime(
            request: AppServerTurnRequest,
        ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
            Box::pin(async move {
                START_COUNT.fetch_add(1, Ordering::SeqCst);

                Ok(TestRuntime {
                    folder: request.folder,
                    model: request.model,
                    provider_conversation_id: Some("conversation-1".to_string()),
                    restored_context: true,
                    shutdown_count: &SHUTDOWN_COUNT,
                })
            })
        }

        fn run_turn<'scope>(
            _runtime: &'scope mut Self::Runtime,
            _prompt: &'scope TurnPrompt,
            _reasoning_level: ReasoningLevel,
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
        provider_conversation_id: Option<String>,
        restored_context: bool,
        shutdown_count: &'static AtomicUsize,
    }

    impl RuntimeClientRuntime for TestRuntime {
        fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
            self.folder == request.folder && self.model == request.model
        }

        fn pid(&self) -> Option<u32> {
            Some(42)
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

    #[tokio::test]
    async fn runtime_client_stores_successful_runtime_until_session_shutdown() {
        // Arrange
        RUN_COUNT.store(0, Ordering::SeqCst);
        SHUTDOWN_COUNT.store(0, Ordering::SeqCst);
        START_COUNT.store(0, Ordering::SeqCst);

        let client = ProviderRuntimeClient::<TestProvider>::new();
        let request = AppServerTurnRequest {
            folder: std::env::temp_dir(),
            live_session_output: None,
            main_checkout_root: None,
            model: "test-model".to_string(),
            persisted_instruction_conversation_id: None,
            prompt: TurnPrompt::from_text("Hello".to_string()),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::High,
            request_kind: AgentRequestKind::SessionStart,
            session_id: "session-1".to_string(),
        };
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

        // Act
        let response = client
            .run_turn(request, stream_tx)
            .await
            .expect("turn should succeed");
        client.shutdown_session("session-1".to_string()).await;

        // Assert
        assert_eq!(response.assistant_message, "assistant");
        assert!(!response.context_reset);
        assert_eq!(response.input_tokens, 11);
        assert_eq!(response.output_tokens, 12);
        assert_eq!(response.pid, Some(42));
        assert_eq!(
            response.provider_conversation_id,
            Some("conversation-1".to_string())
        );
        assert_eq!(START_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(RUN_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(SHUTDOWN_COUNT.load(Ordering::SeqCst), 1);
    }
}
