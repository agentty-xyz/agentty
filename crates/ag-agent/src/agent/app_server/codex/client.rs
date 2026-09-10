//! Codex app-server client orchestration.

use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use super::super::client::{ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime};
use super::super::stdio_transport::AppServerStdioTransport;
use super::lifecycle::{self, CodexRuntimeState};
use crate::app_server::{
    AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    BorrowedAppServerFuture,
};
use crate::model::agent::{AgentKind, ReasoningLevel};
use crate::model::session::SpeedMode;
use crate::{agent, app_server_transport};

/// Production [`AppServerClient`] backed by `codex app-server` process
/// instances.
pub(crate) type RealCodexAppServerClient = ProviderRuntimeClient<CodexRuntimeProvider>;

/// Codex hooks used by the shared app-server runtime client.
pub(crate) struct CodexRuntimeProvider;

impl RuntimeClientProvider for CodexRuntimeProvider {
    type Runtime = CodexSessionRuntime;

    fn label() -> &'static str {
        "Codex"
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        agent::protocol_schema_instruction_mode(AgentKind::Codex)
    }

    fn retain_runtime_after_turn() -> bool {
        true
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        Box::pin(async move {
            let (child, transport, state) = lifecycle::start_runtime(&request).await?;

            Ok(CodexSessionRuntime {
                child,
                state,
                transport,
            })
        })
    }

    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        protocol_profile: ProtocolRequestProfile,
        reasoning_level: ReasoningLevel,
        speed_mode: SpeedMode,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        Box::pin(async move {
            lifecycle::run_turn_with_runtime(
                &mut runtime.transport,
                &mut runtime.state,
                prompt,
                protocol_profile,
                reasoning_level,
                speed_mode,
                stream_tx,
            )
            .await
        })
    }
}

/// Active Codex app-server session runtime.
pub(crate) struct CodexSessionRuntime {
    child: app_server_transport::AppServerRuntimeChild,
    state: CodexRuntimeState,
    transport: AppServerStdioTransport,
}

impl RuntimeClientRuntime for CodexSessionRuntime {
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder
            && self.state.model == request.model
            && self.state.permission_mode == request.permission_mode
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    fn provider_conversation_id(&self) -> Option<String> {
        if self.state.thread_id.is_empty() {
            None
        } else {
            Some(self.state.thread_id.clone())
        }
    }

    fn restored_context(&self) -> bool {
        self.state.restored_context
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
