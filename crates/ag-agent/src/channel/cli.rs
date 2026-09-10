//! CLI subprocess [`AgentChannel`] adapter.
//!
//! Spawns a provider CLI process per turn, streams stdout line-by-line as
//! [`TurnEvent`]s, and parses the final process output when the process exits.

use std::sync::Arc;

use ag_protocol::{AgentResponse, TurnPrompt, build_protocol_repair_prompt};
use tokio::sync::mpsc;

use crate::agent::cli::error;
use crate::agent::cli::execution::{
    self, CliExecutionError, CliExecutionObserver, CliExitStatus, CollectingCliObserver,
};
use crate::agent::{self as agent, AgentBackend, BuildCommandRequest};
use crate::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};
use crate::model::agent::AgentKind;

/// [`AgentChannel`] adapter that spawns one CLI subprocess per agent turn.
///
/// Stdout lines are classified by
/// [`agent::parse_stream_output_line`] and transient loader updates are
/// forwarded as [`TurnEvent::ThoughtDelta`]. A kill signal transitions the
/// turn to a failed state with a `[Stopped]` banner. A spawn failure is
/// surfaced through [`AgentError`].
pub(crate) struct CliAgentChannel {
    /// Provider-specific command builder.
    backend: Arc<dyn AgentBackend>,
    /// Provider family used for stream and response parsing.
    kind: AgentKind,
}

impl CliAgentChannel {
    /// Creates a CLI channel backed by the given pre-built backend.
    ///
    /// Channel factories use this helper so transport selection can be done
    /// once before constructing the concrete channel. Tests also use it to
    /// inject a [`MockAgentBackend`] that controls command construction and
    /// process spawning without relying on a real provider binary.
    pub(crate) fn with_backend(backend: Arc<dyn agent::AgentBackend>, kind: AgentKind) -> Self {
        Self { backend, kind }
    }
}

/// Bridges raw CLI execution observations into session turn events.
struct CliTurnObserver {
    /// Session event sink receiving PID and thought updates.
    events: mpsc::UnboundedSender<TurnEvent>,
    /// Provider family used to classify streamed stdout lines.
    kind: AgentKind,
}

impl CliExecutionObserver for CliTurnObserver {
    fn pid_updated(&self, child_pid: Option<u32>) {
        let _ = self.events.send(TurnEvent::PidUpdate(child_pid));
    }

    fn stdout_line(&self, line: &str) {
        let Some((text, is_response_content)) = agent::parse_stream_output_line(self.kind, line)
        else {
            return;
        };
        if is_response_content {
            return;
        }

        let trimmed_text = text.trim();
        if trimmed_text.is_empty() {
            return;
        }

        let _ = self
            .events
            .send(TurnEvent::ThoughtDelta(trimmed_text.to_string()));
    }
}

/// Builds the provider backend command request for one CLI turn.
fn build_command_request<'a>(
    request: &'a TurnRequest,
    prompt_text: &'a str,
) -> BuildCommandRequest<'a> {
    BuildCommandRequest {
        attachments: &request.prompt.attachments,
        folder: &request.folder,
        main_checkout_root: request.main_checkout_root.as_deref(),
        replay_transcript: request.continuation.replay_transcript(),
        model: &request.model,
        permission_mode: request.permission_mode,
        personality_prompt: request.personality.current(),
        prompt: prompt_text,
        reasoning_level: request.reasoning_level,
        request_kind: &request.request_kind,
        speed_mode: request.speed_mode,
    }
}

impl AgentChannel for CliAgentChannel {
    /// Returns a [`SessionRef`] immediately; CLI turns are stateless.
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>> {
        let session_id = req.session_id;

        Box::pin(async move { Ok(SessionRef { session_id }) })
    }

    /// Spawns a CLI process for the turn and streams its output as events.
    ///
    /// Stdout lines are parsed with the provider-specific stream parser and
    /// loader-oriented interim text is forwarded as
    /// [`TurnEvent::ThoughtDelta`]. After the process exits, usage
    /// statistics are extracted from the raw stdout/stderr and the final
    /// parsed response is returned in [`TurnResult`].
    ///
    /// # Errors
    /// Returns [`AgentError`] when command construction fails, the process
    /// cannot be spawned, or the process is killed by a signal.
    fn run_turn(
        &self,
        _session_id: String,
        req: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        let kind = self.kind;
        let backend = Arc::clone(&self.backend);

        Box::pin(async move {
            let mut req = req;
            req.prompt = agent::apply_response_style_prompt(
                req.prompt,
                req.request_kind.protocol_profile(),
                req.response_style,
            )
            .map_err(|error| AgentError::Backend(error.to_string()))?;
            let prompt_text = req.prompt.agent_text();
            let replay = agent::replay::ReplayContext::prepare(
                req.folder.clone(),
                req.continuation.replay_transcript().map(str::to_owned),
            )
            .await
            .map_err(|error| AgentError::Backend(error.to_string()))?;
            let mut build_request = build_command_request(&req, &prompt_text);
            build_request.replay_transcript = replay.text.as_deref();
            let observer = CliTurnObserver {
                events: events.clone(),
                kind,
            };
            let output = execution::execute_cli_command(
                backend.as_ref(),
                kind,
                build_request,
                &observer,
                None,
            )
            .await
            .map_err(map_cli_turn_execution_error)?;

            match output.exit_status {
                CliExitStatus::Signaled(_) => {
                    return Err(AgentError::Backend(
                        "[Stopped] Agent interrupted by user.".to_string(),
                    ));
                }
                CliExitStatus::NonZero(exit_code) => {
                    return Err(format_cli_turn_exit_error(
                        kind,
                        exit_code,
                        &output.stdout,
                        &output.stderr,
                    ));
                }
                CliExitStatus::Success => {}
            }

            let parsed = agent::parse_response(kind, &output.stdout, &output.stderr);
            let assistant_message =
                parse_or_repair_cli_response(kind, &parsed.content, &req, &backend, &events)
                    .await?;

            Ok(TurnResult {
                assistant_message,
                context_reset: false,
                input_tokens: parsed.stats.input_tokens,
                output_tokens: parsed.stats.output_tokens,
                provider_conversation_id: None,
            })
        })
    }

