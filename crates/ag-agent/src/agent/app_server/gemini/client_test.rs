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
        ProtocolRequestProfile::SessionTurn,
        ReasoningLevel::default(),
        SpeedMode::Fast,
        stream_tx,
    )
    .await;

    // Assert
    let error = result.expect_err("a closed runtime stdin should fail the turn");
    assert!(matches!(error, AppServerError::Transport(_)));
}
