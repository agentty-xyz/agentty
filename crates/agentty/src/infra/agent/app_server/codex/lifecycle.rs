//! Codex app-server lifecycle and turn orchestration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::transport::{CodexRuntimeTransport, CodexStdioTransport};
use super::{policy, stream_parser, usage};
use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::infra::agent;
use crate::infra::agent::protocol::agent_response_output_schema;
use crate::infra::app_server::{AppServerError, AppServerStreamEvent, AppServerTurnRequest};
use crate::infra::app_server_transport::{self, extract_json_error_message, response_id_matches};
use crate::infra::channel::{TurnPrompt, TurnPromptContentPart};

/// Mutable runtime state required while a Codex app-server process is active.
pub(super) struct CodexRuntimeState {
    /// Session worktree folder used as the runtime cwd.
    pub(super) folder: PathBuf,
    /// Most recent input token count reported by the app-server.
    pub(super) latest_input_tokens: u64,
    /// Selected Codex model identifier.
    pub(super) model: String,
    /// Whether startup restored provider-native context.
    pub(super) restored_context: bool,
    /// Active provider-native thread identifier.
    pub(super) thread_id: String,
}

impl CodexRuntimeState {
    /// Creates runtime state for one pending session bootstrap.
    pub(super) fn new(folder: PathBuf, model: String) -> Self {
        Self {
            folder,
            latest_input_tokens: 0,
            model,
            restored_context: false,
            thread_id: String::new(),
        }
    }
}

/// Starts `codex app-server`, initializes it, and creates or resumes a
/// thread.
pub(super) async fn start_runtime(
    request: &AppServerTurnRequest,
) -> Result<
    (
        tokio::process::Child,
        CodexStdioTransport,
        CodexRuntimeState,
    ),
    AppServerError,
> {
    let request_kind = crate::infra::channel::AgentRequestKind::SessionStart;
    let command = agent::create_backend(AgentKind::Codex)
        .build_command(agent::BuildCommandRequest {
            attachments: &[],
            folder: request.folder.as_path(),
            prompt: "",
            request_kind: &request_kind,
            model: &request.model,
            reasoning_level: request.reasoning_level,
        })
        .map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to build `codex app-server` command: {error}"
            ))
        })?;

    start_runtime_with_built_command(command, request).await
}

/// Starts one pre-built Codex app-server command and bootstraps the session
/// runtime around its stdio streams.
pub(super) async fn start_runtime_with_built_command(
    command: std::process::Command,
    request: &AppServerTurnRequest,
) -> Result<
    (
        tokio::process::Child,
        CodexStdioTransport,
        CodexRuntimeState,
    ),
    AppServerError,
> {
    let (mut child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "codex app-server").await?;
    let mut transport = CodexStdioTransport::new(stdin, stdout);
    let mut state = CodexRuntimeState::new(request.folder.clone(), request.model.clone());

    let bootstrap_result = async {
        initialize_runtime(&mut transport).await?;
        start_or_resume_thread(
            &mut transport,
            &state.folder,
            &state.model,
            request.provider_conversation_id.as_deref(),
            request.reasoning_level,
        )
        .await
    }
    .await;

    match bootstrap_result {
        Ok((thread_id, restored_context)) => {
            state.thread_id = thread_id;
            state.restored_context = restored_context;

            Ok((child, transport, state))
        }
        Err(error) => {
            transport.close_stdin();
            app_server_transport::shutdown_child(&mut child).await;

            Err(error)
        }
    }
}

/// Sends the initialize handshake for one app-server process.
pub(super) async fn initialize_runtime<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
) -> Result<(), AppServerError> {
    let initialize_id = format!("init-{}", uuid::Uuid::new_v4());
    let initialize_payload = serde_json::json!({
        "method": "initialize",
        "id": initialize_id,
        "params": {
            "clientInfo": {
                "name": "agentty",
                "title": "agentty",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "experimentalApi": true,
                "optOutNotificationMethods": Value::Null
            }
        }
    });
    transport.write_json_line(initialize_payload).await?;
    transport.wait_for_response_line(initialize_id).await?;

    let runtime_initialized_payload = serde_json::json!({
        "method": "initialized",
        "params": {}
    });
    transport
        .write_json_line(runtime_initialized_payload)
        .await?;

    Ok(())
}

