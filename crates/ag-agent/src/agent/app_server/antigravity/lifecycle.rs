//! Antigravity persistent NDJSON runtime lifecycle.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use tokio::sync::mpsc;

use super::super::stdio_transport::{AppServerRuntimeTransport, AppServerStdioTransport};
use super::stream_parser;
use super::usage::{TokenUsage, TurnUsageTracker};
use crate::agent::prompt::{
    CliPromptAccessRootMode, cli_prompt_access_directories, render_prompt_with_local_images,
};
use crate::app_server::{AppServerError, AppServerStreamEvent, AppServerTurnRequest};
use crate::model::agent::{AgentKind, ReasoningLevel};
use crate::model::permission::PermissionMode;
use crate::{agent, app_server_transport};

/// Mutable runtime state retained across Antigravity turns.
pub(super) struct AntigravityRuntimeState {
    access_directories: Vec<PathBuf>,
    conversation_id: Option<String>,
    folder: PathBuf,
    model: String,
    permission_mode: PermissionMode,
    previous_cumulative_usage: Option<TokenUsage>,
    protocol_profile: ProtocolRequestProfile,
    reasoning_level: ReasoningLevel,
    restored_context: bool,
}

impl AntigravityRuntimeState {
    /// Creates runtime state matching one launch request.
    pub(super) fn new(request: &AppServerTurnRequest) -> Self {
        Self {
            access_directories: prompt_access_directories(&request.folder, &request.prompt),
            conversation_id: request.provider_conversation_id.clone(),
            folder: request.folder.clone(),
            model: request.model.clone(),
            permission_mode: request.permission_mode,
            previous_cumulative_usage: None,
            protocol_profile: request.request_kind.protocol_profile(),
            reasoning_level: request.reasoning_level,
            restored_context: request.provider_conversation_id.is_some(),
        }
    }

    /// Returns whether the live process can serve the incoming request.
    pub(super) fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        let required_directories = prompt_access_directories(&request.folder, &request.prompt);

        self.folder == request.folder
            && self.model == request.model
            && self.permission_mode == request.permission_mode
            && self.protocol_profile == request.request_kind.protocol_profile()
            && self.reasoning_level == request.reasoning_level
            && required_directories
                .iter()
                .all(|directory| self.access_directories.contains(directory))
    }

    /// Returns the active native conversation id.
    pub(super) fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    /// Returns whether startup requested a provider-native resume.
    pub(super) fn restored_context(&self) -> bool {
        self.restored_context
    }

    fn observe_conversation_id(&mut self, conversation_id: Option<&str>) {
        if let Some(conversation_id) = conversation_id {
            self.conversation_id = Some(conversation_id.to_string());
        }
    }
}

/// Starts one `agy` streaming-input runtime without consuming its initial
/// event, which is emitted only after the first prompt arrives.
pub(super) fn start_runtime(
    request: &AppServerTurnRequest,
) -> Result<
    (
        app_server_transport::AppServerRuntimeChild,
        AppServerStdioTransport,
        AntigravityRuntimeState,
    ),
    AppServerError,
> {
    let backend = agent::create_backend(AgentKind::Antigravity);

    start_runtime_with_backend(request, backend.as_ref())
}

fn start_runtime_with_backend(
    request: &AppServerTurnRequest,
    backend: &dyn agent::AgentBackend,
) -> Result<
    (
        app_server_transport::AppServerRuntimeChild,
        AppServerStdioTransport,
        AntigravityRuntimeState,
    ),
    AppServerError,
> {
    let prompt_text = request.prompt.agent_text();
    let command = backend
        .build_command(agent::BuildCommandRequest {
            attachments: &request.prompt.attachments,
            folder: &request.folder,
            main_checkout_root: request.main_checkout_root.as_deref(),
            replay_transcript: None,
            model: &request.model,
            permission_mode: request.permission_mode,
            personality_prompt: None,
            prompt: &prompt_text,
            reasoning_level: request.reasoning_level,
            request_kind: &request.request_kind,
            speed_mode: request.speed_mode,
        })
        .map_err(|error| {
            AppServerError::Provider(format!(
                "Failed to build Antigravity runtime command: {error}"
            ))
        })?;

    start_runtime_with_built_command(command, request)
}

