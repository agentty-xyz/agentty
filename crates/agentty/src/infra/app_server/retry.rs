//! Shared app-server restart and retry orchestration.

use super::contract::{
    AppServerFuture, AppServerTurnRequest, AppServerTurnResponse, BorrowedAppServerFuture,
};
use super::error::AppServerError;
use super::prompt::{
    instruction_delivery_mode_for_runtime, read_latest_session_output, turn_prompt_for_runtime,
};
use super::registry::{ActiveAppServerTurn, AppServerSessionRegistry};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::agent::ProtocolSchemaInstructionMode;

/// Callbacks for inspecting runtime state during turn execution.
///
/// Bundles the query functions that [`run_turn_with_restart_retry`] uses to
/// check whether the runtime matches the current request, whether it restored
/// provider-native context, and to extract identifiers.
pub struct RuntimeInspector<Runtime> {
    /// Returns `true` when the existing runtime is compatible with the request.
    pub matches_request: fn(&Runtime, &AppServerTurnRequest) -> bool,
    /// Returns the OS process id of the runtime, when available.
    pub pid: fn(&Runtime) -> Option<u32>,
    /// Returns the provider-native conversation id, when available.
    pub provider_conversation_id: fn(&Runtime) -> Option<String>,
    /// Returns `true` when the runtime bootstrapped by restoring prior context.
    pub restored_context: fn(&Runtime) -> bool,
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
pub async fn run_turn_with_restart_retry<Runtime, StartRuntime, RunTurn, ShutdownRuntime>(
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
    let first_provider_conversation_id = (inspector.provider_conversation_id)(&session_runtime);
    let first_prompt = build_attempt_prompt(
        &request,
        first_replays,
        first_provider_conversation_id.as_deref(),
        schema_instruction_mode,
        &mut shutdown_runtime,
        &mut session_runtime,
    )
    .await?;
    let first_attempt = run_cancellable_turn_attempt(
        &active_turn,
        &mut session_runtime,
        &first_prompt,
        &mut run_turn_with_runtime,
        &mut shutdown_runtime,
    )
    .await;
    if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
        return store_successful_runtime_response(
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
    let mut restarted = start_runtime(&request).await?;
    let retry_replays = needs_replay(false, &request, &inspector, &restarted);
    let retry_provider_conversation_id = (inspector.provider_conversation_id)(&restarted);
    let retry_prompt = build_attempt_prompt(
        &request,
        retry_replays,
        retry_provider_conversation_id.as_deref(),
        schema_instruction_mode,
        &mut shutdown_runtime,
        &mut restarted,
    )
    .await?;
    match run_cancellable_turn_attempt(
        &active_turn,
        &mut restarted,
        &retry_prompt,
        &mut run_turn_with_runtime,
        &mut shutdown_runtime,
    )
    .await
    {
        Ok(attempt_output) => {
            store_successful_runtime_response(
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

/// Stores a successful runtime back into the idle registry and builds the
/// normalized app-server response.
///
/// If the registry cannot accept the runtime, this shuts the runtime down
/// before returning the lock error so app-server child processes do not leak.
async fn store_successful_runtime_response<Runtime, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    session_id: String,
    session_runtime: Runtime,
    context_reset: bool,
    attempt_output: (String, u64, u64),
    inspector: &RuntimeInspector<Runtime>,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<AppServerTurnResponse, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let (assistant_message, input_tokens, output_tokens) = attempt_output;
    let pid = (inspector.pid)(&session_runtime);
    let provider_conversation_id = (inspector.provider_conversation_id)(&session_runtime);

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

/// Returns `true` when the attempt should replay prior session output as
/// context for the runtime.
fn needs_replay<Runtime>(
    had_existing_runtime: bool,
    request: &AppServerTurnRequest,
    inspector: &RuntimeInspector<Runtime>,
    runtime: &Runtime,
) -> bool {
    !had_existing_runtime
        && read_latest_session_output(request)
            .as_deref()
            .is_some_and(|session_output| !session_output.trim().is_empty())
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
) -> Result<TurnPrompt, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let session_output = read_latest_session_output(request);
    let instruction_delivery_mode = instruction_delivery_mode_for_runtime(
        request,
        current_provider_conversation_id,
        replays_context,
    );

    match turn_prompt_for_runtime(
        &request.prompt,
        &request.request_kind,
        session_output.as_deref(),
        instruction_delivery_mode,
        schema_instruction_mode,
    ) {
        Ok(prompt) => Ok(prompt),
        Err(error) => {
            shutdown_runtime(runtime).await;

            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::turn_prompt::TurnPrompt;
    use crate::infra::agent::{InstructionDeliveryMode, ProtocolSchemaInstructionMode};
    use crate::infra::channel::AgentRequestKind;

    struct TestRuntime {
        model: String,
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn session_resume_request_kind(session_output: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume {
            session_output: session_output.map(ToString::to_string),
        }
    }

    #[test]
    fn take_session_returns_stored_runtime() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        sessions
            .store_session(
                "session-1".to_string(),
                TestRuntime {
                    model: "model-a".to_string(),
                },
            )
            .expect("store should succeed");

        // Act
        let session = sessions
            .take_session("session-1")
            .expect("take should succeed");

        // Assert
        assert_eq!(
            session.map(|runtime| runtime.model),
            Some("model-a".to_string())
        );
    }

    #[test]
    fn turn_prompt_for_runtime_adds_repo_root_path_instructions_without_context_reset() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &session_start_request_kind(),
            Some("prior context"),
            InstructionDeliveryMode::BootstrapFull,
            ProtocolSchemaInstructionMode::PromptSchema,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.contains("summary"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &session_resume_request_kind(Some("assistant: proposed plan")),
            Some("assistant: proposed plan"),
            InstructionDeliveryMode::BootstrapWithReplay,
            ProtocolSchemaInstructionMode::PromptSchema,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.contains("Continue this session using the full transcript below."));
        assert!(turn_prompt.contains("assistant: proposed plan"));
        assert!(turn_prompt.contains(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_uses_shared_protocol_wrapper_for_utility_prompts() {
        // Arrange
        let prompt = "Generate title";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &AgentRequestKind::UtilityPrompt,
            None,
            InstructionDeliveryMode::BootstrapFull,
            ProtocolSchemaInstructionMode::PromptSchema,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("summary"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn read_latest_session_output_prefers_live_buffer_over_snapshot() {
        // Arrange
        let live_output = Arc::new(Mutex::new("live content from stream".to_string()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: Some(live_output),
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("live content from stream".to_string()));
    }

    #[test]
    fn read_latest_session_output_falls_back_to_snapshot_when_live_buffer_is_empty() {
        // Arrange
        let live_output = Arc::new(Mutex::new(String::new()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: Some(live_output),
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("stale snapshot".to_string()));
    }

    #[test]
    fn read_latest_session_output_falls_back_to_snapshot_when_no_live_buffer() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("stale snapshot".to_string()));
    }

    #[test]
    fn read_latest_session_output_returns_none_when_both_are_absent() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_start_request_kind(),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, None);
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_uses_live_output_on_retry() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let live_output = Arc::new(Mutex::new("streamed before crash".to_string()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: Some(live_output),
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };
        let captured_retry_prompt = Arc::new(Mutex::new(String::new()));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| None,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let run_count = Arc::new(AtomicUsize::new(0));
                let captured_retry_prompt = Arc::clone(&captured_retry_prompt);
                move |_runtime: &mut TestRuntime, prompt: &TurnPrompt| {
                    let attempt = run_count.fetch_add(1, Ordering::SeqCst);
                    let prompt = prompt.to_string();
                    let captured_retry_prompt = Arc::clone(&captured_retry_prompt);

                    Box::pin(async move {
                        if attempt == 0 {
                            return Err(AppServerError::Provider("first failure".to_string()));
                        }

                        if let Ok(mut guard) = captured_retry_prompt.lock() {
                            *guard = prompt;
                        }

                        Ok(("done".to_string(), 7, 3))
                    })
                }
            },
            |_runtime: &mut TestRuntime| Box::pin(async {}),
        )
        .await
        .expect("retry should succeed");

        // Assert
        assert!(response.context_reset);
        assert_eq!(response.provider_conversation_id, None);
        let retry_prompt = captured_retry_prompt
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        assert!(
            retry_prompt.contains("streamed before crash"),
            "retry prompt should contain live output, not stale snapshot"
        );
        assert!(
            !retry_prompt.contains("stale snapshot"),
            "retry prompt should use live output instead of stale snapshot"
        );
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_restarts_once_after_first_failure() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("previous output")),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };
        let start_count = Arc::new(AtomicUsize::new(0));
        let run_count = Arc::new(AtomicUsize::new(0));
        let shutdown_count = Arc::new(AtomicUsize::new(0));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| None,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            {
                let start_count = Arc::clone(&start_count);
                move |request: &AppServerTurnRequest| {
                    let start_count = Arc::clone(&start_count);
                    let model = request.model.clone();

                    Box::pin(async move {
                        start_count.fetch_add(1, Ordering::SeqCst);

                        Ok(TestRuntime { model })
                    })
                }
            },
            {
                let run_count = Arc::clone(&run_count);
                move |_runtime, _prompt| {
                    let attempt = run_count.fetch_add(1, Ordering::SeqCst);

                    Box::pin(async move {
                        if attempt == 0 {
                            return Err(AppServerError::Provider("first failure".to_string()));
                        }

                        Ok(("done".to_string(), 7, 3))
                    })
                }
            },
            {
                let shutdown_count = Arc::clone(&shutdown_count);
                move |_runtime| {
                    let shutdown_count = Arc::clone(&shutdown_count);

                    Box::pin(async move {
                        shutdown_count.fetch_add(1, Ordering::SeqCst);
                    })
                }
            },
        )
        .await
        .expect("retry should succeed");

        // Assert
        assert_eq!(response.assistant_message, "done");
        assert!(response.context_reset);
        assert_eq!((response.input_tokens, response.output_tokens), (7, 3));
        assert_eq!(response.pid, Some(42));
        assert_eq!(response.provider_conversation_id, None);
        assert_eq!(start_count.load(Ordering::SeqCst), 2);
        assert_eq!(run_count.load(Ordering::SeqCst), 2);
        assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_shutdown_signal_interrupts_in_flight_runtime() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("previous output")),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };
        let run_count = Arc::new(AtomicUsize::new(0));
        let shutdown_count = Arc::new(AtomicUsize::new(0));

        // Act
        let result = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| None,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let run_count = Arc::clone(&run_count);
                let sessions = sessions.clone();
                move |_runtime, _prompt| {
                    let run_count = Arc::clone(&run_count);
                    let sessions = sessions.clone();

                    Box::pin(async move {
                        run_count.fetch_add(1, Ordering::SeqCst);
                        sessions
                            .cancel_active_turn("session-1")
                            .expect("cancel should signal active turn");
                        std::future::pending::<Result<(String, u64, u64), AppServerError>>().await
                    })
                }
            },
            {
                let shutdown_count = Arc::clone(&shutdown_count);
                move |_runtime| {
                    let shutdown_count = Arc::clone(&shutdown_count);

                    Box::pin(async move {
                        shutdown_count.fetch_add(1, Ordering::SeqCst);
                    })
                }
            },
        )
        .await;

        // Assert
        assert!(matches!(result, Err(AppServerError::InterruptedByUser(_))));
        assert_eq!(run_count.load(Ordering::SeqCst), 1);
        assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
    }

    /// Verifies restored-context retries keep the user prompt while avoiding
    /// transcript replay.
    #[tokio::test]
    async fn run_turn_with_restart_retry_skips_replay_when_runtime_restores_context() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("previous output")),
            provider_conversation_id: Some("thread-123".to_string()),
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };
        let captured_prompt = Arc::new(Mutex::new(String::new()));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(24),
                provider_conversation_id: |_runtime| Some("thread-123".to_string()),
                restored_context: |_runtime| true,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let captured_prompt = Arc::clone(&captured_prompt);
                move |_runtime: &mut TestRuntime, prompt: &TurnPrompt| {
                    let prompt = prompt.to_string();
                    let captured_prompt = Arc::clone(&captured_prompt);

                    Box::pin(async move {
                        if let Ok(mut guard) = captured_prompt.lock() {
                            *guard = prompt;
                        }

                        Ok(("done".to_string(), 1, 1))
                    })
                }
            },
            |_runtime: &mut TestRuntime| Box::pin(async {}),
        )
        .await
        .expect("turn should succeed");

        // Assert
        assert_eq!(response.assistant_message, "done");
        assert!(!response.context_reset);
        assert_eq!(
            response.provider_conversation_id,
            Some("thread-123".to_string())
        );
        assert_eq!(response.pid, Some(24));
        let captured_prompt = captured_prompt
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        assert!(captured_prompt.contains("repository-root-relative POSIX paths"));
        assert!(captured_prompt.ends_with("Do work"));
        assert!(!captured_prompt.contains("previous output"));
    }
}