/// Restores a known thread when possible and otherwise starts a fresh thread.
///
/// Returns the active thread id plus a flag indicating whether provider
/// context was restored.
pub(super) async fn start_or_resume_thread<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
    model: &str,
    provider_conversation_id: Option<&str>,
    reasoning_level: ReasoningLevel,
) -> Result<(String, bool), AppServerError> {
    if let Some(provider_conversation_id) = provider_conversation_id
        && let Ok(thread_id) =
            resume_thread(transport, provider_conversation_id, model, reasoning_level).await
    {
        return Ok((thread_id, true));
    }

    let thread_id = start_thread(transport, folder, model, reasoning_level).await?;

    Ok((thread_id, false))
}

/// Starts one Codex thread and returns its identifier.
pub(super) async fn start_thread<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
    model: &str,
    reasoning_level: ReasoningLevel,
) -> Result<String, AppServerError> {
    let thread_start_id = format!("thread-start-{}", uuid::Uuid::new_v4());
    let thread_start_payload =
        build_thread_start_payload(folder, model, reasoning_level, &thread_start_id);

    transport.write_json_line(thread_start_payload).await?;
    let response_line = transport.wait_for_response_line(thread_start_id).await?;
    let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse thread/start response JSON: {error}"
        ))
    })?;

    response_value
        .get("result")
        .and_then(|result| result.get("thread"))
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            AppServerError::Provider(
                "Codex app-server `thread/start` response does not include a thread id".to_string(),
            )
        })
}

/// Resumes one existing Codex thread and returns the active identifier.
pub(super) async fn resume_thread<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    thread_id: &str,
    model: &str,
    reasoning_level: ReasoningLevel,
) -> Result<String, AppServerError> {
    let thread_resume_request_id = format!("thread-resume-{}", uuid::Uuid::new_v4());
    let thread_resume_payload =
        build_thread_resume_payload(&thread_resume_request_id, thread_id, model, reasoning_level);

    transport.write_json_line(thread_resume_payload).await?;
    let response_line = transport
        .wait_for_response_line(thread_resume_request_id)
        .await?;
    let response_value = serde_json::from_str::<Value>(&response_line).map_err(|error| {
        AppServerError::Provider(format!(
            "Failed to parse thread/resume response JSON: {error}"
        ))
    })?;

    response_value
        .get("result")
        .and_then(|result| result.get("thread"))
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            AppServerError::Provider(
                "Codex app-server `thread/resume` response does not include a thread id"
                    .to_string(),
            )
        })
}

/// Builds one `thread/start` request payload for a runtime folder.
pub(super) fn build_thread_start_payload(
    folder: &Path,
    model: &str,
    reasoning_level: ReasoningLevel,
    thread_start_id: &str,
) -> Value {
    serde_json::json!({
        "method": "thread/start",
        "id": thread_start_id,
        "params": {
            "model": model,
            "cwd": folder.to_string_lossy(),
            "approvalPolicy": policy::approval_policy(),
            "sandbox": policy::thread_sandbox_mode(),
            "config": policy::thread_config(reasoning_level),
            "experimentalRawEvents": false,
            "persistExtendedHistory": false
        }
    })
}

/// Builds one `thread/resume` request payload.
pub(super) fn build_thread_resume_payload(
    thread_resume_request_id: &str,
    thread_id: &str,
    model: &str,
    reasoning_level: ReasoningLevel,
) -> Value {
    serde_json::json!({
        "method": "thread/resume",
        "id": thread_resume_request_id,
        "params": {
            "threadId": thread_id,
            "model": model,
            "approvalPolicy": policy::approval_policy(),
            "sandbox": policy::thread_sandbox_mode(),
            "config": policy::thread_config(reasoning_level),
            "experimentalRawEvents": false,
            "persistExtendedHistory": false
        }
    })
}

