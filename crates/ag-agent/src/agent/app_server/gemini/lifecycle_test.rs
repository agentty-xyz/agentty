use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ag_contracts::{PermissionMode, ReasoningLevel, SpeedMode};
use ag_protocol::{ProtocolRequestProfile, TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use ag_session::AgentModel;
use ag_telemetry::Span;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    AGENT_METHOD_NAMES, CLIENT_METHOD_NAMES, InitializeResponse, NewSessionResponse,
};
use mockall::Sequence;
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::mpsc;

use crate::agent::app_server::gemini::lifecycle::{
    bootstrap_response_timeout, bootstrap_runtime_session, build_prompt_content_blocks,
    initialize_runtime, parse_session_new_response, prompt_image_mime_type,
    push_text_content_block, run_turn_with_runtime, start_runtime,
    start_runtime_with_built_command, start_session, stream_assistant_chunk,
};
use crate::agent::app_server::stdio_transport::MockAppServerRuntimeTransport;
use crate::app_server::{AppServerError, AppServerStreamEvent, AppServerTurnRequest};
use crate::app_server_transport::{self, AppServerTransportError};
use crate::telemetry::TRACER_PROVIDER_LOCK;

#[tokio::test]
async fn gemini_transport_traces_tool_lifecycle_for_the_current_session() {
    // Arrange
    let _guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let prompt_id = Arc::new(Mutex::new(String::new()));
    let mut transport = MockAppServerRuntimeTransport::new();
    let written_id = Arc::clone(&prompt_id);
    transport
        .expect_write_json_line()
        .once()
        .returning(move |payload| {
            *written_id.lock().expect("id") =
                payload["id"].as_str().expect("prompt id").to_string();

            Box::pin(async { Ok(()) })
        });
    let updates = Arc::new(Mutex::new(vec![
        ("session-1", "completed"),
        ("session-1", "in_progress"),
        ("other-session", "in_progress"),
    ]));
    transport.expect_next_stdout().times(4).returning(move || {
        let line = if let Some((session, status)) = updates.lock().expect("updates").pop() {
            serde_json::json!({"method": "session/update", "params": {"sessionId": session, "update": {
                "sessionUpdate": "tool_call_update", "toolCallId": "call-1", "status": status, "kind": "execute", "title": "private title", "rawInput":{"command":"pwd"}, "rawOutput":"project",
            }}})
        } else {
            serde_json::json!({"id": prompt_id.lock().expect("id").clone(), "result": {"response": "done"}})
        }.to_string();

        Box::pin(async move { Ok(Some(line)) })
    });
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = Span::root("agent.attempt", Vec::new())
        .scope(run_turn_with_runtime(
            &mut transport,
            "session-1",
            PermissionMode::ReadOnly,
            "prompt",
            ProtocolRequestProfile::SessionTurn,
            stream_tx,
        ))
        .await
        .expect("turn");

    // Assert
    assert_eq!(result.0, "done");
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 2);
    let attempt = spans
        .iter()
        .find(|span| span.name == "agent.attempt")
        .expect("attempt");
    let tool = spans
        .iter()
        .find(|span| span.name == "agent.tool")
        .expect("tool");
    assert_eq!(tool.parent_span_id, attempt.span_context.span_id());
    assert!(
        tool.attributes
            .contains(&KeyValue::new("agentty.outcome", "completed"))
    );
    assert!(!format!("{spans:?}").contains("private"));
    let activity = std::iter::from_fn(|| stream_rx.try_recv().ok())
        .filter_map(|event| match event {
            AppServerStreamEvent::Activity(event) => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activity.len(), 2);
    assert_eq!(activity[0].input.as_deref(), Some("pwd"));
    assert_eq!(activity[1].output.as_deref(), Some("project"));
    assert_eq!(activity[1].status, ag_contracts::ActivityStatus::Completed);
    assert_eq!(activity[0].attempt_id, activity[1].attempt_id);
}

fn turn_request(folder: PathBuf, permission_mode: PermissionMode) -> AppServerTurnRequest {
    AppServerTurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        provider_call_budget: None,
        folder,
        live_transcript: None,
        main_checkout_root: None,
        model: AgentModel::Gemini31Pro.as_str().to_string(),
        permission_mode,
        persisted_instruction_conversation_id: None,
        agent_profile: ag_contracts::AgentDefinitionPrompt::default(),
        prompt: TurnPrompt::from("Inspect the architecture"),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::High,
        replay_transcript: None,
        request_kind: ag_contracts::AgentRequestKind::SessionStart,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::Normal,
    }
}

#[test]
fn utility_prompts_receive_long_running_bootstrap_timeout() {
    // Arrange
    let utility_request_kind = ag_contracts::AgentRequestKind::UtilityPrompt;
    let session_request_kind = ag_contracts::AgentRequestKind::SessionStart;

    // Act
    let utility_timeout = bootstrap_response_timeout(&utility_request_kind);
    let session_timeout = bootstrap_response_timeout(&session_request_kind);
    let metadata_timeout =
        bootstrap_response_timeout(&ag_contracts::AgentRequestKind::ReviewMetadata);

    // Assert
    assert_eq!(utility_timeout, app_server_transport::TURN_TIMEOUT);
    assert_eq!(metadata_timeout, app_server_transport::TURN_TIMEOUT);
    assert_eq!(session_timeout, app_server_transport::STARTUP_TIMEOUT);
}

