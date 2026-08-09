//! Gemini ACP client orchestration.

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use super::super::client::{ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime};
use super::super::stdio_transport::AppServerStdioTransport;
use super::lifecycle::{self, GeminiRuntimeState};
use crate::app_server::{
    AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    BorrowedAppServerFuture,
};
use crate::model::agent::{AgentKind, ReasoningLevel};
use crate::model::session::SpeedMode;
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

    fn retain_runtime_after_turn() -> bool {
        false
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
        _speed_mode: SpeedMode,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        Box::pin(async move {
            lifecycle::run_turn_with_runtime(
                &mut runtime.transport,
                &runtime.state.session_id,
                runtime.state.permission_mode,
                prompt,
                stream_tx,
            )
            .await
        })
    }
}

/// Active Gemini ACP session runtime.
pub(crate) struct GeminiSessionRuntime {
    child: app_server_transport::AppServerRuntimeChild,
    state: GeminiRuntimeState,
    transport: AppServerStdioTransport,
}

impl RuntimeClientRuntime for GeminiSessionRuntime {
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder
            && self.state.model == request.model
            && self.state.permission_mode == request.permission_mode
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::agent::AgentModel;

    /// Builds one Gemini session runtime whose stdin is already closed so turn
    /// writes fail deterministically without a live ACP process.
    fn build_stopped_session_runtime() -> GeminiSessionRuntime {
        let (child, stdin, stdout) =
            app_server_transport::spawn_runtime_command(std::process::Command::new("cat"), "cat")
                .expect("`cat` should spawn as a runtime stand-in");
        let mut transport = AppServerStdioTransport::new(
            stdin,
            stdout,
            "Gemini ACP stdin is unavailable",
            "Failed reading Gemini ACP stdout",
        );
        transport.close_stdin();
        let mut state = GeminiRuntimeState::new(
            PathBuf::from("/tmp/agentty-gemini-runtime"),
            AgentModel::Gemini31Pro.as_str().to_string(),
            crate::model::permission::PermissionMode::AutoEdit,
        );
        state.session_id = "session-1".to_string();

        GeminiSessionRuntime {
            child,
            state,
            transport,
        }
    }

    #[tokio::test]
    async fn runtime_reuse_requires_matching_permission_mode() {
        // Arrange
        let mut runtime = build_stopped_session_runtime();
        let mut request = AppServerTurnRequest {
            folder: runtime.state.folder.clone(),
            live_transcript: None,
            main_checkout_root: None,
            model: runtime.state.model.clone(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            persisted_instruction_conversation_id: None,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: TurnPrompt::from("Continue"),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            replay_transcript: None,
            request_kind: crate::channel::AgentRequestKind::SessionResume,
            session_id: "session-1".to_string(),
            speed_mode: SpeedMode::default(),
        };

        // Act
        let auto_edit_matches = runtime.matches_request(&request);
        request.permission_mode = crate::model::permission::PermissionMode::ReadOnly;
        let read_only_matches = runtime.matches_request(&request);
        runtime.shutdown_runtime().await;

        // Assert
        assert!(auto_edit_matches);
        assert!(!read_only_matches);
    }

    #[tokio::test]
    async fn run_turn_ignores_speed_mode_and_surfaces_transport_failures() {
        // Arrange
        let mut runtime = build_stopped_session_runtime();
        let prompt = TurnPrompt::from("Implement the task");
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

        // Act
        let result = GeminiRuntimeProvider::run_turn(
            &mut runtime,
            &prompt,
            ReasoningLevel::default(),
            SpeedMode::Fast,
            stream_tx,
        )
        .await;

        // Assert
        let error = result.expect_err("a closed runtime stdin should fail the turn");
        assert!(matches!(error, AppServerError::Transport(_)));
    }
}
