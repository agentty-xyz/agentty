//! One-shot agent prompt execution helpers.
//!
//! These helpers run isolated utility prompts outside the long-lived session
//! turn flow. They require the shared structured response protocol on every
//! transport so one-shot callers enforce the same schema contract as normal
//! session turns.

use std::future::poll_fn;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use ag_contracts::{
    OneShotClient, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    SessionDiffState, SessionStats,
};
use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, build_protocol_repair_prompt,
    format_protocol_parse_debug_details, parse_protocol_response_strict,
};
use ag_session::AgentKind;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::backend::{AgentBackend, BuildCommandRequest};
use super::cli::error;
use super::cli::execution::{self, CliExecutionError, CliExecutionObserver, CliExitStatus};
use super::submission_pool::{SubmissionLease, SubmissionPool};
use super::{ParsedResponse, create_app_server_client, create_backend, parse_response};
use crate::app_server::{AppServerClient, AppServerTurnRequest};

/// Production [`OneShotClient`] that routes through the selected provider.
pub struct RealOneShotClient {
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
    force_shutdown: CancellationToken,
    pool: Mutex<Option<SubmissionPool>>,
}

impl RealOneShotClient {
    /// Creates a client with an optional app-server override.
    ///
    /// Production passes `None` so each provider supplies its native client;
    /// deterministic environments may inject a shared app-server boundary.
    pub fn new(app_server_client_override: Option<Arc<dyn AppServerClient>>) -> Self {
        Self {
            app_server_client_override,
            force_shutdown: CancellationToken::new(),
            pool: Mutex::new(None),
        }
    }

    /// Creates a scoped client that reuses idle app-server processes while
    /// starting fresh conversation context for every submission. Call
    /// [`OneShotClient::close`] after submissions and their cleanup finish.
    pub fn pooled(app_server_client_override: Option<Arc<dyn AppServerClient>>) -> Self {
        Self {
            app_server_client_override,
            force_shutdown: CancellationToken::new(),
            pool: Mutex::new(Some(SubmissionPool::default())),
        }
    }
}

#[async_trait]
impl OneShotClient for RealOneShotClient {
    fn force_shutdown(&self) {
        self.force_shutdown.cancel();
        // Release idle registries immediately; active cleanup tasks keep their
        // leases until the forced-shutdown signal drops their provider work.
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        self.submit_cancellable(request, CancellationToken::new())
            .await
    }

    async fn submit_cancellable(
        &self,
        request: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        let agent_kind = request
            .harness
            .parse::<AgentKind>()
            .map_err(OneShotError::new)?;
        let lease = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|pool| pool.acquire(agent_kind, self.app_server_client_override.clone()));
        if let Some(lease) = lease {
            return submit_app_server_session(
                Arc::clone(&lease.0.client),
                request,
                cancellation,
                self.force_shutdown.clone(),
                Some(lease),
            )
            .await
            .map_err(OneShotError::new);
        }
        let client = create_app_server_client(agent_kind, self.app_server_client_override.clone());
        if let Some(client) = client {
            return submit_one_shot_with_app_server_client(
                client,
                request,
                cancellation,
                self.force_shutdown.clone(),
            )
            .await
            .map_err(OneShotError::new);
        }
        let backend = create_backend(agent_kind);
        tokio::select! {
            biased;
            () = self.force_shutdown.cancelled() => Err(OneShotError::new("[Stopped] Agent runtime forced to shut down")),
            () = cancellation.cancelled() => Err(OneShotError::new("[Stopped] Agent run canceled")),
            result = submit_one_shot_with_backend(backend.as_ref(), request) => result.map_err(OneShotError::new),
        }
    }

    async fn close(&self) {
        let pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(pool) = pool {
            pool.close().await;
        }
    }
}

/// Executes an isolated app-server prompt in an owned cleanup task.
/// Dropping the caller signals cancellation without dropping the provider turn;
/// cooperative callers await this future until provider shutdown completes.
async fn submit_one_shot_with_app_server_client(
    app_server_client: Arc<dyn AppServerClient>,
    request: OneShotRequest,
    cancellation: CancellationToken,
    force_shutdown: CancellationToken,
) -> Result<OneShotSubmission, String> {
    submit_app_server_session(
        app_server_client,
        request,
        cancellation,
        force_shutdown,
        None,
    )
    .await
}

