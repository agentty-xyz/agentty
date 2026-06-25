//! Gemini ACP client orchestration.

use tokio::sync::mpsc;

use super::lifecycle::{self, GeminiRuntimeState};
use super::transport::GeminiStdioTransport;
use crate::domain::agent::AgentKind;
use crate::infra::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::{agent, app_server_transport};

/// Production [`AppServerClient`] backed by `gemini --acp`.
pub(crate) struct RealGeminiAcpClient {
    sessions: AppServerSessionRegistry<GeminiSessionRuntime>,
}

impl RealGeminiAcpClient {
    /// Creates an empty ACP runtime registry for Gemini sessions.
    pub(crate) fn new() -> Self {
        Self {
            sessions: AppServerSessionRegistry::new("Gemini ACP"),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<GeminiSessionRuntime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, AppServerError> {
        let stream_tx = stream_tx.clone();

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: GeminiSessionRuntime::matches_request,
                pid: |runtime| runtime.child.id(),
                provider_conversation_id: GeminiSessionRuntime::provider_conversation_id,
                restored_context: GeminiSessionRuntime::restored_context,
            },
            agent::protocol_schema_instruction_mode(AgentKind::Gemini),
            |request| {
                let request = request.clone();

                Box::pin(async move {
                    let (child, transport, state) = lifecycle::start_runtime(&request).await?;

                    Ok(GeminiSessionRuntime {
                        child,
                        state,
                        transport,
                    })
                })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Box::pin(async move {
                    lifecycle::run_turn_with_runtime(
                        &mut runtime.transport,
                        &runtime.state.session_id,
                        prompt,
                        stream_tx,
                    )
                    .await
                })
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Terminates one Gemini ACP runtime process.
    async fn shutdown_runtime(session: &mut GeminiSessionRuntime) {
        session.transport.close_stdin();
        app_server_transport::shutdown_child(&mut session.child).await;
    }
}

impl Default for RealGeminiAcpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServerClient for RealGeminiAcpClient {
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

            Self::shutdown_runtime(&mut session_runtime).await;
        })
    }
}

/// Active Gemini ACP session runtime.
struct GeminiSessionRuntime {
    child: tokio::process::Child,
    state: GeminiRuntimeState,
    transport: GeminiStdioTransport,
}

impl GeminiSessionRuntime {
    /// Returns whether the runtime matches one incoming turn request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder && self.state.model == request.model
    }

    /// Returns whether runtime startup restored prior provider context.
    fn restored_context(&self) -> bool {
        self.state.restored_context
    }

    /// Returns the active provider-native Gemini ACP `sessionId`, or `None`
    /// when the runtime has not yet started a session.
    fn provider_conversation_id(&self) -> Option<String> {
        if self.state.session_id.is_empty() {
            None
        } else {
            Some(self.state.session_id.clone())
        }
    }
}
