//! Antigravity persistent-runtime client orchestration.

use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use super::super::client::{ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime};
use super::super::stdio_transport::AppServerStdioTransport;
use super::lifecycle::{self, AntigravityRuntimeState};
use crate::app_server::{
    AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    BorrowedAppServerFuture,
};
use crate::model::agent::{AgentKind, ReasoningLevel};
use crate::model::session::SpeedMode;
use crate::{agent, app_server_transport};

/// Production client backed by `agy --input-format stream-json`.
pub(crate) type RealAntigravityClient = ProviderRuntimeClient<AntigravityRuntimeProvider>;

/// Antigravity hooks used by the shared provider-runtime client.
pub(crate) struct AntigravityRuntimeProvider;

impl RuntimeClientProvider for AntigravityRuntimeProvider {
    type Runtime = AntigravitySessionRuntime;

    fn label() -> &'static str {
        "Antigravity"
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        agent::protocol_schema_instruction_mode(AgentKind::Antigravity)
    }

    fn retain_runtime_after_turn() -> bool {
        true
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        Box::pin(async move {
            lifecycle::start_runtime(&request).map(AntigravitySessionRuntime::from_parts)
        })
    }

    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        _protocol_profile: ProtocolRequestProfile,
        _reasoning_level: ReasoningLevel,
        _speed_mode: SpeedMode,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        Box::pin(async move {
            lifecycle::run_turn_with_runtime(
                &mut runtime.transport,
                &mut runtime.state,
                prompt,
                stream_tx,
            )
            .await
        })
    }
}

/// Active Antigravity stream-input session runtime.
pub(crate) struct AntigravitySessionRuntime {
    child: app_server_transport::AppServerRuntimeChild,
    state: AntigravityRuntimeState,
    transport: AppServerStdioTransport,
}

impl AntigravitySessionRuntime {
    fn from_parts(
        (child, transport, state): (
            app_server_transport::AppServerRuntimeChild,
            AppServerStdioTransport,
            AntigravityRuntimeState,
        ),
    ) -> Self {
        Self {
            child,
            state,
            transport,
        }
    }
}

impl RuntimeClientRuntime for AntigravitySessionRuntime {
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.matches_request(request)
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    fn provider_conversation_id(&self) -> Option<String> {
        self.state.conversation_id().map(str::to_string)
    }

    fn restored_context(&self) -> bool {
        self.state.restored_context()
    }

    fn shutdown_runtime(&mut self) -> BorrowedAppServerFuture<'_, ()> {
        Box::pin(async move {
            self.transport.close_stdin();
            app_server_transport::shutdown_child(&mut self.child).await;
        })
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
