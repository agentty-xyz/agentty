//! Channel turn execution for session workers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::worker::{SessionWorkerContext, TurnMetadata};
use super::{SessionTaskService, isolation, post_turn};
use crate::app::session::SessionError;
use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{SessionId, Status};
use crate::domain::setting::SettingName;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::channel::{
    AgentError, AgentRequestKind, TurnEvent, TurnPrompt, TurnRequest, TurnResult,
};
use crate::infra::db::AppRepositories;
use crate::infra::process;

/// Main-checkout tracked-file status captured before one provider turn.
struct MainCheckoutSnapshot {
    main_repo_root: PathBuf,
    tracked_status_output: String,
}

impl MainCheckoutSnapshot {
    /// Captures the main repository checkout tracked status before a provider
    /// turn.
    ///
    /// # Errors
    /// Returns a workflow error when the session folder is not a valid linked
    /// worktree or the main-checkout tracked status cannot be read.
    async fn capture(context: &SessionWorkerContext) -> Result<Self, SessionError> {
        let validation = isolation::validate_session_worktree(
            context.fs_client.as_ref(),
            context.git_client.as_ref(),
            &context.folder,
            &context.session_id,
        )
        .await?;
        let tracked_status_output = context
            .git_client
            .tracked_worktree_status(validation.main_repo_root.clone())
            .await
            .map_err(|error| Self::status_error(&error))?;

        Ok(Self {
            main_repo_root: validation.main_repo_root,
            tracked_status_output,
        })
    }

    /// Builds a warning when a provider turn changed tracked files in the main
    /// checkout.
    ///
    /// # Errors
    /// Returns a workflow error when the main-checkout tracked status cannot
    /// be read after the provider turn.
    async fn changed_warning(
        &self,
        context: &SessionWorkerContext,
    ) -> Result<Option<String>, SessionError> {
        let current_status = context
            .git_client
            .tracked_worktree_status(self.main_repo_root.clone())
            .await
            .map_err(|error| Self::status_error(&error))?;
        if current_status != self.tracked_status_output {
            return Ok(Some(TranscriptNotice::MainCheckoutWarning.format(
                "Tracked files in the main checkout changed during this turn. Continuing this \
                 session; merge and sync actions still require a clean main checkout.",
            )));
        }

        Ok(None)
    }

    /// Converts main-checkout tracked status failures into workflow errors.
    fn status_error(error: &crate::infra::git::GitError) -> SessionError {
        SessionError::Workflow(format!(
            "Session isolation violation: failed to inspect main checkout tracked status: {error}"
        ))
    }
}