/// Sends one turn prompt and waits for terminal completion notification.
pub(super) async fn run_turn_with_runtime<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    state: &mut CodexRuntimeState,
    prompt: impl Into<TurnPrompt>,
    reasoning_level: ReasoningLevel,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
) -> Result<(String, u64, u64), AppServerError> {
    let prompt = prompt.into();
    let auto_compact_threshold = policy::auto_compact_input_token_threshold(&state.model);

    if state.latest_input_tokens >= auto_compact_threshold {
        let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
            "Compacting context".to_string(),
        ));
        send_compact_request(transport, &state.thread_id, &mut state.latest_input_tokens).await?;
    }

    let result = execute_turn_event_loop(
        transport,
        &state.folder,
        &state.model,
        &state.thread_id,
        &prompt,
        reasoning_level,
        stream_tx.clone(),
    )
    .await;

    match result {
        Ok((message, input_tokens, output_tokens)) => {
            state.latest_input_tokens = input_tokens;

            Ok((message, input_tokens, output_tokens))
        }
        Err(ref error) if stream_parser::is_context_window_exceeded_error(&error.to_string()) => {
            let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                "Compacting context".to_string(),
            ));
            send_compact_request(transport, &state.thread_id, &mut state.latest_input_tokens)
                .await?;

            let (message, input_tokens, output_tokens) = execute_turn_event_loop(
                transport,
                &state.folder,
                &state.model,
                &state.thread_id,
                &prompt,
                reasoning_level,
                stream_tx,
            )
            .await?;
            state.latest_input_tokens = input_tokens;

            Ok((message, input_tokens, output_tokens))
        }
        Err(error) => Err(error),
    }
}

/// Sends `thread/compact/start` and waits for compaction to complete.
pub(super) async fn send_compact_request<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    thread_id: &str,
    latest_input_tokens: &mut u64,
) -> Result<(), AppServerError> {
    send_compact_request_with_timeout(
        transport,
        thread_id,
        latest_input_tokens,
        app_server_transport::TURN_TIMEOUT,
    )
    .await
}

/// Sends `thread/compact/start` and waits for compaction to complete within
/// one caller-provided timeout window.
pub(super) async fn send_compact_request_with_timeout<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    thread_id: &str,
    latest_input_tokens: &mut u64,
    turn_timeout: Duration,
) -> Result<(), AppServerError> {
    let compact_id = format!("compact-{}", uuid::Uuid::new_v4());
    let compact_payload = serde_json::json!({
        "method": "thread/compact/start",
        "id": compact_id,
        "params": {
            "threadId": thread_id
        }
    });

    transport.write_json_line(compact_payload).await?;
    transport.wait_for_response_line(compact_id).await?;

    tokio::time::timeout(turn_timeout, async {
        loop {
            let stdout_line = read_required_stdout_line(transport, " during compaction").await?;
            if stdout_line.trim().is_empty() {
                continue;
            }

            let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                continue;
            };

            if response_value.get("method").and_then(Value::as_str) == Some("turn/completed") {
                let status = response_value
                    .get("params")
                    .and_then(|params| params.get("turn"))
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");

                if status == "completed" {
                    *latest_input_tokens = 0;

                    return Ok(());
                }

                let error_message =
                    stream_parser::extract_turn_completed_error_message(&response_value)
                        .unwrap_or_else(|| "Compaction failed".to_string());

                return Err(AppServerError::Provider(format!(
                    "Codex context compaction failed: {error_message}"
                )));
            }
        }
    })
    .await
    .map_err(|_| compaction_timeout_error(turn_timeout))?
}

/// Sends one `turn/start` request and processes the event stream until
/// `turn/completed` is received.
pub(super) async fn execute_turn_event_loop<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
    model: &str,
    thread_id: &str,
    prompt: impl Into<TurnPrompt>,
    reasoning_level: ReasoningLevel,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
) -> Result<(String, u64, u64), AppServerError> {
    let prompt = prompt.into();
    execute_turn_event_loop_with_timeout(
        transport,
        folder,
        model,
        thread_id,
        &prompt,
        reasoning_level,
        stream_tx,
        app_server_transport::TURN_TIMEOUT,
    )
    .await
}

