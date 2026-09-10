//! Shared app-server runtime client scaffold.

use std::marker::PhantomData;

use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use crate::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse, BorrowedAppServerFuture,
};
use crate::model::agent::ReasoningLevel;
use crate::model::session::SpeedMode;

/// Provider hook surface for the shared app-server client lifecycle.
pub(crate) trait RuntimeClientProvider: Send + Sync + 'static {
    /// Provider-specific runtime state stored per Agentty session.
    type Runtime: RuntimeClientRuntime + 'static;

    /// User-facing provider label used by retry and lock errors.
    fn label() -> &'static str;

    /// Returns whether prompts should include transport-level schema text.
    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode;

    /// Returns whether successful runtimes remain alive between turns.
    fn retain_runtime_after_turn() -> bool;

    /// Starts and bootstraps one provider runtime for a request.
    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>>;

    /// Runs one turn against an already-started provider runtime.
    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        protocol_profile: ProtocolRequestProfile,
        reasoning_level: ReasoningLevel,
        speed_mode: SpeedMode,
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
        let shutdown_stream_tx = stream_tx.clone();
        let reasoning_level = request.reasoning_level;
        let protocol_profile = request.request_kind.protocol_profile();
        let speed_mode = request.speed_mode;

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: Self::matches_request,
                pid: Self::pid,
                provider_conversation_id: Self::provider_conversation_id,
                retain_runtime_after_turn: Provider::retain_runtime_after_turn(),
                restored_context: Self::restored_context,
            },
            Provider::schema_instruction_mode(),
            |request| {
                let request = request.clone();

                Provider::start_runtime(request)
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(runtime.pid()));

                Provider::run_turn(
                    runtime,
                    prompt,
                    protocol_profile,
                    reasoning_level,
                    speed_mode,
                    stream_tx,
                )
            },
            move |runtime| Self::shutdown_runtime(runtime, &shutdown_stream_tx),
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

    /// Invalidates accounting before releasing a runtime PID, including while
    /// replacement startup or replay is still pending.
    fn shutdown_runtime<'scope>(
        runtime: &'scope mut Provider::Runtime,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, ()> {
        let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(None));

        runtime.shutdown_runtime()
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
#[path = "client_test.rs"]
mod tests;
