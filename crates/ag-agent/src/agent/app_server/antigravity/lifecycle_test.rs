#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_protocol::{TurnPrompt, TurnPromptAttachment};
use mockall::Sequence;
use tempfile::tempdir;
use tokio::sync::mpsc;

use crate::agent::app_server::antigravity::lifecycle::{
    AntigravityRuntimeState, append_conversation_argument, run_turn_with_runtime,
    run_turn_with_timeout, start_runtime_with_backend, start_runtime_with_built_command,
};
use crate::agent::app_server::stdio_transport::MockAppServerRuntimeTransport;
use crate::agent::backend::{AgentBackendError, MockAgentBackend};
use crate::app_server::{AppServerStreamEvent, AppServerTurnRequest};
use crate::app_server_transport;
use crate::model::agent::{AgentModel, ReasoningLevel};
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;

fn request(folder: PathBuf) -> AppServerTurnRequest {
    AppServerTurnRequest {
        provider_call_budget: None,
        folder,
        live_transcript: None,
        main_checkout_root: None,
        model: AgentModel::Gemini31Pro.as_str().to_string(),
        permission_mode: PermissionMode::AutoEdit,
        persisted_instruction_conversation_id: None,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Inspect the architecture"),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        replay_transcript: None,
        request_kind: crate::channel::AgentRequestKind::SessionStart,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::default(),
    }
}

#[test]
fn restored_state_exposes_conversation_and_matches_original_request() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let mut request = request(folder.path().to_path_buf());
    request.provider_conversation_id = Some("conversation-1".to_string());

    // Act
    let state = AntigravityRuntimeState::new(&request);

    // Assert
    assert_eq!(state.conversation_id(), Some("conversation-1"));
    assert!(state.restored_context());
    assert!(state.matches_request(&request));
}

#[test]
fn runtime_rejects_changed_launch_settings_and_new_attachment_roots() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let attachment_folder = tempdir().expect("create attachment folder");
    let request = request(folder.path().to_path_buf());
    let state = AntigravityRuntimeState::new(&request);
    let mut changed_model = request.clone();
    changed_model.model = "different-model".to_string();
    let mut changed_permission = request.clone();
    changed_permission.permission_mode = PermissionMode::ReadOnly;
    let mut changed_reasoning = request.clone();
    changed_reasoning.reasoning_level = ReasoningLevel::Low;
    let mut added_attachment = request;
    added_attachment.prompt.attachments = vec![TurnPromptAttachment {
        placeholder: "[Image #1]".to_string(),
        local_image_path: attachment_folder.path().join("image.png"),
    }];

    // Act / Assert
    assert!(!state.matches_request(&changed_model));
    assert!(!state.matches_request(&changed_permission));
    assert!(!state.matches_request(&changed_reasoning));
    assert!(!state.matches_request(&added_attachment));
}

#[test]
fn conversation_argument_is_appended_for_native_resume() {
    // Arrange
    let mut command = Command::new("agy");

    // Act
    append_conversation_argument(&mut command, "conversation-1");

    // Assert
    assert_eq!(
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["--conversation", "conversation-1"]
    );
}

#[tokio::test]
async fn prebuilt_runtime_starts_with_restored_state() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let mut request = request(folder.path().to_path_buf());
    request.provider_conversation_id = Some("conversation-1".to_string());

    // Act
    let (mut child, mut transport, state) =
        start_runtime_with_built_command(Command::new("cat"), &request)
            .expect("`cat` should start as an Antigravity runtime stand-in");

    // Assert
    assert_eq!(state.conversation_id(), Some("conversation-1"));
    assert!(state.restored_context());
    transport.close_stdin();
    app_server_transport::shutdown_child(&mut child).await;
}

#[tokio::test]
async fn backend_runtime_builds_and_starts_the_returned_command() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut backend = MockAgentBackend::new();
    backend
        .expect_build_command()
        .times(1)
        .returning(|build_request| {
            assert_eq!(build_request.prompt, "Inspect the architecture");

            Ok(Command::new("cat"))
        });

    // Act
    let (mut child, mut transport, state) =
        start_runtime_with_backend(&request, &backend).expect("mock backend command should start");

    // Assert
    assert!(state.matches_request(&request));
    transport.close_stdin();
    app_server_transport::shutdown_child(&mut child).await;
}

#[test]
fn backend_runtime_wraps_command_build_errors() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut backend = MockAgentBackend::new();
    backend.expect_build_command().times(1).returning(|_| {
        Err(AgentBackendError::CommandBuild(
            "unsupported test CLI".to_string(),
        ))
    });

    // Act
    let error = start_runtime_with_backend(&request, &backend)
        .err()
        .expect("command build failure should fail startup");

    // Assert
    assert_eq!(
        error.to_string(),
        "Failed to build Antigravity runtime command: unsupported test CLI"
    );
}