/// Executes one agent turn through the session channel and applies all
/// post-turn effects (stats, auto-commit, size refresh, status update).
///
/// When `request_kind` is [`AgentRequestKind::SessionResume`], the session
/// is first transitioned to `InProgress` (start turns set `InProgress` in
/// the lifecycle before enqueueing). Start turns schedule detached title
/// generation immediately before the main turn request runs. Progress
/// events update the UI indicator; `PidUpdate` events update the shared PID
/// slot used for cancellation. If the turn fails, the error is appended to
/// session output before transitioning to `Review`; user-stopped turns
/// skip that fallback so the UI cancellation path can finalize `Canceled`.
///
/// A fresh [`CancellationToken`] is swapped into the shared mutex at
/// the top of this function so stale cancellations from previous
/// turns cannot affect new work. A `Ctrl+c` arriving during setup
/// cancels the new token, which is detected by the early-exit check
/// in [`run_turn_with_cancellation`].
pub(super) async fn run_channel_turn(
    context: &SessionWorkerContext,
    turn_metadata: TurnMetadata,
    request_kind: AgentRequestKind,
    prompt: TurnPrompt,
) -> Result<(), SessionError> {
    // Swap in a fresh token so stale cancellations from previous
    // turns are discarded. The cloned token is passed to
    // `run_turn_with_cancellation` for the duration of this turn.
    let turn_cancel_token = fresh_turn_cancel_token(context)?;

    if matches!(request_kind, AgentRequestKind::SessionResume { .. }) {
        // Best-effort: questions persistence failure is non-critical.
        let _ = context
            .db
            .sessions()
            .update_session_questions(&context.session_id, "")
            .await;

        // Best-effort: status transition failure is non-critical.
        let _ = SessionTaskService::update_status(
            &context.status,
            context.clock.as_ref(),
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            Status::InProgress,
        )
        .await;
    }

    let main_checkout_snapshot = match MainCheckoutSnapshot::capture(context).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            SessionManager::cleanup_prompt_attachment_paths(
                context.fs_client.clone(),
                prompt.local_image_paths().cloned().collect(),
            )
            .await;
            let post_turn_context = post_turn::PostTurnContext::from_worker(context);
            let finalizer_context = post_turn::TurnFinalizerContext::from_worker(context);
            let result = post_turn::apply_turn_result(
                &post_turn_context,
                turn_metadata,
                Err(AgentError::Backend(error.to_string())),
            )
            .await;
            post_turn::finalize_channel_turn(&finalizer_context, &result).await;

            return result.map(|_| ());
        }
    };

    let session_project_id = load_session_project_id(&context.db, &context.session_id).await;
    let reasoning_level =
        load_session_reasoning_level(&context.db, &context.session_id, session_project_id).await;
    let provider_conversation_id = context
        .db
        .sessions()
        .get_session_provider_conversation_id(&context.session_id)
        .await
        .ok()
        .flatten();
    let persisted_instruction_conversation_id = context
        .db
        .sessions()
        .get_session_instruction_conversation_id(&context.session_id)
        .await
        .ok()
        .flatten();

    let req = TurnRequest {
        folder: context.folder.clone(),
        live_session_output: Some(Arc::clone(&context.output)),
        model: turn_metadata
            .session_agent
            .model()
            .provider_model_str()
            .to_string(),
        request_kind: request_kind.clone(),
        prompt: prompt.clone(),
        provider_conversation_id,
        persisted_instruction_conversation_id,
        reasoning_level,
    };

    let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let consumer = tokio::spawn(consume_turn_events(
        event_rx,
        context.app_event_tx.clone(),
        context.session_id.clone(),
        Arc::clone(&context.child_pid),
    ));

    spawn_start_turn_title_generation(
        context,
        session_project_id,
        &request_kind,
        &prompt.text,
        turn_metadata.session_agent.model(),
    )
    .await;

    let turn_result = run_turn_with_cancellation(context, turn_cancel_token, req, event_tx).await;
    SessionManager::cleanup_prompt_attachment_paths(
        context.fs_client.clone(),
        prompt.local_image_paths().cloned().collect(),
    )
    .await;

    let _ = consumer.await;

    let turn_result =
        add_main_checkout_warning(context, &main_checkout_snapshot, turn_result).await;
    let post_turn_context = post_turn::PostTurnContext::from_worker(context);
    let finalizer_context = post_turn::TurnFinalizerContext::from_worker(context);
    let result = post_turn::apply_turn_result(&post_turn_context, turn_metadata, turn_result).await;
    post_turn::finalize_channel_turn(&finalizer_context, &result).await;

    result.map(|_| ())
}

