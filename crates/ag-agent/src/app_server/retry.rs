//! Shared app-server restart and retry orchestration.

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};

use super::contract::{
    AppServerFuture, AppServerTurnRequest, AppServerTurnResponse, BorrowedAppServerFuture,
};
use super::error::AppServerError;
use super::prompt::{
    instruction_delivery_mode_for_runtime, read_latest_replay_transcript, turn_prompt_for_runtime,
};
use super::registry::{ActiveAppServerTurn, AppServerSessionRegistry};
use crate::agent::replay::ReplayContext;

/// Callbacks for inspecting runtime state during turn execution.
///
/// Bundles the query functions that [`run_turn_with_restart_retry`] uses to
/// check whether the runtime matches the current request, whether it restored
/// provider-native context, and to extract identifiers.
pub(crate) struct RuntimeInspector<Runtime> {
    /// Returns `true` when the existing runtime is compatible with the request.
    pub(crate) matches_request: fn(&Runtime, &AppServerTurnRequest) -> bool,
    /// Returns the OS process id of the runtime, when available.
    pub(crate) pid: fn(&Runtime) -> Option<u32>,
    /// Returns the provider-native conversation id, when available.
    pub(crate) provider_conversation_id: fn(&Runtime) -> Option<String>,
    /// Returns `true` when the runtime bootstrapped by restoring prior context.
    pub(crate) restored_context: fn(&Runtime) -> bool,
    /// Whether successful runtimes remain resident between session turns.
    pub(crate) retain_runtime_after_turn: bool,
}

/// Runs one app-server turn with restart-and-retry semantics.
///
/// Runtime lifecycle details (`start`, per-turn execution, and shutdown) are
/// injected by the provider. The function keeps a session-scoped runtime in
/// `sessions`, invalidates it when request shape changes, and retries once
/// after restarting the runtime when the first attempt fails.
///
/// Prompt transcript replay is only applied when a newly started runtime does
/// not expose restored provider-native context for the session.
///
/// `schema_instruction_mode` is selected by the provider client so bootstrap
/// prompts include the full JSON Schema only for transports that need
/// prompt-side schema guidance.
///
/// # Errors
/// Returns an error when runtime startup/execution fails, retry fails, or the
/// session registry lock is unavailable.
pub(crate) async fn run_turn_with_restart_retry<Runtime, StartRuntime, RunTurn, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    request: AppServerTurnRequest,
    inspector: RuntimeInspector<Runtime>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    mut start_runtime: StartRuntime,
    mut run_turn_with_runtime: RunTurn,
    mut shutdown_runtime: ShutdownRuntime,
) -> Result<AppServerTurnResponse, AppServerError>
where
    StartRuntime: FnMut(&AppServerTurnRequest) -> AppServerFuture<Result<Runtime, AppServerError>>,
    RunTurn: for<'scope> FnMut(
        &'scope mut Runtime,
        &'scope TurnPrompt,
    ) -> BorrowedAppServerFuture<
        'scope,
        Result<(String, u64, u64), AppServerError>,
    >,
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let session_id = request.session_id.clone();
    let active_turn = sessions.register_active_turn(&session_id)?;
    let session_runtime =
        take_compatible_session_runtime(sessions, &request, &inspector, &mut shutdown_runtime)
            .await?;

    let had_existing_runtime = session_runtime.is_some();
    let mut session_runtime = match session_runtime {
        Some(existing_runtime) => existing_runtime,
        None => start_runtime(&request).await?,
    };
    let first_replays = needs_replay(had_existing_runtime, &request, &inspector, &session_runtime);
    let first_attempt = {
        let (first_prompt, _first_replay) = build_attempt_prompt(
            &request,
            first_replays,
            (inspector.provider_conversation_id)(&session_runtime).as_deref(),
            schema_instruction_mode,
            &mut shutdown_runtime,
            &mut session_runtime,
        )
        .await?;

        run_cancellable_turn_attempt(
            &active_turn,
            &mut session_runtime,
            &first_prompt,
            request.provider_call_budget.as_ref(),
            &mut run_turn_with_runtime,
            &mut shutdown_runtime,
        )
        .await
    };
    if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
        return complete_successful_runtime_response(
            sessions,
            session_id,
            session_runtime,
            first_replays,
            (assistant_message, input_tokens, output_tokens),
            &inspector,
            &mut shutdown_runtime,
        )
        .await;
    }

    let first_error = first_attempt
        .err()
        .unwrap_or_else(|| AppServerError::Provider("App-server turn failed".to_string()));
    if matches!(first_error, AppServerError::InterruptedByUser(_)) {
        return Err(first_error);
    }

    shutdown_runtime(&mut session_runtime).await;
    if crate::is_input_size_error(&first_error.to_string()) {
        return Err(first_error);
    }

    let mut restarted = start_runtime(&request).await?;
    let retry_replays = needs_replay(false, &request, &inspector, &restarted);
    let (retry_prompt, _retry_replay) = build_attempt_prompt(
        &request,
        retry_replays,
        (inspector.provider_conversation_id)(&restarted).as_deref(),
        schema_instruction_mode,
        &mut shutdown_runtime,
        &mut restarted,
    )
    .await?;
    match run_cancellable_turn_attempt(
        &active_turn,
        &mut restarted,
        &retry_prompt,
        request.provider_call_budget.as_ref(),
        &mut run_turn_with_runtime,
        &mut shutdown_runtime,
    )
    .await
    {
        Ok(attempt_output) => {
            complete_successful_runtime_response(
                sessions,
                session_id,
                restarted,
                retry_replays,
                attempt_output,
                &inspector,
                &mut shutdown_runtime,
            )
            .await
        }
        Err(retry_error) => {
            if matches!(retry_error, AppServerError::InterruptedByUser(_)) {
                return Err(retry_error);
            }

            shutdown_runtime(&mut restarted).await;

            Err(AppServerError::RetryExhausted {
                provider: sessions.provider_name(),
                first_error: first_error.to_string(),
                retry_error: retry_error.to_string(),
            })
        }
    }
}