#[tokio::test]
async fn turn_writes_user_event_and_returns_step_usage() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut state = AntigravityRuntimeState::new(&request);
    let written_payload = Arc::new(Mutex::new(None));
    let mut transport = MockAppServerRuntimeTransport::new();
    let mut sequence = Sequence::new();
    let written_payload_clone = Arc::clone(&written_payload);
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |payload| {
            *written_payload_clone.lock().expect("payload lock") = Some(payload);

            Box::pin(async { Ok(()) })
        });
    let lines = Arc::new(Mutex::new(vec![
        serde_json::json!({
            "event": "result",
            "result": {
                "conversation_id": "conversation-1",
                "status": "SUCCESS",
                "response": "{\"answer\":\"done\"}",
                "usage": {"input_tokens": 110, "output_tokens": 12},
            },
        })
        .to_string(),
        serde_json::json!({
            "event": "step_update",
            "step_update": {
                "conversation_id": "conversation-1",
                "step_index": 2,
                "state": "DONE",
                "step_type": "agent_response",
                "text_delta": "partial",
                "usage": {"input_tokens": 10, "output_tokens": 2},
            },
        })
        .to_string(),
        serde_json::json!({"event": "init"}).to_string(),
        "not-json".to_string(),
    ]));
    transport.expect_next_stdout().times(4).returning(move || {
        let line = lines.lock().expect("line lock").pop();

        Box::pin(async move { Ok(line) })
    });
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    // Act
    let output = run_turn_with_runtime(&mut transport, &mut state, &request.prompt, stream_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(output, ("{\"answer\":\"done\"}".to_string(), 10, 2));
    assert_eq!(state.conversation_id(), Some("conversation-1"));
    let payload = written_payload
        .lock()
        .expect("payload lock")
        .clone()
        .expect("prompt payload should be written");
    assert_eq!(payload["event"], "user");
    assert_eq!(payload["message"]["content"], "Inspect the architecture");
    assert_eq!(
        stream_rx.try_recv().expect("assistant delta should stream"),
        AppServerStreamEvent::AssistantMessage {
            is_delta: true,
            message: "partial".to_string(),
            phase: None,
        }
    );
}

#[tokio::test]
async fn turn_reports_runtime_eof_before_result() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut state = AntigravityRuntimeState::new(&request);
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .returning(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_next_stdout()
        .returning(|| Box::pin(async { Ok(None) }));
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let error = run_turn_with_runtime(&mut transport, &mut state, &request.prompt, stream_tx)
        .await
        .expect_err("stdout EOF should fail the turn");

    // Assert
    assert_eq!(
        error.to_string(),
        "Antigravity terminated before emitting a turn result"
    );
}

#[tokio::test]
async fn turn_timeout_reports_configured_seconds() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut state = AntigravityRuntimeState::new(&request);
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .returning(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_next_stdout()
        .returning(|| Box::pin(std::future::pending()));
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let error = run_turn_with_timeout(
        &mut transport,
        &mut state,
        &request.prompt,
        stream_tx,
        Duration::ZERO,
    )
    .await
    .expect_err("pending stdout should time out");

    // Assert
    assert_eq!(
        error.to_string(),
        "Timed out waiting for Antigravity turn completion after 0 seconds"
    );
}

#[tokio::test]
async fn turn_surfaces_provider_result_error() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut state = AntigravityRuntimeState::new(&request);
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .returning(|_| Box::pin(async { Ok(()) }));
    transport.expect_next_stdout().returning(|| {
        Box::pin(async {
            Ok(Some(
                serde_json::json!({
                    "event": "result",
                    "result": {"status": "ERROR", "error": "quota exhausted"},
                })
                .to_string(),
            ))
        })
    });
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let error = run_turn_with_runtime(&mut transport, &mut state, &request.prompt, stream_tx)
        .await
        .expect_err("failed provider result should fail the turn");

    // Assert
    assert_eq!(error.to_string(), "quota exhausted");
}

#[tokio::test]
async fn turn_reports_missing_result_response() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut state = AntigravityRuntimeState::new(&request);
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .returning(|_| Box::pin(async { Ok(()) }));
    transport.expect_next_stdout().returning(|| {
        Box::pin(async {
            Ok(Some(
                serde_json::json!({
                    "event": "result",
                    "result": {"status": "SUCCESS"},
                })
                .to_string(),
            ))
        })
    });
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let error = run_turn_with_runtime(&mut transport, &mut state, &request.prompt, stream_tx)
        .await
        .expect_err("response-free result should fail the turn");

    // Assert
    assert_eq!(
        error.to_string(),
        "Antigravity result did not contain a response"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn turn_rejects_non_utf8_attachment_path_before_writing() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = request(folder.path().to_path_buf());
    let mut state = AntigravityRuntimeState::new(&request);
    let prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: OsString::from_vec(vec![0x66, 0x80, 0x6f]).into(),
        }],
        ..TurnPrompt::from("Review [Image #1]")
    };
    let mut transport = MockAppServerRuntimeTransport::new();
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let error = run_turn_with_runtime(&mut transport, &mut state, &prompt, stream_tx)
        .await
        .expect_err("non-UTF-8 image path should fail prompt rendering");

    // Assert
    assert_eq!(
        error.to_string(),
        "Antigravity prompt image path is not valid UTF-8"
    );
}