/// Sends one `turn/start` request and processes the event stream using a
/// caller-provided timeout window.
pub(super) async fn execute_turn_event_loop_with_timeout<Transport: CodexRuntimeTransport>(
    transport: &mut Transport,
    folder: &Path,
    model: &str,
    thread_id: &str,
    prompt: impl Into<TurnPrompt>,
    reasoning_level: ReasoningLevel,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    turn_timeout: Duration,
) -> Result<(String, u64, u64), AppServerError> {
    let prompt = prompt.into();
    let turn_start_id = format!("turn-start-{}", uuid::Uuid::new_v4());
    let turn_start_payload = build_turn_start_payload(
        folder,
        model,
        reasoning_level,
        thread_id,
        &prompt,
        &turn_start_id,
    );
    transport.write_json_line(turn_start_payload).await?;

    let mut assistant_messages = Vec::new();
    let mut active_turn_id: Option<String> = None;
    let mut active_phase: Option<String> = None;
    let mut waiting_for_handoff_turn_completion = false;
    let mut latest_stream_usage: Option<(u64, u64)> = None;
    let mut completed_turn_usage: Option<(u64, u64)> = None;
    tokio::time::timeout(turn_timeout, async {
        loop {
            let stdout_line =
                read_required_stdout_line(transport, " before `turn/completed` was received")
                    .await?;

            if stdout_line.trim().is_empty() {
                continue;
            }

            if let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) {
                if response_id_matches(&response_value, &turn_start_id) {
                    if response_value.get("error").is_some() {
                        return Err(AppServerError::Provider(
                            extract_json_error_message(&response_value).unwrap_or_else(|| {
                                "Codex app-server returned an error for `turn/start`".to_string()
                            }),
                        ));
                    }
                    if active_turn_id.is_none() {
                        active_turn_id = stream_parser::extract_turn_id_from_turn_start_response(
                            &response_value,
                        );
                        if active_turn_id.is_some() {
                            waiting_for_handoff_turn_completion = false;
                        }
                    }

                    continue;
                }

                if let Some(approval_response) =
                    policy::build_pre_action_approval_response(&response_value)
                {
                    transport.write_json_line(approval_response).await?;

                    continue;
                }

                update_active_turn_tracking_for_response(
                    &response_value,
                    &mut active_turn_id,
                    &mut waiting_for_handoff_turn_completion,
                );
                stream_turn_content_from_response(
                    &response_value,
                    &stream_tx,
                    &mut assistant_messages,
                    &mut active_phase,
                );
                usage::update_turn_usage_from_response(
                    &response_value,
                    active_turn_id.as_deref(),
                    &mut completed_turn_usage,
                    &mut latest_stream_usage,
                );

                if stream_parser::is_interrupted_turn_completion_without_error(
                    &response_value,
                    active_turn_id.as_deref(),
                ) {
                    active_turn_id = None;
                    waiting_for_handoff_turn_completion = true;

                    continue;
                }

                if let Some(turn_result) =
                    stream_parser::parse_turn_completed(&response_value, active_turn_id.as_deref())
                {
                    let (input_tokens, output_tokens) =
                        usage::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

                    return finalize_turn_completion(
                        turn_result,
                        &assistant_messages,
                        &stream_tx,
                        input_tokens,
                        output_tokens,
                    );
                }
            }
        }
    })
    .await
    .map_err(|_| turn_completed_timeout_error(turn_timeout))?
}

/// Builds one `turn/start` request payload for the active thread.
pub(super) fn build_turn_start_payload(
    folder: &Path,
    model: &str,
    reasoning_level: ReasoningLevel,
    thread_id: &str,
    prompt: impl Into<TurnPrompt>,
    turn_start_id: &str,
) -> Value {
    let prompt = prompt.into();

    serde_json::json!({
        "method": "turn/start",
        "id": turn_start_id,
        "params": {
            "threadId": thread_id,
            "input": build_turn_input_items(&prompt),
            "cwd": folder.to_string_lossy(),
            "approvalPolicy": policy::approval_policy(),
            "sandboxPolicy": policy::turn_sandbox_policy(),
            "model": model,
            "effort": reasoning_level.codex(),
            "summary": Value::Null,
            "personality": Value::Null,
            "outputSchema": agent_response_output_schema()
        }
    })
}

/// Builds ordered Codex `turn/start` input items from one structured prompt
/// payload.
pub(super) fn build_turn_input_items(prompt: &TurnPrompt) -> Vec<Value> {
    if !prompt.has_attachments() {
        return vec![serde_json::json!({
            "type": "text",
            "text": prompt.text,
            "text_elements": []
        })];
    }

    let mut input_items = Vec::new();
    for content_part in prompt.content_parts() {
        match content_part {
            TurnPromptContentPart::Text(text) => {
                if !text.is_empty() {
                    input_items.push(serde_json::json!({
                        "type": "text",
                        "text": text,
                        "text_elements": []
                    }));
                }
            }
            TurnPromptContentPart::Attachment(attachment)
            | TurnPromptContentPart::OrphanAttachment(attachment) => {
                input_items.push(build_local_image_input_item(&attachment.local_image_path));
            }
        }
    }

    input_items
}