/// Holds a pooled lease until the owned turn and asynchronous cleanup settle.
async fn submit_app_server_session(
    app_server_client: Arc<dyn AppServerClient>,
    request: OneShotRequest,
    cancellation: CancellationToken,
    force_shutdown: CancellationToken,
    lease: Option<SubmissionLease>,
) -> Result<OneShotSubmission, String> {
    let cancellation = cancellation.child_token();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    tokio::spawn(async move {
        let session_id = lease.as_ref().map_or_else(
            || format!("one-shot-{}", uuid::Uuid::new_v4()),
            |lease| lease.0.session_id.clone(),
        );
        let pooled = lease.is_some();
        let _lease = lease;
        let child_pid = request.child_pid.clone();
        let result = tokio::select! {
            biased;
            () = force_shutdown.cancelled() => Err("[Stopped] Agent runtime forced to shut down".to_string()),
            result = finish_one_shot_app_server_submission(app_server_client, request, cancellation, session_id, pooled) => result,
        };
        clear_child_pid_slot(child_pid.as_deref());
        result
    })
    .await
    .map_err(|error| format!("One-shot cleanup task failed: {error}"))?
}

/// Retains the provider future while shutdown signals its active runtime.
async fn finish_one_shot_app_server_submission(
    app_server_client: Arc<dyn AppServerClient>,
    request: OneShotRequest,
    cancellation: CancellationToken,
    session_id: String,
    pooled: bool,
) -> Result<OneShotSubmission, String> {
    clear_child_pid_slot(request.child_pid.as_deref());
    let work = execute_one_shot_app_server_turns(
        app_server_client.as_ref(),
        request,
        &session_id,
        &cancellation,
        pooled,
    );
    tokio::pin!(work);
    let work = poll_fn(|context| {
        catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(context)))
            .unwrap_or_else(|_| Poll::Ready(Err("One-shot provider turn panicked".to_string())))
    });
    tokio::pin!(work);
    tokio::select! {
        biased;
        result = &mut work => {
            if !pooled || result.is_err() || cancellation.is_cancelled() {
                app_server_client.shutdown_session(session_id.clone()).await;
            }
            result
        }
        () = cancellation.cancelled() => {
            // Keep polling the turn so the adapter can observe shutdown and
            // release the runtime it temporarily removed from its registry.
            let _ = tokio::join!(app_server_client.shutdown_session(session_id.clone()), &mut work);
            Err("[Stopped] Agent run canceled".to_string())
        }
    }
}

/// Executes the initial turn and optional protocol repair in the same session.
async fn execute_one_shot_app_server_turns(
    app_server_client: &dyn AppServerClient,
    request: OneShotRequest,
    session_id: &str,
    cancellation: &CancellationToken,
    pooled: bool,
) -> Result<OneShotSubmission, String> {
    if cancellation.is_cancelled() {
        return Err("[Stopped] Agent run canceled".to_string());
    }
    let protocol_profile = request.request_kind.protocol_profile();
    let (stream_tx, _stream_rx) = tokio::sync::mpsc::unbounded_channel();
    let turn_request = AppServerTurnRequest {
        execution_policy: request.execution_policy.clone(),
        provider_call_budget: request.provider_call_budget.clone(),
        folder: request.folder.clone(),
        live_transcript: None,
        main_checkout_root: None,
        model: request.model.clone(),
        permission_mode: request.permission_mode,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: ag_protocol::TurnPrompt::from_agent_data(request.prompt.clone()),
        request_kind: request.request_kind.clone(),
        replay_transcript: None,
        provider_conversation_id: None,
        persisted_instruction_conversation_id: None,
        reasoning_level: request.reasoning_level,
        session_id: session_id.to_string(),
        speed_mode: request.speed_mode,
    };

    let turn_result = if pooled {
        app_server_client.run_isolated_turn(turn_request, stream_tx)
    } else {
        app_server_client.run_turn(turn_request, stream_tx)
    }
    .await
    .map_err(|error| format!("Failed to execute one-shot app-server turn: {error}"))?;
    if cancellation.is_cancelled() {
        return Err("[Stopped] Agent run canceled".to_string());
    }

    let parse_result =
        match parse_one_shot_response(&turn_result.assistant_message, protocol_profile) {
            Ok(response) => Ok((response, 0, 0)),
            Err(parse_error) => {
                attempt_one_shot_app_server_repair(
                    app_server_client,
                    &parse_error,
                    &turn_result.assistant_message,
                    request,
                    session_id,
                    turn_result.provider_conversation_id.as_deref(),
                    pooled,
                )
                .await
            }
        };

    let (response, repair_input_tokens, repair_output_tokens) = parse_result?;

    Ok(OneShotSubmission {
        response,
        stats: SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: turn_result.input_tokens + repair_input_tokens,
            output_tokens: turn_result.output_tokens + repair_output_tokens,
        },
    })
}