#[test]
fn prompt_image_mime_type_uses_file_extension() {
    // Arrange
    let paths = ["image.GIF", "image.jpg", "image.webp", "image.bmp", "image"];

    // Act
    let mime_types = paths.map(Path::new).map(prompt_image_mime_type);

    // Assert
    assert_eq!(
        mime_types,
        [
            "image/gif",
            "image/jpeg",
            "image/webp",
            "image/png",
            "image/png"
        ]
    );
}

#[tokio::test]
async fn prompt_content_blocks_interleave_text_with_inline_images() {
    // Arrange
    let folder = tempdir().expect("create image folder");
    let image_path = folder.path().join("screenshot.png");
    std::fs::write(&image_path, [1, 2, 3]).expect("write prompt image");
    let prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            local_image_path: image_path,
            placeholder: "[Image #1]".to_string(),
        }],
        text: "Compare [Image #1] now".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let content_blocks = build_prompt_content_blocks(&prompt)
        .await
        .expect("prompt image should be readable");

    // Assert
    let content_blocks =
        serde_json::to_value(content_blocks).expect("content blocks should serialize");
    assert_eq!(content_blocks[0]["text"], "Compare ");
    assert_eq!(content_blocks[1]["type"], "image");
    assert_eq!(content_blocks[1]["data"], "AQID");
    assert_eq!(content_blocks[1]["mimeType"], "image/png");
    assert_eq!(content_blocks[2]["text"], " now");
    assert_eq!(content_blocks.as_array().map(Vec::len), Some(3));
}

#[tokio::test]
async fn prompt_content_blocks_report_unreadable_images() {
    // Arrange
    let folder = tempdir().expect("create image folder");
    let prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            local_image_path: folder.path().join("missing.png"),
            placeholder: "[Image #1]".to_string(),
        }],
        text: "[Image #1]".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let result = build_prompt_content_blocks(&prompt).await;

    // Assert
    assert!(matches!(
        result,
        Err(AppServerError::Provider(message))
            if message.starts_with("Failed to read Gemini prompt image")
                && message.contains("missing.png")
    ));
}

#[test]
fn empty_fragments_are_neither_sent_nor_streamed() {
    // Arrange
    let mut content_blocks = Vec::new();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    // Act
    push_text_content_block(&mut content_blocks, "");
    stream_assistant_chunk(&stream_tx, String::new());

    // Assert
    assert_eq!(content_blocks.len(), 0);
    assert!(stream_rx.try_recv().is_err());
}

#[tokio::test]
async fn initialize_runtime_rejects_unusable_responses() {
    for (response_line, expected_message) in [
        ("not json", "Failed to parse Gemini ACP initialize response"),
        (
            r#"{"error":{"message":"Authentication required"}}"#,
            "Authentication required",
        ),
        (
            r#"{"error":{"code":-32603}}"#,
            "Gemini ACP returned an error for `initialize`",
        ),
        (
            r#"{"result":{}}"#,
            "Failed to parse Gemini ACP ``initialize`` result",
        ),
    ] {
        // Arrange
        let mut transport = MockAppServerRuntimeTransport::new();
        transport
            .expect_write_json_line()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_wait_for_response_line_with_timeout()
            .times(1)
            .returning(move |_, _| Box::pin(async move { Ok(response_line.to_string()) }));

        // Act
        let result =
            initialize_runtime(&mut transport, app_server_transport::STARTUP_TIMEOUT).await;

        // Assert
        assert!(
            matches!(
                &result,
                Err(AppServerError::Provider(message)) if message.starts_with(expected_message)
            ),
            "unexpected initialize result for {response_line}: {result:?}"
        );
    }
}

#[test]
fn session_new_response_rejections_are_normalized() {
    for (response, expected_message) in [
        (
            serde_json::json!({"error": {"message": "Workspace is not trusted"}}),
            "Workspace is not trusted",
        ),
        (
            serde_json::json!({"error": {"code": -32603}}),
            "Gemini ACP returned an error for `session/new`",
        ),
        (
            serde_json::json!({"result": {}}),
            "Gemini ACP `session/new` response missing `sessionId`",
        ),
        (
            serde_json::json!({"result": {"sessionId": 7}}),
            "Failed to parse Gemini ACP ``session/new`` result",
        ),
    ] {
        // Act
        let result = parse_session_new_response(&response);

        // Assert
        assert!(
            matches!(
                &result,
                Err(AppServerError::Provider(message)) if message.starts_with(expected_message)
            ),
            "unexpected session/new result for {response}: {result:?}"
        );
    }
}

