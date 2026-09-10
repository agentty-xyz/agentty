use std::path::PathBuf;

use super::*;
use crate::model::agent::AgentModel;
use crate::model::permission::PermissionMode;

fn request(folder: PathBuf) -> AppServerTurnRequest {
    AppServerTurnRequest {
        folder,
        live_transcript: None,
        main_checkout_root: None,
        model: AgentModel::Gemini31Pro.as_str().to_string(),
        permission_mode: PermissionMode::AutoEdit,
        persisted_instruction_conversation_id: None,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Continue"),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        replay_transcript: None,
        request_kind: crate::channel::AgentRequestKind::SessionResume,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::default(),
    }
}

#[test]
fn runtime_matching_includes_reasoning_and_attachment_access() {
    // Arrange
    let folder = tempfile::tempdir().expect("create runtime folder");
    let mut request = request(folder.path().to_path_buf());
    let state = AntigravityRuntimeState::new(&request);

    // Act
    let matching = state.matches_request(&request);
    request.reasoning_level = ReasoningLevel::Low;
    let different_reasoning = state.matches_request(&request);

    // Assert
    assert!(matching);
    assert!(!different_reasoning);
}

#[test]
fn provider_retains_runtime_between_turns() {
    // Arrange / Act / Assert
    assert!(AntigravityRuntimeProvider::retain_runtime_after_turn());
}

fn build_stopped_session_runtime(request: &AppServerTurnRequest) -> AntigravitySessionRuntime {
    let (child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(std::process::Command::new("cat"), "cat")
            .expect("`cat` should spawn as a runtime stand-in");
    let mut transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "Antigravity stdin is unavailable",
        "Failed reading Antigravity stdout",
    );
    transport.close_stdin();

    AntigravitySessionRuntime::from_parts((child, transport, AntigravityRuntimeState::new(request)))
}

#[tokio::test]
async fn runtime_hooks_expose_state_and_shutdown_the_child() {
    // Arrange
    let folder = tempfile::tempdir().expect("create runtime folder");
    let mut request = request(folder.path().to_path_buf());
    request.provider_conversation_id = Some("conversation-1".to_string());
    let mut runtime = build_stopped_session_runtime(&request);

    // Act
    let matches = runtime.matches_request(&request);
    let pid = runtime.pid();
    let conversation_id = runtime.provider_conversation_id();
    let restored = runtime.restored_context();
    runtime.shutdown_runtime().await;

    // Assert
    assert!(matches);
    assert!(pid.is_some());
    assert_eq!(conversation_id.as_deref(), Some("conversation-1"));
    assert!(restored);
}

#[tokio::test]
async fn provider_turn_surfaces_closed_transport_failure() {
    // Arrange
    let folder = tempfile::tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut runtime = build_stopped_session_runtime(&request);
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = AntigravityRuntimeProvider::run_turn(
        &mut runtime,
        &request.prompt,
        ProtocolRequestProfile::SessionTurn,
        ReasoningLevel::Low,
        SpeedMode::Fast,
        stream_tx,
    )
    .await;
    runtime.shutdown_runtime().await;

    // Assert
    assert!(matches!(result, Err(AppServerError::Transport(_))));
}

#[test]
fn provider_metadata_uses_antigravity_transport_schema() {
    // Arrange / Act / Assert
    assert_eq!(AntigravityRuntimeProvider::label(), "Antigravity");
    assert_eq!(
        AntigravityRuntimeProvider::schema_instruction_mode(),
        ProtocolSchemaInstructionMode::TransportSchema
    );
}

#[tokio::test]
async fn provider_start_surfaces_runtime_launch_failure() {
    // Arrange
    let folder = tempfile::tempdir().expect("create runtime parent");
    let request = request(folder.path().join("missing-runtime-folder"));

    // Act
    let result = AntigravityRuntimeProvider::start_runtime(request).await;

    // Assert
    assert!(result.is_err());
}
