//! Gemini ACP lifecycle and turn orchestration.

use std::path::{Path, PathBuf};

use ag_protocol::{
    ProtocolRequestProfile, TurnPrompt, TurnPromptAttachment, TurnPromptContentPart,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AGENT_METHOD_NAMES, ContentBlock, ImageContent, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, TextContent,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::super::stdio_transport::{AppServerRuntimeTransport, AppServerStdioTransport};
use super::{policy, stream_parser, usage};
use crate::agent;
use crate::app_server::{AppServerError, AppServerStreamEvent, AppServerTurnRequest};
use crate::app_server_transport::{self, extract_json_error_message, response_id_matches};
use crate::model::agent::AgentKind;
use crate::model::permission::PermissionMode;

/// Mutable runtime state required while a Gemini ACP process is active.
pub(super) struct GeminiRuntimeState {
    /// Session worktree folder used as the runtime cwd.
    pub(super) folder: PathBuf,
    /// Selected Gemini model identifier.
    pub(super) model: String,
    /// Provider permission policy enforced for this runtime.
    pub(super) permission_mode: PermissionMode,
    /// Whether startup restored provider-native context.
    pub(super) restored_context: bool,
    /// Active provider-native session identifier.
    pub(super) session_id: String,
}

impl GeminiRuntimeState {
    /// Creates runtime state for one pending Gemini bootstrap.
    pub(super) fn new(folder: PathBuf, model: String, permission_mode: PermissionMode) -> Self {
        Self {
            folder,
            model,
            permission_mode,
            restored_context: false,
            session_id: String::new(),
        }
    }
}

/// Starts one Gemini ACP runtime, initializes it, and creates a session.
pub(super) async fn start_runtime(
    request: &AppServerTurnRequest,
) -> Result<
    (
        app_server_transport::AppServerRuntimeChild,
        AppServerStdioTransport,
        GeminiRuntimeState,
    ),
    AppServerError,
> {
    let command = agent::create_backend(AgentKind::Gemini)
        .build_command(agent::BuildCommandRequest {
            attachments: &[],
            folder: request.folder.as_path(),
            main_checkout_root: request.main_checkout_root.as_deref(),
            replay_transcript: None,
            model: &request.model,
            permission_mode: request.permission_mode,
            personality_prompt: None,
            prompt: "",
            reasoning_level: request.reasoning_level,
            request_kind: &request.request_kind,
            speed_mode: request.speed_mode,
        })
        .map_err(|error| {
            AppServerError::Provider(format!("Failed to build `gemini --acp` command: {error}"))
        })?;

    start_runtime_with_built_command(command, request).await
}

/// Starts one pre-built Gemini ACP command and bootstraps the session runtime
/// around its stdio streams.
pub(super) async fn start_runtime_with_built_command(
    command: std::process::Command,
    request: &AppServerTurnRequest,
) -> Result<
    (
        app_server_transport::AppServerRuntimeChild,
        AppServerStdioTransport,
        GeminiRuntimeState,
    ),
    AppServerError,
> {
    let (mut child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "gemini --acp")?;
    let mut transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "Gemini ACP stdin is unavailable",
        "Failed reading Gemini ACP stdout",
    );
    let mut state = GeminiRuntimeState::new(
        request.folder.clone(),
        request.model.clone(),
        request.permission_mode,
    );

    let bootstrap_timeout = bootstrap_response_timeout(&request.request_kind);
    match bootstrap_runtime_session(&mut transport, state.folder.as_path(), bootstrap_timeout).await
    {
        Ok(session_id) => {
            state.session_id = session_id;

            Ok((child, transport, state))
        }
        Err(error) => {
            transport.close_stdin();
            app_server_transport::shutdown_child(&mut child).await;

            Err(error)
        }
    }
}

/// Completes ACP bootstrap by sending `initialize` and creating
/// `session/new`.
pub(super) async fn bootstrap_runtime_session<Transport: AppServerRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
    response_timeout: std::time::Duration,
) -> Result<String, AppServerError> {
    initialize_runtime(transport, response_timeout).await?;

    start_session(transport, folder, response_timeout).await
}

/// Selects a long-running bootstrap deadline for isolated utility prompts.
///
/// Gemini initializes plan-mode tools and creates a fresh ACP session before
/// one-shot focused review can submit its prompt. Normal persistent sessions
/// retain the bounded startup timeout so unrelated configuration failures are
/// still reported promptly.
fn bootstrap_response_timeout(
    request_kind: &crate::channel::AgentRequestKind,
) -> std::time::Duration {
    if matches!(
        request_kind,
        crate::channel::AgentRequestKind::FocusedReview
            | crate::channel::AgentRequestKind::UtilityPrompt
    ) {
        app_server_transport::TURN_TIMEOUT
    } else {
        app_server_transport::STARTUP_TIMEOUT
    }
}

