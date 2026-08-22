//! Antigravity persistent NDJSON runtime lifecycle.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use ag_protocol::TurnPrompt;
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
mod tests {
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::sync::{Arc, Mutex};

    use ag_protocol::TurnPromptAttachment;
    use mockall::Sequence;
    use tempfile::tempdir;

    use super::super::super::stdio_transport::MockAppServerRuntimeTransport;
    use super::*;
    use crate::MockAgentBackend;
    use crate::agent::AgentBackendError;
    use crate::model::agent::AgentModel;
    use crate::model::session::SpeedMode;

    fn request(folder: PathBuf) -> AppServerTurnRequest {
        AppServerTurnRequest {
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
        let (mut child, mut transport, state) = start_runtime_with_backend(&request, &backend)
            .expect("mock backend command should start");

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
}