/// Executes one isolated prompt using the provided backend.
///
/// This shared helper keeps process execution behind the existing
/// `AgentBackend` trait boundary so production callers and tests can reuse
/// the same one-shot parsing path.
///
/// # Errors
/// Returns an error when command construction fails, process execution fails,
/// or the final output is empty or otherwise unusable.
async fn submit_one_shot_with_backend(
    backend: &dyn AgentBackend,
    request: OneShotRequest,
) -> Result<OneShotSubmission, String> {
    let protocol_profile = request.request_kind.protocol_profile();
    let parsed_response =
        execute_one_shot_command(backend, &request.prompt, request.clone()).await?;
    let (agent_response, repair_stats) =
        match parse_one_shot_response(&parsed_response.content, protocol_profile) {
            Ok(response) => (response, None),
            Err(parse_error) => {
                let repair_prompt =
                    build_protocol_repair_prompt(&parse_error, &parsed_response.content)?;
                let request = OneShotRequest {
                    permission_mode: PermissionMode::ReadOnly,
                    ..request
                };
                let repair_response = execute_one_shot_command(backend, &repair_prompt, request)
                    .await
                    .map_err(|error| format!("{parse_error}\nrepair transport failed: {error}"))?;

                let response = parse_one_shot_response(&repair_response.content, protocol_profile)
                    .map_err(|error| {
                        format!(
                            "{parse_error}\nrepair retry also failed: \
                             {error}\nrepair_response:\n{}",
                            repair_response.content
                        )
                    })?;

                (response, Some(repair_response.stats))
            }
        };

    let mut stats = parsed_response.stats;
    if let Some(repair) = repair_stats {
        stats.input_tokens += repair.input_tokens;
        stats.output_tokens += repair.output_tokens;
    }

    Ok(OneShotSubmission {
        response: agent_response,
        stats,
    })
}

/// Parses one one-shot response strictly against the shared protocol schema.
///
/// # Errors
/// Returns an error when the response is empty or not valid protocol JSON. The
/// error carries the parse reason and derived diagnostics only, never the
/// provider payload itself.
fn parse_one_shot_response(
    content: &str,
    protocol_profile: ProtocolRequestProfile,
) -> Result<AgentResponse, String> {
    parse_protocol_response_strict(content, protocol_profile).map_err(|error| {
        format!(
            "One-shot agent output did not match the required JSON schema: \
             {error}\ndebug_details:\n{}",
            format_protocol_parse_debug_details(content)
        )
    })
}

/// Attempts one protocol-repair retry through the app-server transport for
/// a one-shot prompt whose initial response failed schema validation.
///
/// The repair prompt is sent as a follow-up turn on the same session so the
/// agent retains the original conversation context. The initial turn's
/// `provider_conversation_id` is threaded through so providers that depend
/// on conversation state can continue the same thread.
/// Pooled repairs retain the runtime without resetting that context.
///
/// Returns the parsed response together with the repair turn's token usage
/// so the caller can aggregate stats across both attempts.
///
/// # Errors
/// Returns the combined original and repair error when the retry fails.
async fn attempt_one_shot_app_server_repair(
    app_server_client: &dyn AppServerClient,
    parse_error: &str,
    malformed_response: &str,
    request: OneShotRequest,
    session_id: &str,
    provider_conversation_id: Option<&str>,
    pooled: bool,
) -> Result<(AgentResponse, u64, u64), String> {
    let protocol_profile = request.request_kind.protocol_profile();
    let repair_prompt = build_protocol_repair_prompt(parse_error, malformed_response)?;

    let (repair_stream_tx, _repair_stream_rx) = tokio::sync::mpsc::unbounded_channel();
    let repair_turn_request = AppServerTurnRequest {
        execution_policy: request.execution_policy.clone(),
        provider_call_budget: request.provider_call_budget.clone(),
        folder: request.folder,
        live_transcript: None,
        main_checkout_root: None,
        model: request.model.clone(),
        permission_mode: request.permission_mode,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: ag_protocol::TurnPrompt::from_agent_data(repair_prompt),
        request_kind: request.request_kind,
        replay_transcript: None,
        provider_conversation_id: provider_conversation_id.map(String::from),
        persisted_instruction_conversation_id: None,
        reasoning_level: request.reasoning_level,
        session_id: session_id.to_string(),
        speed_mode: request.speed_mode,
    };
    let repair_result = if pooled {
        app_server_client.run_retained_turn(repair_turn_request, repair_stream_tx)
    } else {
        app_server_client.run_turn(repair_turn_request, repair_stream_tx)
    }
    .await
    .map_err(|error| format!("{parse_error}\nrepair transport failed: {error}"))?;

    let response = parse_one_shot_response(&repair_result.assistant_message, protocol_profile)
        .map_err(|error| {
            format!(
                "{parse_error}\nrepair retry also failed: {error}\nrepair_response:\n{}",
                repair_result.assistant_message
            )
        })?;

    Ok((
        response,
        repair_result.input_tokens,
        repair_result.output_tokens,
    ))
}