/// Sends the ACP initialize handshake.
pub(super) async fn initialize_runtime<Transport: AppServerRuntimeTransport>(
    transport: &mut Transport,
    response_timeout: std::time::Duration,
) -> Result<(), AppServerError> {
    let initialization_request_id = format!("init-{}", uuid::Uuid::new_v4());
    let initialization_request = build_initialize_request_payload(&initialization_request_id)?;
    transport.write_json_line(initialization_request).await?;
    let initialize_response_line = transport
        .wait_for_response_line_with_timeout(initialization_request_id, response_timeout)
        .await?;
    let initialize_response =
        serde_json::from_str::<Value>(&initialize_response_line).map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to parse Gemini ACP initialize response: {error}"
            ))
        })?;
    if initialize_response.get("error").is_some() {
        return Err(AppServerError::Provider(
            extract_json_error_message(&initialize_response)
                .unwrap_or_else(|| "Gemini ACP returned an error for `initialize`".to_string()),
        ));
    }
    parse_json_rpc_result::<InitializeResponse>(&initialize_response, "`initialize`")?;

    let initialized_notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized"
    });
    transport.write_json_line(initialized_notification).await?;

    Ok(())
}

/// Builds a typed ACP `initialize` request with conservative client
/// capabilities.
pub(super) fn build_initialize_request_payload(request_id: &str) -> Result<Value, AppServerError> {
    let initialize_params = InitializeRequest::new(ProtocolVersion::LATEST);
    let mut initialize_payload = build_json_rpc_request_payload(
        request_id,
        AGENT_METHOD_NAMES.initialize,
        initialize_params,
    )?;
    let Some(params) = initialize_payload.get_mut("params") else {
        return Err(AppServerError::Provider(
            "Failed to build Gemini ACP `initialize` request params".to_string(),
        ));
    };
    let Some(params) = params.as_object_mut() else {
        return Err(AppServerError::Provider(
            "Failed to build Gemini ACP `initialize` request params object".to_string(),
        ));
    };
    params.insert(
        "clientCapabilities".to_string(),
        Value::Object(serde_json::Map::new()),
    );

    Ok(initialize_payload)
}

/// Builds a typed JSON-RPC request payload.
pub(super) fn build_json_rpc_request_payload<T: Serialize>(
    request_id: &str,
    method: &str,
    params: T,
) -> Result<Value, AppServerError> {
    let params_value = serde_json::to_value(params).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to serialize `{method}` request params: {error}"
        ))
    })?;

    Ok(serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params_value
    }))
}

/// Extracts one typed JSON-RPC `result` payload.
pub(super) fn parse_json_rpc_result<T: serde::de::DeserializeOwned>(
    response_value: &Value,
    method: &str,
) -> Result<T, AppServerError> {
    let result_value = response_value.get("result").cloned().ok_or_else(|| {
        AppServerError::Provider(format!("Gemini ACP `{method}` response missing `result`"))
    })?;

    serde_json::from_value::<T>(result_value).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse Gemini ACP `{method}` result: {error}"
        ))
    })
}

/// Creates one ACP session and returns the assigned `sessionId`.
pub(super) async fn start_session<Transport: AppServerRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
    response_timeout: std::time::Duration,
) -> Result<String, AppServerError> {
    let session_new_id = format!("session-new-{}", uuid::Uuid::new_v4());
    let session_new_payload = build_json_rpc_request_payload(
        &session_new_id,
        AGENT_METHOD_NAMES.session_new,
        NewSessionRequest::new(folder.to_path_buf()),
    )?;
    transport.write_json_line(session_new_payload).await?;
    let response_line = transport
        .wait_for_response_line_with_timeout(session_new_id, response_timeout)
        .await?;
    let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse session/new response JSON: {error}"
        ))
    })?;

    parse_session_new_response(&response_value)
}

/// Parses one ACP `session/new` response into a session identifier.
pub(super) fn parse_session_new_response(response_value: &Value) -> Result<String, AppServerError> {
    if response_value.get("error").is_some() {
        return Err(AppServerError::Provider(
            extract_json_error_message(response_value)
                .unwrap_or_else(|| "Gemini ACP returned an error for `session/new`".to_string()),
        ));
    }

    let session_new_result =
        parse_json_rpc_result::<NewSessionResponse>(response_value, "`session/new`").map_err(
            |error| {
                let error_message = error.to_string();
                if error_message.contains("missing field `sessionId`") {
                    return AppServerError::Provider(
                        "Gemini ACP `session/new` response missing `sessionId`".to_string(),
                    );
                }

                error
            },
        )?;

    Ok(session_new_result.session_id.to_string())
}