/// Runs one agent turn with cancellation support.
///
/// Races `run_turn` against the per-turn [`CancellationToken`]. When the
/// token is cancelled (`Ctrl+c`), `SIGTERM` is sent to the active child
/// process (if any) via [`terminate_child_process`], the channel is shut
/// down gracefully through `shutdown_session`, and the function waits for
/// the `run_turn` future to resolve (with a timeout) so the subprocess is
/// not orphaned. Sending `SIGTERM` from inside the cancellation branch
/// (rather than from the UI) eliminates the stale-PID / PID-reuse risk:
/// `run_turn` has not returned yet, so the PID slot still belongs to the
/// active child.
///
/// Each turn receives its own fresh token, created at the start of
/// [`run_channel_turn`]. This eliminates the stale-permit problem that
/// required the previous `Notify` + `AtomicBool` flag-check pattern.
pub(super) async fn run_turn_with_cancellation(
    context: &SessionWorkerContext,
    cancel_token: CancellationToken,
    req: TurnRequest,
    event_tx: mpsc::UnboundedSender<TurnEvent>,
) -> Result<TurnResult, AgentError> {
    // Honour a cancel that arrived during pre-turn setup, before the
    // select had a chance to observe it. The token was freshly created
    // at the top of `run_channel_turn`, so a cancelled state here is a
    // real `Ctrl+c`, not a stale leftover.
    if cancel_token.is_cancelled() {
        terminate_child_process(context);
        let _ = context
            .channel
            .shutdown_session(context.session_id.to_string())
            .await;

        return Err(AgentError::InterruptedByUser(
            "[Stopped] Session interrupted by user.".to_string(),
        ));
    }

    let turn_future = context
        .channel
        .run_turn(context.session_id.to_string(), req, event_tx);
    tokio::pin!(turn_future);

    tokio::select! {
        result = &mut turn_future => result,
        () = cancel_token.cancelled() => {
            // Send SIGTERM to the child process while it is guaranteed
            // alive (run_turn has not returned yet). This is safe from
            // PID-reuse because the PID slot is only cleared after
            // run_turn completes. App-server channels ignore the signal
            // because their PID slot is always None.
            terminate_child_process(context);

            // Graceful shutdown: close stdin, wait for exit, kill if
            // needed.
            let _ = context
                .channel
                .shutdown_session(context.session_id.to_string())
                .await;

            // Wait for the turn future to resolve so the subprocess is
            // not orphaned. CLI channels return a signal-killed error
            // once the child exits; app-server channels complete once
            // their runtime stops. A timeout guards against indefinite
            // blocking if the channel does not shut down promptly.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                &mut turn_future,
            )
            .await;

            Err(AgentError::InterruptedByUser(
                "[Stopped] Session interrupted by user.".to_string(),
            ))
        }
    }
}

/// Converts channel-layer turn failures into session workflow errors.
pub(super) fn session_error_from_agent_error(error: AgentError) -> SessionError {
    match error {
        AgentError::InterruptedByUser(message) => SessionError::StoppedByUser(message),
        other => SessionError::Workflow(other.to_string()),
    }
}

/// Loads the project identifier associated with one persisted session.
pub(super) async fn load_session_project_id(db: &AppRepositories, session_id: &str) -> Option<i64> {
    db.sessions()
        .load_session_project_id(session_id)
        .await
        .ok()
        .flatten()
}

/// Loads the effective reasoning level for one session context.
pub(super) async fn load_session_reasoning_level(
    db: &AppRepositories,
    session_id: &str,
    project_id: Option<i64>,
) -> ReasoningLevel {
    if let Ok(Some(reasoning_level)) = db
        .sessions()
        .load_session_reasoning_level_override(session_id)
        .await
    {
        return reasoning_level;
    }

    let Some(project_id) = project_id else {
        return ReasoningLevel::default();
    };

    db.settings()
        .load_project_reasoning_level(project_id)
        .await
        .unwrap_or_default()
}

/// Consumes [`TurnEvent`]s from `event_rx` and applies their side effects.
///
/// - [`TurnEvent::ThoughtDelta`]: updates the transient thinking loader text.
/// - [`TurnEvent::PidUpdate`]: writes the new PID into `child_pid`.
/// - [`TurnEvent::Completed`] / [`TurnEvent::Failed`]: reserved; ignored here
///   because completion is signalled by `run_turn`'s return value.
pub(super) async fn consume_turn_events(
    mut event_rx: mpsc::UnboundedReceiver<TurnEvent>,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    session_id: SessionId,
    child_pid: Arc<Mutex<Option<u32>>>,
) {
    let mut active_progress: Option<String> = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TurnEvent::ThoughtDelta(thought) => {
                let Some(thought) = normalize_thinking_stream_text(&thought) else {
                    continue;
                };
                if active_progress.as_deref() == Some(thought.as_str()) {
                    continue;
                }

                active_progress = Some(thought.clone());
                SessionTaskService::set_session_progress(&app_event_tx, &session_id, Some(thought));
            }
            TurnEvent::PidUpdate(pid) => {
                // Sync critical section (single assignment, no `.await`);
                // `std::sync::Mutex` is the correct choice per CLAUDE.md
                // §"Mutex Selection".
                if let Ok(mut guard) = child_pid.lock() {
                    *guard = pid;
                }
            }
            TurnEvent::Completed { .. } | TurnEvent::Failed(_) => {
                // Completion is signalled by run_turn's return value; these
                // variants are reserved for future use and ignored here.
            }
        }
    }

    if active_progress.take().is_some() {
        SessionTaskService::clear_session_progress(&app_event_tx, &session_id);
    }
}