/// Builds one Codex `localImage` input item using a plain filesystem path
/// string.
pub(super) fn build_local_image_input_item(local_image_path: &Path) -> Value {
    serde_json::json!({
        "type": "localImage",
        "path": local_image_path.to_string_lossy().into_owned()
    })
}

/// Builds a stable timeout error for compaction completion waits.
pub(super) fn compaction_timeout_error(turn_timeout: Duration) -> AppServerError {
    AppServerError::Provider(format!(
        "Timed out waiting for Codex app-server compaction to complete after {} seconds",
        turn_timeout.as_secs()
    ))
}

/// Builds a stable timeout error for turn completion waits.
pub(super) fn turn_completed_timeout_error(turn_timeout: Duration) -> AppServerError {
    AppServerError::Provider(format!(
        "Timed out waiting for Codex app-server `turn/completed` after {} seconds",
        turn_timeout.as_secs()
    ))
}

/// Updates active turn tracking from one response notification.
fn update_active_turn_tracking_for_response(
    response_value: &Value,
    active_turn_id: &mut Option<String>,
    waiting_for_handoff_turn_completion: &mut bool,
) {
    if active_turn_id.is_some() {
        return;
    }

    if let Some(turn_id) =
        stream_parser::extract_turn_id_from_turn_started_notification(response_value)
    {
        *active_turn_id = Some(turn_id);
        *waiting_for_handoff_turn_completion = false;

        return;
    }

    if let Some(turn_id) = stream_parser::extract_handoff_turn_id_from_completion(
        response_value,
        active_turn_id.as_deref(),
        *waiting_for_handoff_turn_completion,
    ) {
        *active_turn_id = Some(turn_id);
    }
}

/// Streams progress updates plus assistant delta/completed items from one
/// response.
fn stream_turn_content_from_response(
    response_value: &Value,
    stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    assistant_messages: &mut Vec<String>,
    active_phase: &mut Option<String>,
) {
    if let Some(progress) = stream_parser::extract_item_started_progress(response_value) {
        let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
    }

    if let Some(agent_message) = stream_parser::extract_agent_message_delta(response_value) {
        if let Some(phase) = agent_message.phase.as_deref() {
            emit_phase_progress_update(stream_tx, active_phase, phase);
        }

        let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
            is_delta: true,
            message: agent_message.message,
            phase: agent_message.phase,
        });
    }

    if let Some(agent_message) = stream_parser::extract_agent_message(response_value) {
        if let Some(phase) = agent_message.phase.as_deref() {
            emit_phase_progress_update(stream_tx, active_phase, phase);
        }

        let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
            is_delta: false,
            message: agent_message.message.clone(),
            phase: agent_message.phase.clone(),
        });
        assistant_messages.push(agent_message.message);
    }
}

/// Emits a phase progress event when an assistant item reports a new phase.
fn emit_phase_progress_update(
    stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    active_phase: &mut Option<String>,
    phase: &str,
) {
    if active_phase.as_deref() == Some(phase) {
        return;
    }

    *active_phase = Some(phase.to_string());
    let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(format!(
        "Phase: {phase}"
    )));
}

/// Finalizes one parsed `turn/completed` result into the normalized turn
/// response tuple.
fn finalize_turn_completion(
    turn_result: Result<(), String>,
    assistant_messages: &[String],
    stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    input_tokens: u64,
    output_tokens: u64,
) -> Result<(String, u64, u64), AppServerError> {
    match turn_result {
        Ok(()) => {
            let assistant_message =
                stream_parser::preferred_completed_assistant_message(assistant_messages);

            Ok((assistant_message, input_tokens, output_tokens))
        }
        Err(error) => {
            let streamed_error = format!("[Codex app-server] {error}");
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: streamed_error,
                phase: None,
            });

            Err(AppServerError::Provider(error))
        }
    }
}

/// Reads the next stdout line from the app-server runtime.
fn read_required_stdout_line<'scope, Transport: CodexRuntimeTransport>(
    transport: &'scope mut Transport,
    context: &'scope str,
) -> impl std::future::Future<Output = Result<String, AppServerError>> + Send + 'scope {
    async move {
        transport
            .next_stdout()
            .await
            .map_err(AppServerError::from)?
            .ok_or_else(|| {
                AppServerError::Provider(format!("Codex app-server terminated{context}"))
            })
    }
}