/// Takes the idle runtime for a request and shuts it down when it no longer
/// matches the requested model or provider context.
async fn take_compatible_session_runtime<Runtime, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    request: &AppServerTurnRequest,
    inspector: &RuntimeInspector<Runtime>,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<Option<Runtime>, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let mut session_runtime = sessions.take_session(&request.session_id)?;

    if session_runtime
        .as_ref()
        .is_some_and(|runtime| !(inspector.matches_request)(runtime, request))
    {
        if let Some(runtime) = session_runtime.as_mut() {
            shutdown_runtime(runtime).await;
        }

        session_runtime = None;
    }

    Ok(session_runtime)
}

/// Completes a successful turn, either retaining or shutting down its runtime,
/// and builds the normalized app-server response.
///
/// If the registry cannot accept the runtime, this shuts the runtime down
/// before returning the lock error so app-server child processes do not leak.
async fn complete_successful_runtime_response<Runtime, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    session_id: String,
    mut session_runtime: Runtime,
    context_reset: bool,
    attempt_output: (String, u64, u64),
    inspector: &RuntimeInspector<Runtime>,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<AppServerTurnResponse, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let (assistant_message, input_tokens, output_tokens) = attempt_output;
    let provider_conversation_id = (inspector.provider_conversation_id)(&session_runtime);

    if !inspector.retain_runtime_after_turn {
        shutdown_runtime(&mut session_runtime).await;

        return Ok(AppServerTurnResponse {
            assistant_message,
            context_reset,
            input_tokens,
            output_tokens,
            pid: None,
            provider_conversation_id,
        });
    }

    let pid = (inspector.pid)(&session_runtime);
    if let Err((error, mut leaked)) = sessions.store_session_or_recover(session_id, session_runtime)
    {
        shutdown_runtime(&mut leaked).await;

        return Err(error);
    }

    Ok(AppServerTurnResponse {
        assistant_message,
        context_reset,
        input_tokens,
        output_tokens,
        pid,
        provider_conversation_id,
    })
}