#[tokio::test]
async fn start_session_rejects_non_json_response() {
    // Arrange
    let folder = tempdir().expect("create session folder");
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    transport
        .expect_wait_for_response_line_with_timeout()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("not json".to_string()) }));

    // Act
    let result = start_session(
        &mut transport,
        folder.path(),
        app_server_transport::STARTUP_TIMEOUT,
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(AppServerError::Provider(message))
            if message.starts_with("Failed to parse session/new response JSON")
    ));
}

#[tokio::test]
async fn start_session_rejects_unserializable_folder_before_writing() {
    // Arrange
    let folder = PathBuf::from(OsStr::from_bytes(b"/tmp/agentty-\xff"));
    let mut transport = MockAppServerRuntimeTransport::new();

    // Act
    let result = start_session(
        &mut transport,
        &folder,
        app_server_transport::STARTUP_TIMEOUT,
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(AppServerError::Provider(message))
            if message.starts_with("Failed to serialize `session/new` request params")
    ));
}

/// Scripts one prompt turn whose stdout replays `stdout_lines`, replacing
/// `{prompt_id}` with the written prompt request id; `None` ends stdout.
fn scripted_prompt_transport(
    stdout_lines: Vec<Option<&'static str>>,
) -> MockAppServerRuntimeTransport {
    let prompt_id = Arc::new(Mutex::new(String::new()));
    let mut transport = MockAppServerRuntimeTransport::new();
    let written_id = Arc::clone(&prompt_id);
    transport
        .expect_write_json_line()
        .times(1)
        .returning(move |payload| {
            *written_id.lock().expect("prompt id lock") =
                payload["id"].as_str().expect("prompt id").to_string();

            Box::pin(async { Ok(()) })
        });
    let mut stdout_lines = stdout_lines.into_iter();
    transport
        .expect_next_stdout()
        .times(stdout_lines.len())
        .returning(move || {
            let line = stdout_lines
                .next()
                .expect("scripted stdout line")
                .map(|line| {
                    line.replace("{prompt_id}", &prompt_id.lock().expect("prompt id lock"))
                });

            Box::pin(async move { Ok(line) })
        });

    transport
}

#[tokio::test]
async fn turn_streams_session_chunks_and_skips_unparsable_lines() {
    // Arrange
    let mut transport = scripted_prompt_transport(vec![
        Some("   "),
        Some("not json"),
        Some(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_thought_chunk"}}}"#,
        ),
        Some(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello"}}}}"#,
        ),
        Some(r#"{"id":"{prompt_id}","result":{"stopReason":"end_turn"}}"#),
    ]);
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = run_turn_with_runtime(
        &mut transport,
        "session-1",
        PermissionMode::AutoEdit,
        "Say hello",
        ProtocolRequestProfile::SessionTurn,
        stream_tx,
    )
    .await;

    // Assert
    assert_eq!(
        result.expect("turn should complete"),
        ("Hello".to_string(), 0, 0)
    );
    let events = std::iter::from_fn(|| stream_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.contains(&AppServerStreamEvent::ProgressUpdate(
        "Thinking".to_string()
    )));
    assert!(events.contains(&AppServerStreamEvent::AssistantMessage {
        is_delta: true,
        message: "Hello".to_string(),
        phase: None,
    }));
}

#[tokio::test]
async fn turn_reports_provider_and_process_failures() {
    for (stdout_line, expected_message) in [
        (
            None,
            "Gemini ACP terminated before prompt completion response",
        ),
        (
            Some(r#"{"id":"{prompt_id}","error":{"message":"Quota exceeded"}}"#),
            "Quota exceeded",
        ),
        (
            Some(r#"{"id":"{prompt_id}","error":{"code":-32603}}"#),
            "Gemini ACP returned an error for `session/prompt`",
        ),
    ] {
        // Arrange
        let mut transport = scripted_prompt_transport(vec![stdout_line]);
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

        // Act
        let result = run_turn_with_runtime(
            &mut transport,
            "session-1",
            PermissionMode::AutoEdit,
            "Do work",
            ProtocolRequestProfile::SessionTurn,
            stream_tx,
        )
        .await;

        // Assert
        assert!(
            matches!(
                &result,
                Err(AppServerError::Provider(message)) if message == expected_message
            ),
            "unexpected turn result for {stdout_line:?}: {result:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn turn_times_out_when_completion_never_arrives() {
    // Arrange
    let mut transport = MockAppServerRuntimeTransport::new();
    transport
        .expect_write_json_line()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    transport.expect_next_stdout().times(1).returning(|| {
        Box::pin(std::future::pending::<
            Result<Option<String>, AppServerTransportError>,
        >())
    });
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = run_turn_with_runtime(
        &mut transport,
        "session-1",
        PermissionMode::AutoEdit,
        "Do work",
        ProtocolRequestProfile::SessionTurn,
        stream_tx,
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(AppServerError::Provider(message))
            if message == "Timed out waiting for Gemini ACP prompt completion after 14400 seconds"
    ));
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
                && payload.pointer("/params/clientCapabilities") == Some(&serde_json::json!({}))
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