    /// No-op; CLI sessions are stateless and require no teardown.
    fn shutdown_session(&self, _session_id: String) -> AgentFuture<Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Parses one CLI turn response strictly, falling back to a single
/// protocol-repair retry when the initial parse fails.
///
/// When repair is attempted, a concise [`TurnEvent::ThoughtDelta`] is emitted
/// so the user can see that schema repair is in progress without flooding the
/// session output with parser diagnostics unless the turn ultimately fails.
async fn parse_or_repair_cli_response(
    kind: AgentKind,
    content: &str,
    req: &TurnRequest,
    backend: &Arc<dyn AgentBackend>,
    events: &mpsc::UnboundedSender<TurnEvent>,
) -> Result<AgentResponse, AgentError> {
    let protocol_profile = req.request_kind.protocol_profile();

    let parse_error = match agent::parse_turn_response(kind, content, protocol_profile) {
        Ok(response) => return Ok(response),
        Err(error) => error,
    };

    let _ = events.send(TurnEvent::ThoughtDelta(format!(
        "Protocol parse error; retrying schema repair for {kind}."
    )));

    let repair_prompt =
        build_protocol_repair_prompt(&parse_error, content).map_err(AgentError::Backend)?;

    let repair_content = execute_cli_repair_turn(backend.as_ref(), kind, req, &repair_prompt)
        .await
        .map_err(|error| {
            AgentError::Backend(format!(
                "{parse_error}\nprotocol repair transport failed: {error}"
            ))
        })?;

    agent::parse_turn_response(kind, &repair_content, protocol_profile).map_err(|repair_error| {
        AgentError::Backend(format!(
            "{parse_error}\nprotocol repair retry also failed: {repair_error}"
        ))
    })
}

/// Maximum wall-clock time for one protocol-repair CLI subprocess.
///
/// Repair turns ask the agent to re-emit a single JSON object, so they
/// should complete quickly. The timeout prevents a hung subprocess from
/// blocking the parent turn indefinitely. `kill_on_drop(true)` on the
/// child ensures the process is terminated when the future is dropped.
const REPAIR_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

/// Spawns a fresh CLI process for one protocol-repair retry and returns
/// the parsed provider content string.
///
/// This helper strips down the full turn-execution pipeline to the minimum
/// needed for repair: command build, spawn, stdout/stderr collection, and
/// provider response parsing. No streaming, PID tracking, or signal
/// handling is performed because the repair is a transparent one-shot
/// correction, not a user-visible turn. A [`REPAIR_TURN_TIMEOUT`] guard
/// ensures a stuck process does not block the parent turn indefinitely.
async fn execute_cli_repair_turn(
    backend: &dyn AgentBackend,
    kind: AgentKind,
    request: &TurnRequest,
    repair_prompt: &str,
) -> Result<String, String> {
    let prompt_payload = TurnPrompt::from_agent_data(repair_prompt.to_string());
    let build_request = BuildCommandRequest {
        attachments: &prompt_payload.attachments,
        folder: &request.folder,
        main_checkout_root: None,
        replay_transcript: None,
        model: &request.model,
        permission_mode: crate::model::permission::PermissionMode::ReadOnly,
        personality_prompt: None,
        prompt: repair_prompt,
        reasoning_level: request.reasoning_level,
        request_kind: &request.request_kind,
        speed_mode: request.speed_mode,
    };
    execute_cli_repair_command(backend, kind, build_request, REPAIR_TURN_TIMEOUT).await
}

/// Executes one prepared repair command with an explicit deadline.
async fn execute_cli_repair_command(
    backend: &dyn AgentBackend,
    kind: AgentKind,
    build_request: BuildCommandRequest<'_>,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let output = execution::execute_cli_command(
        backend,
        kind,
        build_request,
        &CollectingCliObserver,
        Some(timeout),
    )
    .await
    .map_err(|error| format!("repair {error}"))?;

    match output.exit_status {
        CliExitStatus::NonZero(exit_code) => {
            return Err(format!(
                "repair process exited with code {}",
                exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string())
            ));
        }
        CliExitStatus::Signaled(signal) => {
            return Err(format!("repair process was interrupted by signal {signal}"));
        }
        CliExitStatus::Success => {}
    }

    let parsed = agent::parse_response(kind, &output.stdout, &output.stderr);

    Ok(parsed.content)
}

/// Maps shared execution failures into the existing channel error categories.
fn map_cli_turn_execution_error(error: CliExecutionError) -> AgentError {
    match error {
        CliExecutionError::CommandBuild(error) => {
            AgentError::Backend(format!("Failed to build command: {error}"))
        }
        CliExecutionError::Spawn(error) => {
            AgentError::Io(format!("Failed to spawn process: {error}"))
        }
        CliExecutionError::StdinBuild(error) => {
            AgentError::Backend(format!("Failed to build command stdin payload: {error}"))
        }
        error => AgentError::Io(error.to_string()),
    }
}

/// Formats one failed CLI turn into a user-facing error.
fn format_cli_turn_exit_error(
    kind: AgentKind,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> AgentError {
    AgentError::Backend(error::format_agent_cli_exit_error(
        kind,
        "Agent command",
        exit_code,
        stdout,
        stderr,
    ))
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