/// Runs one provider runtime attempt while watching the session-scoped
/// app-server cancellation token.
///
/// `run_turn_with_restart_retry()` temporarily owns the runtime while a turn is
/// in flight, so provider `shutdown_session()` cannot remove it from the idle
/// registry. This helper gives that shutdown path a token to fire; when it
/// fires, the running turn future is dropped, the runtime is shut down through
/// the provider lifecycle hook, and the attempt returns a user-interruption
/// error instead of retrying.
async fn run_cancellable_turn_attempt<Runtime, RunTurn, ShutdownRuntime>(
    active_turn: &ActiveAppServerTurn,
    runtime: &mut Runtime,
    prompt: &TurnPrompt,
    provider_call_budget: Option<&crate::ProviderCallBudget>,
    run_turn_with_runtime: &mut RunTurn,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<(String, u64, u64), AppServerError>
where
    RunTurn: for<'scope> FnMut(
        &'scope mut Runtime,
        &'scope TurnPrompt,
    ) -> BorrowedAppServerFuture<
        'scope,
        Result<(String, u64, u64), AppServerError>,
    >,
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let cancellation_token = active_turn.token();
    if cancellation_token.is_cancelled() {
        shutdown_runtime(runtime).await;

        return Err(interrupted_by_user_error());
    }

    if let Some(budget) = provider_call_budget {
        budget
            .consume()
            .map_err(|error| AppServerError::Provider(error.to_string()))?;
    }
    let turn_outcome = {
        let turn_future = run_turn_with_runtime(runtime, prompt);
        tokio::pin!(turn_future);
        tokio::select! {
            result = &mut turn_future => TurnAttemptOutcome::Completed(result),
            () = cancellation_token.cancelled() => TurnAttemptOutcome::Interrupted,
        }
    };

    match turn_outcome {
        TurnAttemptOutcome::Completed(result) => result,
        TurnAttemptOutcome::Interrupted => {
            shutdown_runtime(runtime).await;

            Err(interrupted_by_user_error())
        }
    }
}

/// Result of racing one app-server turn against cancellation.
enum TurnAttemptOutcome {
    /// The provider turn completed before cancellation fired.
    Completed(Result<(String, u64, u64), AppServerError>),
    /// The session cancellation token fired first.
    Interrupted,
}

/// Builds the app-server interruption error shared by initial and retry turns.
fn interrupted_by_user_error() -> AppServerError {
    AppServerError::InterruptedByUser("[Stopped] Session interrupted by user.".to_string())
}

/// Returns `true` when the attempt should replay prior transcript as
/// context for the runtime.
fn needs_replay<Runtime>(
    had_existing_runtime: bool,
    request: &AppServerTurnRequest,
    inspector: &RuntimeInspector<Runtime>,
    runtime: &Runtime,
) -> bool {
    !had_existing_runtime
        && read_latest_replay_transcript(request)
            .as_deref()
            .is_some_and(|replay_transcript| !replay_transcript.trim().is_empty())
        && !(inspector.restored_context)(runtime)
}

/// Prepares the prompt for one turn attempt, shutting down the runtime on
/// failure.
async fn build_attempt_prompt<Runtime, ShutdownRuntime>(
    request: &AppServerTurnRequest,
    replays_context: bool,
    current_provider_conversation_id: Option<&str>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    shutdown_runtime: &mut ShutdownRuntime,
    runtime: &mut Runtime,
) -> Result<(TurnPrompt, ReplayContext), AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let replay_transcript = read_latest_replay_transcript(request);
    let replay = match ReplayContext::prepare(request.folder.clone(), replay_transcript).await {
        Ok(replay) => replay,
        Err(error) => {
            shutdown_runtime(runtime).await;

            return Err(AppServerError::PromptRender(error.to_string()));
        }
    };
    let instruction_delivery_mode = instruction_delivery_mode_for_runtime(
        request,
        current_provider_conversation_id,
        replays_context,
    );

    let mut prompt = request.prompt.clone();
    if !replays_context && let Some(reference) = &replay.reference {
        prompt.text = format!("{reference}\n\n{}", prompt.agent_text());
        prompt.text_source = ag_protocol::TurnPromptTextSource::AgentData;
    }

    let prepared = turn_prompt_for_runtime(
        &prompt,
        &request.request_kind,
        replay.text.as_deref(),
        instruction_delivery_mode,
        &request.personality,
        schema_instruction_mode,
        &request.folder,
    );

    finish_prompt_preparation(prepared, replay, shutdown_runtime, runtime).await
}

/// Keeps the replay archive alive through rendering and shuts the runtime down
/// if the renderer fails. Accepts the fallible result so cleanup is testable
/// independently of the concrete template renderer.
async fn finish_prompt_preparation<Runtime, ShutdownRuntime>(
    prepared: Result<TurnPrompt, AppServerError>,
    replay: ReplayContext,
    shutdown_runtime: &mut ShutdownRuntime,
    runtime: &mut Runtime,
) -> Result<(TurnPrompt, ReplayContext), AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    match prepared {
        Ok(prompt) => Ok((prompt, replay)),
        Err(error) => {
            shutdown_runtime(runtime).await;

            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "retry_test.rs"]
mod tests;
