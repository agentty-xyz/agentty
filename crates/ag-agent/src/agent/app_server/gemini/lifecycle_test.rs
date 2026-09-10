use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::CLIENT_METHOD_NAMES;
use mockall::Sequence;
use tempfile::tempdir;

use super::*;
use crate::agent::app_server::stdio_transport::MockAppServerRuntimeTransport;
use crate::model::agent::{AgentModel, ReasoningLevel};
use crate::model::session::SpeedMode;

fn turn_request(folder: PathBuf, permission_mode: PermissionMode) -> AppServerTurnRequest {
    AppServerTurnRequest {
        folder,
        live_transcript: None,
        main_checkout_root: None,
        model: AgentModel::Gemini31Pro.as_str().to_string(),
        permission_mode,
        persisted_instruction_conversation_id: None,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Inspect the architecture"),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        replay_transcript: None,
        request_kind: crate::channel::AgentRequestKind::SessionStart,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::Normal,
    }
}

#[test]
fn utility_prompts_receive_long_running_bootstrap_timeout() {
    // Arrange
    let utility_request_kind = crate::channel::AgentRequestKind::UtilityPrompt;
    let session_request_kind = crate::channel::AgentRequestKind::SessionStart;

    // Act
    let utility_timeout = bootstrap_response_timeout(&utility_request_kind);
    let session_timeout = bootstrap_response_timeout(&session_request_kind);

    // Assert
    assert_eq!(utility_timeout, app_server_transport::TURN_TIMEOUT);
    assert_eq!(session_timeout, app_server_transport::STARTUP_TIMEOUT);
}

#[test]
fn prompt_image_mime_type_uses_file_extension() {
    // Arrange
    let paths = ["image.GIF", "image.jpg", "image.webp", "image"];

    // Act
    let mime_types = paths.map(Path::new).map(prompt_image_mime_type);

    // Assert
    assert_eq!(
        mime_types,
        ["image/gif", "image/jpeg", "image/webp", "image/png"]
    );
}

#[tokio::test]
async fn start_runtime_reports_spawn_error_for_missing_folder() {
    // Arrange
    let runtime_parent = tempdir().expect("create runtime parent");
    let request = turn_request(
        runtime_parent.path().join("missing-runtime"),
        PermissionMode::ReadOnly,
    );

    // Act
    let result = start_runtime(&request).await;

    // Assert
    assert!(matches!(
        result,
        Err(error) if error.to_string().contains("Failed to spawn `gemini --acp`")
    ));
}

#[tokio::test]
async fn start_runtime_with_built_command_constructs_state_before_bootstrap() {
    // Arrange
    let folder = tempdir().expect("create runtime folder");
    let request = turn_request(folder.path().to_path_buf(), PermissionMode::ReadOnly);

    // Act
    let result =
        start_runtime_with_built_command(std::process::Command::new("cat"), &request).await;

    // Assert
    let error = result
        .err()
        .expect("an echoing runtime should not return a usable session id");
    assert!(
        error.to_string().contains("initialize"),
        "unexpected bootstrap error: {error}"
    );
}

#[tokio::test]
async fn bootstrap_runtime_session_propagates_long_running_response_timeout() {
    // Arrange
    let folder = tempdir().expect("create session folder");
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .times(3)
        .returning(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_wait_for_response_line_with_timeout()
        .times(2)
        .withf(|_, response_timeout| *response_timeout == app_server_transport::TURN_TIMEOUT)
        .returning(|response_id, _| {
            let response = if response_id.starts_with("init-") {
                let result = InitializeResponse::new(ProtocolVersion::LATEST);

                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": response_id,
                    "result": result,
                })
            } else {
                let result = NewSessionResponse::new("gemini-review-session");

                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": response_id,
                    "result": result,
                })
            };

            Box::pin(async move { Ok(response.to_string()) })
        });

    // Act
    let session_id = bootstrap_runtime_session(
        &mut transport,
        folder.path(),
        app_server_transport::TURN_TIMEOUT,
    )
    .await;

    // Assert
    assert_eq!(
        session_id.expect("Gemini bootstrap should accept the long-running timeout"),
        "gemini-review-session"
    );
}