/// Converts post-turn main-checkout changes into transcript warnings while
/// preserving the successful provider result.
async fn add_main_checkout_warning(
    context: &SessionWorkerContext,
    main_checkout_snapshot: &MainCheckoutSnapshot,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<TurnResult, AgentError> {
    let result = turn_result?;
    match main_checkout_snapshot.changed_warning(context).await {
        Ok(Some(warning)) => {
            append_main_checkout_warning(context, warning).await;

            Ok(result)
        }
        Ok(None) => Ok(result),
        Err(error) => Err(AgentError::Backend(error.to_string())),
    }
}

/// Sends `SIGTERM` to the active child process tracked in
/// `context.child_pid`, if any.
///
/// Best-effort: the PID slot may be `None` (app-server channels never
/// publish a PID) or the process may have already exited. Both cases are
/// silently ignored.
pub(super) fn terminate_child_process(context: &SessionWorkerContext) {
    // Sync critical section (the guard is dropped at the end of the chain
    // expression, before any `.await`); `std::sync::Mutex` is the correct
    // choice per CLAUDE.md §"Mutex Selection".
    let active_pid = context
        .child_pid
        .lock()
        .ok()
        .and_then(|mut child_pid| child_pid.take());

    if let Some(pid) = active_pid {
        process::send_terminate_signal(pid);
    }
}

/// Replaces the shared cancellation token for a new turn and returns the
/// token used by the running channel future.
fn fresh_turn_cancel_token(
    context: &SessionWorkerContext,
) -> Result<CancellationToken, SessionError> {
    // Sync critical section (assignment + clone, no `.await`); `std::sync::Mutex`
    // is the correct choice per CLAUDE.md §"Mutex Selection".
    let mut guard = context
        .cancel_token
        .lock()
        .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))?;
    *guard = CancellationToken::new();

    Ok(guard.clone())
}

/// Appends one main-checkout warning to the live and persisted transcript.
async fn append_main_checkout_warning(context: &SessionWorkerContext, warning: String) {
    SessionTaskService::append_session_output(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &warning,
    )
    .await;
}

/// Spawns first-turn session title generation from the initial user prompt.
async fn spawn_start_turn_title_generation(
    context: &SessionWorkerContext,
    session_project_id: Option<i64>,
    request_kind: &AgentRequestKind,
    prompt: &str,
    session_model: AgentModel,
) {
    if !matches!(request_kind, AgentRequestKind::SessionStart) {
        return;
    }

    let title_model = load_project_model_setting(
        &context.db,
        session_project_id,
        SettingName::DefaultFastModel,
    )
    .await
    .unwrap_or(session_model);

    let _title_generation_task = SessionManager::spawn_session_title_generation_task(
        context.app_event_tx.clone(),
        context.db.clone(),
        &context.session_id,
        &context.folder,
        prompt,
        title_model,
        None,
    );
}

/// Loads one project-scoped model setting and parses it into an [`AgentModel`].
///
/// Retired persisted model ids are upgraded to their current replacement
/// models before the setting is returned.
async fn load_project_model_setting(
    db: &AppRepositories,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> Option<AgentModel> {
    let project_id = project_id?;

    db.settings()
        .get_project_setting(project_id, setting_name)
        .await
        .ok()
        .flatten()
        .and_then(|setting_value| AgentModel::parse_persisted(&setting_value).ok())
}

/// Returns one normalized thinking text line.
fn normalize_thinking_stream_text(text: &str) -> Option<String> {
    let trimmed_text = text.trim();
    if trimmed_text.is_empty() {
        return None;
    }

    Some(trimmed_text.to_string())
}
