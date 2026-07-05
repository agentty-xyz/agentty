//! Gemini ACP client orchestration.

use ag_protocol::ProtocolSchemaInstructionMode;
use tokio::sync::mpsc;

use super::super::client::{ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime};
use super::super::stdio_transport::AppServerStdioTransport;
use super::lifecycle::{self, GeminiRuntimeState};
use crate::app_server::contract::BorrowedAppServerFuture;
use crate::app_server::{
    AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
};
use crate::model::agent::{AgentKind, ReasoningLevel};
use crate::model::turn_prompt::TurnPrompt;
use crate::{agent, app_server_transport};

/// Production [`AppServerClient`] backed by `gemini --acp`.
pub(crate) type RealGeminiAcpClient = ProviderRuntimeClient<GeminiRuntimeProvider>;

/// Gemini hooks used by the shared app-server runtime client.
pub(crate) struct GeminiRuntimeProvider;

impl RuntimeClientProvider for GeminiRuntimeProvider {
    type Runtime = GeminiSessionRuntime;

    fn label() -> &'static str {
        "Gemini ACP"
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        agent::protocol_schema_instruction_mode(AgentKind::Gemini)
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        Box::pin(async move {
            let (child, transport, state) = lifecycle::start_runtime(&request).await?;

            Ok(GeminiSessionRuntime {
                child,
                state,
                transport,
            })
        })
    }

    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        _reasoning_level: ReasoningLevel,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        Box::pin(async move {
            lifecycle::run_turn_with_runtime(
                &mut runtime.transport,
                &runtime.state.session_id,
                prompt,
                stream_tx,
            )
            .await
        })
    }
}

/// Active Gemini ACP session runtime.
pub(crate) struct GeminiSessionRuntime {
    child: tokio::process::Child,
    state: GeminiRuntimeState,
    transport: AppServerStdioTransport,
}

impl RuntimeClientRuntime for GeminiSessionRuntime {
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder && self.state.model == request.model
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    fn provider_conversation_id(&self) -> Option<String> {
        if self.state.session_id.is_empty() {
            None
        } else {
            Some(self.state.session_id.clone())
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