#[tokio::test]
async fn initialize_runtime_uses_long_running_response_timeout() {
    // Arrange
    let request_id = Arc::new(Mutex::new(None));
    let mut transport = MockAppServerRuntimeTransport::new();
    let mut sequence = Sequence::new();
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some(AGENT_METHOD_NAMES.initialize)
        })
        .returning({
            let request_id = Arc::clone(&request_id);

            move |payload| {
                *request_id
                    .lock()
                    .expect("initialize id lock should remain usable") = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_wait_for_response_line_with_timeout()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|_, response_timeout| *response_timeout == app_server_transport::TURN_TIMEOUT)
        .returning(move |_, _| {
            let response_id = request_id
                .lock()
                .expect("initialize id lock should remain usable")
                .clone()
                .expect("initialize id should be captured");
            let response = InitializeResponse::new(ProtocolVersion::LATEST);

            Box::pin(async move {
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": response_id,
                    "result": response,
                })
                .to_string())
            })
        });
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("initialized"))
        .return_once(|_| Box::pin(async { Ok(()) }));

    // Act
    let result = initialize_runtime(&mut transport, app_server_transport::TURN_TIMEOUT).await;

    // Assert
    result.expect("Gemini initialization should accept the long-running timeout");
}

#[tokio::test]
async fn start_session_uses_long_running_response_timeout() {
    // Arrange
    let folder = tempdir().expect("create session folder");
    let request_id = Arc::new(Mutex::new(None));
    let mut transport = MockAppServerRuntimeTransport::new();
    let mut sequence = Sequence::new();
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some(AGENT_METHOD_NAMES.session_new)
        })
        .returning({
            let request_id = Arc::clone(&request_id);

            move |payload| {
                *request_id
                    .lock()
                    .expect("session/new id lock should remain usable") = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_wait_for_response_line_with_timeout()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|_, response_timeout| *response_timeout == app_server_transport::TURN_TIMEOUT)
        .returning(move |_, _| {
            let response_id = request_id
                .lock()
                .expect("session/new id lock should remain usable")
                .clone()
                .expect("session/new id should be captured");
            let response = NewSessionResponse::new("gemini-review-session");

            Box::pin(async move {
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": response_id,
                    "result": response,
                })
                .to_string())
            })
        });

    // Act
    let session_id = start_session(
        &mut transport,
        folder.path(),
        app_server_transport::TURN_TIMEOUT,
    )
    .await;

    // Assert
    assert_eq!(
        session_id.expect("Gemini session creation should accept the long-running timeout"),
        "gemini-review-session"
    );
}

#[tokio::test]
async fn read_only_turn_cancels_permission_request_before_completing() {
    // Arrange
    let prompt_request_id = Arc::new(Mutex::new(None));
    let mut transport = MockAppServerRuntimeTransport::new();
    let mut sequence = Sequence::new();
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some(AGENT_METHOD_NAMES.session_prompt)
        })
        .returning({
            let prompt_request_id = Arc::clone(&prompt_request_id);

            move |payload| {
                *prompt_request_id
                    .lock()
                    .expect("prompt request id lock should remain usable") = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_next_stdout()
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(|| {
            Box::pin(async {
                Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": "permission-1",
                        "method": CLIENT_METHOD_NAMES.session_request_permission,
                        "params": {
                            "sessionId": "session-1",
                            "toolCall": {"toolCallId": "tool-1"},
                            "options": [{
                                "optionId": "allow-once",
                                "name": "Allow once",
                                "kind": "allow_once"
                            }]
                        }
                    })
                    .to_string(),
                ))
            })
        });
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|payload| {
            payload.get("id") == Some(&Value::String("permission-1".to_string()))
                && payload.pointer("/result/outcome/outcome")
                    == Some(&Value::String("cancelled".to_string()))
        })
        .return_once(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_next_stdout()
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(move || {
            let response_id = prompt_request_id
                .lock()
                .expect("prompt request id lock should remain usable")
                .clone()
                .expect("prompt request id should be captured");

            Box::pin(async move {
                Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": response_id,
                        "result": {
                            "response": "Research complete",
                            "usage": {"inputTokens": 7, "outputTokens": 3}
                        }
                    })
                    .to_string(),
                ))
            })
        });
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = run_turn_with_runtime(
        &mut transport,
        "session-1",
        PermissionMode::ReadOnly,
        "Inspect the architecture",
        ProtocolRequestProfile::SessionTurn,
        stream_tx,
    )
    .await;

    // Assert
    assert_eq!(
        result.expect("turn should complete after denying mutation"),
        ("Research complete".to_string(), 7, 3)
    );
}