/// Sends one prompt turn and waits for the matching prompt response id.
pub(super) async fn run_turn_with_runtime<Transport: AppServerRuntimeTransport>(
    transport: &mut Transport,
    session_id: &str,
    permission_mode: PermissionMode,
    prompt: impl Into<TurnPrompt>,
    protocol_profile: ProtocolRequestProfile,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
) -> Result<(String, u64, u64), AppServerError> {
    let prompt = prompt.into();
    let content_blocks = build_prompt_content_blocks(&prompt).await?;
    let prompt_id = format!("session-prompt-{}", uuid::Uuid::new_v4());
    let session_prompt_payload = build_json_rpc_request_payload(
        &prompt_id,
        AGENT_METHOD_NAMES.session_prompt,
        PromptRequest::new(session_id.to_string(), content_blocks),
    )?;
    transport.write_json_line(session_prompt_payload).await?;

    let mut assistant_message = String::new();
    tokio::time::timeout(app_server_transport::TURN_TIMEOUT, async {
        loop {
            let stdout_line = transport.next_stdout().await?.ok_or_else(|| {
                AppServerError::Provider(
                    "Gemini ACP terminated before prompt completion response".to_string(),
                )
            })?;

            if stdout_line.trim().is_empty() {
                continue;
            }

            let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                continue;
            };

            if let Some(permission_response) =
                policy::build_permission_response(&response_value, session_id, permission_mode)
            {
                transport.write_json_line(permission_response).await?;

                continue;
            }

            if response_id_matches(&response_value, &prompt_id) {
                if response_value.get("error").is_some() {
                    return Err(AppServerError::Provider(
                        extract_json_error_message(&response_value).unwrap_or_else(|| {
                            "Gemini ACP returned an error for `session/prompt`".to_string()
                        }),
                    ));
                }
                let prompt_completion = usage::parse_prompt_completion_response(&response_value)?;
                assistant_message = stream_parser::select_preferred_assistant_message(
                    &assistant_message,
                    prompt_completion.assistant_message.as_deref(),
                    protocol_profile,
                );

                return Ok((
                    assistant_message,
                    prompt_completion.input_tokens,
                    prompt_completion.output_tokens,
                ));
            }

            if let Some(progress) =
                stream_parser::extract_progress_update(&response_value, session_id)
            {
                let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
            }

            if let Some(chunk) =
                stream_parser::extract_assistant_message_chunk(&response_value, session_id)
            {
                assistant_message.push_str(chunk.as_str());
                stream_assistant_chunk(&stream_tx, chunk);
            }
        }
    })
    .await
    .map_err(|_| {
        AppServerError::Provider(format!(
            "Timed out waiting for Gemini ACP prompt completion after {} seconds",
            app_server_transport::TURN_TIMEOUT.as_secs()
        ))
    })?
}

/// Streams one non-empty assistant delta chunk to the UI.
pub(super) fn stream_assistant_chunk(
    stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    chunk: String,
) {
    if chunk.is_empty() {
        return;
    }

    let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
        is_delta: true,
        message: chunk,
        phase: None,
    });
}

/// Builds Gemini ACP content blocks for one structured prompt payload.
pub(super) async fn build_prompt_content_blocks(
    prompt: &TurnPrompt,
) -> Result<Vec<ContentBlock>, AppServerError> {
    let prompt = prompt.clone();

    tokio::task::spawn_blocking(move || build_prompt_content_blocks_blocking(&prompt))
        .await
        .map_err(|error| {
            AppServerError::Provider(format!("Gemini prompt-image task failed: {error}"))
        })?
}

/// Builds Gemini ACP content blocks for one prompt on a blocking worker
/// thread.
pub(super) fn build_prompt_content_blocks_blocking(
    prompt: &TurnPrompt,
) -> Result<Vec<ContentBlock>, AppServerError> {
    if !prompt.has_attachments() {
        return Ok(vec![ContentBlock::Text(TextContent::new(
            prompt.text.clone(),
        ))]);
    }

    let mut content_blocks = Vec::new();
    for content_part in prompt.content_parts() {
        match content_part {
            TurnPromptContentPart::Text(text) => {
                push_text_content_block(&mut content_blocks, text);
            }
            TurnPromptContentPart::Attachment(attachment)
            | TurnPromptContentPart::OrphanAttachment(attachment) => {
                content_blocks.push(build_image_content_block(attachment)?);
            }
        }
    }

    Ok(content_blocks)
}

/// Appends one non-empty Gemini text content block.
pub(super) fn push_text_content_block(content_blocks: &mut Vec<ContentBlock>, text: &str) {
    if text.is_empty() {
        return;
    }

    content_blocks.push(ContentBlock::Text(TextContent::new(text.to_string())));
}

/// Builds one Gemini ACP image content block from a persisted local prompt
/// attachment.
pub(super) fn build_image_content_block(
    attachment: &TurnPromptAttachment,
) -> Result<ContentBlock, AppServerError> {
    let image_bytes = std::fs::read(&attachment.local_image_path).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to read Gemini prompt image `{}`: {error}",
            attachment.local_image_path.display()
        ))
    })?;
    let mime_type = prompt_image_mime_type(&attachment.local_image_path);

    Ok(ContentBlock::Image(ImageContent::new(
        BASE64_STANDARD.encode(image_bytes),
        mime_type,
    )))
}

/// Returns the MIME type Gemini should use for one persisted prompt image.
#[must_use]
pub(super) fn prompt_image_mime_type(local_image_path: &Path) -> &'static str {
    let Some(extension) = local_image_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
    else {
        return "image/png";
    };

    match extension.to_ascii_lowercase().as_str() {
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
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
                payload.get("method").and_then(Value::as_str)
                    == Some(AGENT_METHOD_NAMES.session_new)
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
                payload.get("method").and_then(Value::as_str)
                    == Some(AGENT_METHOD_NAMES.session_prompt)
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
}