/// Starts one pre-built Antigravity command and constructs its retained
/// streaming runtime around the child stdio handles.
fn start_runtime_with_built_command(
    mut command: Command,
    request: &AppServerTurnRequest,
) -> Result<
    (
        app_server_transport::AppServerRuntimeChild,
        AppServerStdioTransport,
        AntigravityRuntimeState,
    ),
    AppServerError,
> {
    if let Some(conversation_id) = request.provider_conversation_id.as_deref() {
        append_conversation_argument(&mut command, conversation_id);
    }
    let (child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "agy stream-json")?;
    let transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "Antigravity stdin is unavailable",
        "Failed reading Antigravity stdout",
    );

    Ok((child, transport, AntigravityRuntimeState::new(request)))
}

fn append_conversation_argument(command: &mut Command, conversation_id: &str) {
    command.arg("--conversation").arg(conversation_id);
}

/// Sends one user event and waits for that turn's terminal result event.
pub(super) async fn run_turn_with_runtime<Transport: AppServerRuntimeTransport>(
    transport: &mut Transport,
    state: &mut AntigravityRuntimeState,
    prompt: &TurnPrompt,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
) -> Result<(String, u64, u64), AppServerError> {
    run_turn_with_timeout(
        transport,
        state,
        prompt,
        stream_tx,
        app_server_transport::TURN_TIMEOUT,
    )
    .await
}

async fn run_turn_with_timeout<Transport: AppServerRuntimeTransport>(
    transport: &mut Transport,
    state: &mut AntigravityRuntimeState,
    prompt: &TurnPrompt,
    stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    turn_timeout: Duration,
) -> Result<(String, u64, u64), AppServerError> {
    let prompt_text =
        render_prompt_with_local_images(&prompt.text, &prompt.attachments, "Antigravity")
            .map_err(|error| AppServerError::PromptRender(error.to_string()))?;
    transport
        .write_json_line(serde_json::json!({
            "event": "user",
            "message": {"content": prompt_text},
        }))
        .await?;

    tokio::time::timeout(turn_timeout, async {
        let mut usage_tracker = TurnUsageTracker::default();
        loop {
            let stdout_line = transport.next_stdout().await?.ok_or_else(|| {
                AppServerError::Provider(
                    "Antigravity terminated before emitting a turn result".to_string(),
                )
            })?;
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&stdout_line) else {
                continue;
            };
            state.observe_conversation_id(stream_parser::conversation_id(&payload));

            if let Some(step_update) = stream_parser::step_update(&payload) {
                usage_tracker.record_step(step_update);
                if let Some(event) = stream_parser::stream_event(step_update) {
                    let _ = stream_tx.send(event);
                }

                continue;
            }
            let Some(result) = stream_parser::result(&payload) else {
                continue;
            };
            if !stream_parser::result_succeeded(result) {
                let error = stream_parser::result_error(result)
                    .unwrap_or("Antigravity returned an unsuccessful turn result");

                return Err(AppServerError::Provider(error.to_string()));
            }
            let assistant_message = stream_parser::result_response(result).ok_or_else(|| {
                AppServerError::Provider(
                    "Antigravity result did not contain a response".to_string(),
                )
            })?;
            let usage = usage_tracker.finish(result, &mut state.previous_cumulative_usage);

            return Ok((assistant_message, usage.input_tokens, usage.output_tokens));
        }
    })
    .await
    .map_err(|_| {
        AppServerError::Provider(format!(
            "Timed out waiting for Antigravity turn completion after {} seconds",
            turn_timeout.as_secs()
        ))
    })?
}

fn prompt_access_directories(folder: &std::path::Path, prompt: &TurnPrompt) -> Vec<PathBuf> {
    cli_prompt_access_directories(
        folder,
        &prompt.attachments,
        CliPromptAccessRootMode::WorkspaceThenAttachments,
    )
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