/// Runs one one-shot backend command and returns the parsed provider content.
///
/// The spawned child is configured with `kill_on_drop(true)` so timeout-driven
/// callers do not leave orphaned agent CLI processes behind when the future is
/// canceled before completion.
///
/// # Errors
/// Returns an error when the command cannot be built, run, or exits
/// unsuccessfully.
async fn execute_one_shot_command(
    backend: &dyn AgentBackend,
    prompt: &str,
    request: OneShotRequest,
) -> Result<ParsedResponse, String> {
    if let Some(budget) = &request.provider_call_budget {
        budget.consume().map_err(|error| error.to_string())?;
    }
    let prompt_payload = ag_protocol::TurnPrompt::from_agent_data(prompt.to_string());
    let agent_kind = request.harness.parse::<AgentKind>()?;
    let build_request = BuildCommandRequest {
        execution_policy: &request.execution_policy,
        attachments: &prompt_payload.attachments,
        folder: &request.folder,
        main_checkout_root: None,
        replay_transcript: None,
        model: &request.model,
        permission_mode: request.permission_mode,
        personality_prompt: None,
        prompt,
        reasoning_level: request.reasoning_level,
        request_kind: &request.request_kind,
        speed_mode: request.speed_mode,
    };
    let observer = OneShotCliObserver {
        child_pid: request.child_pid,
    };
    let output =
        execution::execute_cli_command(backend, agent_kind, build_request, &observer, None)
            .await
            .map_err(format_one_shot_execution_error)?;

    match output.exit_status {
        CliExitStatus::Signaled(_) => {
            return Err("One-shot agent command was interrupted".to_string());
        }
        CliExitStatus::NonZero(exit_code) => {
            return Err(format_one_shot_exit_error(
                agent_kind,
                exit_code,
                &output.stdout,
                &output.stderr,
            ));
        }
        CliExitStatus::Success => {}
    }

    let parsed_response = parse_response(agent_kind, &output.stdout, &output.stderr);

    Ok(parsed_response)
}

/// Preserves the established one-shot context around shared execution errors.
fn format_one_shot_execution_error(error: CliExecutionError) -> String {
    match error {
        CliExecutionError::CommandBuild(error) => {
            format!("Failed to build one-shot agent command: {error}")
        }
        CliExecutionError::StdinBuild(error) => {
            format!("Failed to build one-shot agent stdin payload: {error}")
        }
        error => format!("Failed to execute one-shot agent command: {error}"),
    }
}

/// Formats one non-zero one-shot command exit into a user-facing error.
fn format_one_shot_exit_error(
    agent_kind: AgentKind,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    error::format_agent_cli_exit_error(
        agent_kind,
        "One-shot agent command",
        exit_code,
        stdout,
        stderr,
    )
}

/// Clears the shared one-shot child PID slot when one exists.
fn clear_child_pid_slot(child_pid: Option<&Mutex<Option<u32>>>) {
    let Some(child_pid) = child_pid else {
        return;
    };

    if let Ok(mut guard) = child_pid.lock() {
        *guard = None;
    }
}

/// Bridges shared CLI PID observations into the one-shot accounting slot.
struct OneShotCliObserver {
    child_pid: Option<Arc<Mutex<Option<u32>>>>,
}

impl CliExecutionObserver for OneShotCliObserver {
    fn pid_updated(&self, active_child_pid: Option<u32>) {
        let Some(child_pid_slot) = self.child_pid.as_deref() else {
            return;
        };

        if let Ok(mut guard) = child_pid_slot.lock() {
            *guard = active_child_pid;
        }
    }

    fn stdout_line(&self, _line: &str) {}
}

#[cfg(test)]
#[path = "submission_test.rs"]
mod tests;

#[cfg(test)]
#[path = "submission_budget_test.rs"]
mod budget_tests;
