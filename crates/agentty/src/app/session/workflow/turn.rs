//! Channel turn execution for session workers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentDefinitionPrompt, AgentError, AgentRequestKind, LiveTranscript, TurnContinuation,
    TurnEvent, TurnRequest, TurnResult,
};
use ag_session::ResolvedEnv;
use ag_telemetry::{FutureExt as _, Outcome, Span};
use ag_worker::RunClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::lifecycle::SessionTitleGenerationTaskInput;
use super::post_turn::PostTurnContext;
use super::worker::{SessionWorkerContext, SessionWorkerService, TurnMetadata};
use super::{SessionTaskService, StatusTransition, isolation, post_turn};
use crate::app::session::SessionError;
use crate::app::{AppEvent, SessionManager, setting};
use crate::domain::agent::{AgentKind, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptTextSource};
use crate::infra::db::{AppRepositories, LEGACY_SESSION_AGENT_CLEAR_MARKER};

/// Maximum ready turn events folded into one progress app-event emission.
///
/// The cap prevents a chatty provider stream from turning coalescing itself
/// into an unbounded drain. Events beyond this budget remain queued for the
/// consumer's next await/try-receive cycle.
const TURN_EVENT_PROGRESS_COALESCE_BUDGET: usize = 64;
/// Agent-facing behavior appended to ordinary read-only chat turns.
const READ_ONLY_CHAT_PROMPT: &str = include_str!("../../template/read_only_chat_prompt.md");

/// Host-owned inputs shared by ordinary turns and task-chain title generation.
pub(super) struct RequestTitleInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) available_agent_kinds: Arc<[AgentKind]>,
    pub(super) db: AppRepositories,
    pub(super) folder: PathBuf,
    pub(super) prompt: String,
    pub(super) run_client: Arc<dyn RunClient>,
    pub(super) session_agent: AgentSelection,
    pub(super) session_id: SessionId,
}

impl RequestTitleInput {
    /// Captures ordinary-turn ownership before detached title work starts.
    fn from_worker(
        context: &SessionWorkerContext,
        run_client: Arc<dyn RunClient>,
        prompt: &str,
        session_agent: AgentSelection,
    ) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            available_agent_kinds: Arc::clone(&context.available_agent_kinds),
            db: context.db.clone(),
            folder: context.folder.clone(),
            prompt: prompt.to_string(),
            run_client,
            session_agent,
            session_id: context.session_id.clone(),
        }
    }
}

/// Live transcript source exposed to provider transports for replay.
#[derive(Debug)]
struct LiveSessionTranscript {
    transcript: Arc<Mutex<SessionTranscript>>,
}

impl LiveTranscript for LiveSessionTranscript {
    fn replay_text(&self) -> Option<String> {
        self.transcript
            .lock()
            .ok()
            .and_then(|transcript| transcript.replay_text())
    }
}

/// Builds a replay source backed by the session's typed transcript handle.
pub(super) fn live_transcript_source(
    transcript: &Arc<Mutex<SessionTranscript>>,
) -> Arc<dyn LiveTranscript> {
    Arc::new(LiveSessionTranscript {
        transcript: Arc::clone(transcript),
    })
}

/// Main-checkout tracked-file status captured before one provider turn.
struct MainCheckoutSnapshot {
    main_repo_root: PathBuf,
    tracked_status_output: String,
}

impl MainCheckoutSnapshot {
    /// Captures the main repository checkout status before a provider turn.
    ///
    /// Returns `None` when the shared repository is bare and therefore has no
    /// main working checkout to snapshot. The supplied Env has already passed
    /// session worktree validation, including for bare repositories.
    ///
    /// # Errors
    /// Returns a workflow error when main-checkout tracked status cannot be
    /// read.
    async fn capture(
        context: &SessionWorkerContext,
        env: &ResolvedEnv,
    ) -> Result<Option<Self>, SessionError> {
        let Some(main_repo_root) = env.main_checkout.clone() else {
            return Ok(None);
        };
        let tracked_status_output = context
            .git_client
            .tracked_worktree_status(main_repo_root.clone())
            .await
            .map_err(|error| Self::status_error(&error))?;

        Ok(Some(Self {
            main_repo_root,
            tracked_status_output,
        }))
    }

    /// Builds a warning when the main checkout tracked-file status changed and
    /// remains dirty after a provider turn.
    ///
    /// # Errors
    /// Returns a workflow error when main-checkout status cannot be read after
    /// the provider turn.
    async fn dirty_warning(
        &self,
        context: &SessionWorkerContext,
    ) -> Result<Option<String>, SessionError> {
        let current_status = context
            .git_client
            .tracked_worktree_status(self.main_repo_root.clone())
            .await
            .map_err(|error| Self::status_error(&error))?;
        if current_status != self.tracked_status_output && !current_status.trim().is_empty() {
            return Ok(Some(TranscriptNotice::MainCheckoutWarning.format(
                "The main checkout's tracked-file status changed during this turn. Continuing \
                 this session; merge and sync actions still require a clean main checkout.",
            )));
        }

        Ok(None)
    }

    /// Converts main-checkout tracked status failures into workflow errors.
    fn status_error(error: &ag_git::GitError) -> SessionError {
        SessionError::Workflow(format!(
            "Session isolation violation: failed to inspect main checkout tracked status: {error}"
        ))
    }
}

/// Executes one agent turn through the session channel and applies all
/// post-turn effects (stats, auto-commit, size refresh, status update).
///
/// Start and resume turns transition to `InProgress` before provider work.
/// Saved preparation prompts first record a durable execution marker. They
/// schedule detached title generation while the current title remains
/// provisional. Progress events update the UI indicator; `PidUpdate` events
/// update the shared PID slot used for accounting and CLI cancellation. If the
/// turn fails, the error is appended to session output before transitioning to
/// `Review`; user-stopped turns skip that fallback so the UI cancellation path
/// can finalize `Canceled`.
///
/// A fresh [`CancellationToken`] is swapped into the shared mutex at
/// the top of this function so stale cancellations from previous
/// turns cannot affect new work. A `Ctrl+c` arriving during setup
/// cancels the new token, which is detected by the early-exit check
/// in [`run_turn_with_cancellation`].
pub(super) async fn run_channel_turn(
    context: &SessionWorkerContext,
    run_client: Arc<dyn RunClient>,
    mut turn_metadata: TurnMetadata,
    request_kind: AgentRequestKind,
    replay_transcript: Option<String>,
    mut prompt: TurnPrompt,
) -> Result<(), SessionError> {
    // Discard stale cancellations, then keep the new token for this turn.
    let turn_cancel_token = fresh_turn_cancel_token(context)?;
    let run_scope = ag_worker::RunScope {
        cancellation: Some(turn_cancel_token.clone()),
        ..ag_worker::RunScope::default()
    };
    let run_client = ag_worker::scoped_client(run_client, run_scope);

    prepare_session_turn(context, &request_kind).await;

    let post_turn_context =
        PostTurnContext::from_worker(context, Arc::clone(&run_client), turn_cancel_token.clone());
    let workspace =
        match prepare_turn_workspace(context, &mut turn_metadata, &mut prompt, &turn_cancel_token)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return finalize_turn_setup_failure(
                    context,
                    &post_turn_context,
                    turn_metadata,
                    &prompt,
                    &error,
                )
                .await;
            }
        };

    let prepared = match prepare_turn_request(
        context,
        &turn_metadata,
        request_kind,
        replay_transcript,
        &prompt,
        &workspace.env,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            return finalize_turn_setup_failure(
                context,
                &post_turn_context,
                turn_metadata,
                &prompt,
                &error,
            )
            .await;
        }
    };
    let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let consumer = tokio::spawn(
        consume_turn_events(
            event_rx,
            context.app_event_tx.clone(),
            context.session_id.clone(),
            Arc::clone(&context.child_pid),
        )
        .with_context(ag_telemetry::Context::current()),
    );

    spawn_request_title_generation(
        RequestTitleInput::from_worker(
            context,
            Arc::clone(&run_client),
            &prompt.text,
            turn_metadata.session_agent,
        ),
        prepared.session_project_id,
    )
    .await;

    let turn_result =
        run_turn_with_cancellation(context, turn_cancel_token, prepared.request, event_tx).await;
    SessionManager::cleanup_prompt_attachment_paths(
        context.fs_client.clone(),
        prompt.local_image_paths().cloned().collect(),
    )
    .await;

    let activity = consumer.await.unwrap_or_default();

    let turn_result =
        add_main_checkout_warning(context, workspace.snapshot.as_ref(), turn_result).await;
    let finalizer = post_turn::TurnFinalizerContext::from_worker(context);
    let result = Span::child("postprocess")
        .run(
            post_turn::apply_turn_result(&post_turn_context, turn_metadata, turn_result, &activity),
            |error| match error {
                SessionError::StoppedByUser(_) => Outcome::Canceled,
                _ => Outcome::Failed,
            },
        )
        .await;
    activity.clear_live(&context.app_event_tx, &context.session_id);
    Span::child("session.finalize")
        .scope(post_turn::finalize_channel_turn(&finalizer, &result))
        .await;

    result.map(|_| ())
}

/// Validated execution location plus the existing main-checkout warning state.
struct PreparedTurnWorkspace {
    env: ResolvedEnv,
    snapshot: Option<MainCheckoutSnapshot>,
}

/// Waits for earlier branch effects, resolves the Env, and restores
/// unfinished review targets before constructing the next agent request.
async fn prepare_turn_workspace(
    context: &SessionWorkerContext,
    turn_metadata: &mut TurnMetadata,
    prompt: &mut TurnPrompt,
    cancel_token: &CancellationToken,
) -> Result<PreparedTurnWorkspace, SessionError> {
    tokio::select! {
        biased;
        () = cancel_token.cancelled() => Err(SessionError::StoppedByUser(
            "[Stopped] Turn canceled before workspace preparation completed.".to_string(),
        )),
        result = async {
            drop(context.branch_operation_lock.lock().await);
            let workspace = Span::child("workspace.prepare")
                .run(async {
                    let env = isolation::validate_session_worktree(
                        context.fs_client.as_ref(),
                        context.git_client.as_ref(),
                        &context.folder,
                        &context.session_id,
                    ).await?;
                    let snapshot = MainCheckoutSnapshot::capture(context, &env).await?;

                    Ok(PreparedTurnWorkspace { env, snapshot })
                }, |_: &SessionError| Outcome::Failed)
                .await?;
            SessionWorkerService::prepare_review_comment_turn(context, turn_metadata, prompt).await?;

            Ok(workspace)
        } => result,
    }
}

/// Settings resolved at execution time, after workspace preparation.
///
/// The queued model selection and worker-owned execution policy have different
/// lifetimes and deliberately remain outside this value.
struct ResolvedTurnSettings {
    agent_profile: AgentDefinitionPrompt,
    permission_mode: PermissionMode,
    reasoning_level: ReasoningLevel,
    response_style: ResponseStyle,
    speed_mode: SpeedMode,
}

/// Settings plus continuation and host bookkeeping read for one request.
struct ResolvedTurnContext {
    continuation: TurnContinuation,
    session_project_id: Option<i64>,
    settings: ResolvedTurnSettings,
}

impl ResolvedTurnContext {
    /// Reads the existing sources in order without persisting a configuration
    /// snapshot. Permission failures stop preparation; other loaders retain
    /// their existing defaults.
    async fn load(
        context: &SessionWorkerContext,
        replay_transcript: Option<String>,
    ) -> Result<Self, SessionError> {
        let session_project_id = load_session_project_id(&context.db, &context.session_id).await;
        let permission_mode =
            load_session_permission_mode(&context.db, &context.session_id).await?;
        let reasoning_level = load_session_reasoning_level(&context.db, &context.session_id).await;
        let response_style = load_session_response_style(&context.db, &context.session_id).await;
        let speed_mode = load_session_speed_mode(&context.db, &context.session_id).await;
        let continuation = load_turn_continuation(context, replay_transcript).await;
        let agent_profile = AgentDefinitionPrompt::cleared(
            continuation.persisted_instruction_conversation_id()
                == Some(LEGACY_SESSION_AGENT_CLEAR_MARKER),
        );

        Ok(Self {
            continuation,
            session_project_id,
            settings: ResolvedTurnSettings {
                agent_profile,
                permission_mode,
                reasoning_level,
                response_style,
                speed_mode,
            },
        })
    }
}

/// Request and host bookkeeping resolved together during context preparation.
struct PreparedTurnRequest {
    request: TurnRequest,
    session_project_id: Option<i64>,
}

/// Measures configuration and prompt preparation, preserving setup failures.
async fn prepare_turn_request(
    context: &SessionWorkerContext,
    turn_metadata: &TurnMetadata,
    request_kind: AgentRequestKind,
    replay_transcript: Option<String>,
    prompt: &TurnPrompt,
    env: &ResolvedEnv,
) -> Result<PreparedTurnRequest, SessionError> {
    Span::child("context.prepare")
        .run(
            async {
                let ResolvedTurnContext {
                    continuation,
                    session_project_id,
                    settings,
                } = ResolvedTurnContext::load(context, replay_transcript).await?;
                let agent_prompt =
                    apply_read_only_chat_prompt(prompt.clone(), settings.permission_mode);
                let model = turn_metadata.session_agent.model().provider_model_str();

                Ok(PreparedTurnRequest {
                    request: TurnRequest {
                        provider_call_budget: None,
                        execution_policy: ag_contracts::ExecutionPolicy::default(),
                        continuation,
                        folder: env.folder.clone(),
                        main_checkout_root: env.main_checkout.clone(),
                        model: model.to_string(),
                        permission_mode: settings.permission_mode,
                        agent_profile: settings.agent_profile,
                        prompt: agent_prompt,
                        reasoning_level: settings.reasoning_level,
                        request_kind,
                        response_style: settings.response_style,
                        speed_mode: settings.speed_mode,
                    },
                    session_project_id,
                })
            },
            |_| Outcome::Failed,
        )
        .await
}

/// Adds mode-switch guidance only to user-owned read-only chat sessions.
fn apply_read_only_chat_prompt(prompt: TurnPrompt, permission_mode: PermissionMode) -> TurnPrompt {
    if permission_mode != PermissionMode::ReadOnly {
        return prompt;
    }

    let agent_prompt = prompt.agent_text();

    TurnPrompt {
        attachments: prompt.attachments,
        text: format!("{READ_ONLY_CHAT_PROMPT}\n\n{agent_prompt}"),
        text_source: TurnPromptTextSource::AgentData,
    }
}

/// Cleans up and reports a failure that occurs after turn setup has started
/// but before the provider request can run.
async fn finalize_turn_setup_failure(
    context: &SessionWorkerContext,
    post_turn_context: &post_turn::PostTurnContext,
    turn_metadata: TurnMetadata,
    prompt: &TurnPrompt,
    error: &SessionError,
) -> Result<(), SessionError> {
    SessionManager::cleanup_prompt_attachment_paths(
        context.fs_client.clone(),
        prompt.local_image_paths().cloned().collect(),
    )
    .await;
    let finalizer_context = post_turn::TurnFinalizerContext::from_worker(context);
    let result = post_turn::apply_turn_result(
        post_turn_context,
        turn_metadata,
        Err(match error {
            SessionError::StoppedByUser(message) => AgentError::InterruptedByUser(message.clone()),
            _ => AgentError::Backend(error.to_string()),
        }),
        &super::activity::TurnActivity::default(),
    )
    .await;
    post_turn::finalize_channel_turn(&finalizer_context, &result).await;

    result.map(|_| ())
}

/// Loads the durable provider context needed to continue one session turn.
async fn load_turn_continuation(
    context: &SessionWorkerContext,
    replay_transcript: Option<String>,
) -> TurnContinuation {
    let provider_conversation_id = context
        .db
        .sessions()
        .get_session_provider_conversation_id(&context.session_id)
        .await
        .ok()
        .flatten();
    let instruction_conversation_id = context
        .db
        .sessions()
        .get_session_instruction_conversation_id(&context.session_id)
        .await
        .ok()
        .flatten();

    TurnContinuation::provider(
        Some(live_transcript_source(&context.transcript)),
        instruction_conversation_id,
        provider_conversation_id,
        replay_transcript,
    )
}

/// Returns the provider permission mode enforced for one persisted session.
///
/// # Errors
/// Returns an error when the session cannot be loaded or its persisted role
/// or permission mode is invalid.
pub(super) async fn load_session_permission_mode(
    db: &AppRepositories,
    session_id: &str,
) -> Result<PermissionMode, SessionError> {
    let row = db
        .sessions()
        .load_session(session_id)
        .await?
        .ok_or(SessionError::NotFound)?;
    row.permission_mode
        .parse()
        .map_err(|reason| crate::infra::db::DbError::InvalidData {
            entity: "session permission mode",
            reason,
        })
        .map_err(SessionError::from)
}

/// Marks an executing session turn in progress and clears resumed questions.
async fn prepare_session_turn(context: &SessionWorkerContext, request_kind: &AgentRequestKind) {
    if !matches!(
        request_kind,
        AgentRequestKind::SessionStart | AgentRequestKind::SessionResume
    ) {
        return;
    }

    if matches!(request_kind, AgentRequestKind::SessionResume) {
        let _ = context
            .db
            .sessions()
            .update_session_questions(&context.session_id, "")
            .await;
    }

    let status_transition = StatusTransition::from_parts(
        context.app_event_tx.clone(),
        Arc::clone(&context.clock),
        context.db.clone(),
        context.session_id.clone(),
        Arc::clone(&context.session_update_versions),
        Arc::clone(&context.status),
    );
    let _ = status_transition.apply(Status::InProgress).await;
}

/// Runs one turn through the headless worker's cancellation boundary.
pub(super) async fn run_turn_with_cancellation(
    context: &SessionWorkerContext,
    cancel_token: CancellationToken,
    req: TurnRequest,
    event_tx: mpsc::UnboundedSender<TurnEvent>,
) -> Result<TurnResult, AgentError> {
    let result = context
        .session_run
        .submit(req, event_tx, cancel_token)
        .await;
    if matches!(result, Err(AgentError::InterruptedByUser(_)))
        && let Ok(mut pid) = context.child_pid.lock()
    {
        *pid = None;
    }
    result
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
) -> ReasoningLevel {
    db.sessions()
        .load_session_reasoning_level(session_id)
        .await
        .unwrap_or_default()
}

/// Loads the response style for one session context.
pub(super) async fn load_session_response_style(
    db: &AppRepositories,
    session_id: &str,
) -> ResponseStyle {
    db.sessions()
        .load_session_response_style(session_id)
        .await
        .unwrap_or_default()
}

/// Loads the response-speed preference for one session context.
pub(super) async fn load_session_speed_mode(
    db: &AppRepositories,
    session_id: &str,
) -> crate::domain::agent::SpeedMode {
    db.sessions()
        .load_session_speed_mode(session_id)
        .await
        .unwrap_or_default()
}

/// Consumes [`TurnEvent`]s from `event_rx` and applies their side effects.
///
/// - [`TurnEvent::Activity`]: publishes live rows and reduces the usage
///   summary.
/// - [`TurnEvent::ThoughtDelta`]: coalesces immediately ready thought bursts
///   and updates the transient thinking loader text with the latest message.
/// - [`TurnEvent::PidUpdate`]: writes the new PID into `child_pid`.
/// - [`TurnEvent::Completed`] / [`TurnEvent::Failed`]: reserved; ignored here
///   because completion is signalled by `run_turn`'s return value.
pub(super) async fn consume_turn_events(
    mut event_rx: mpsc::UnboundedReceiver<TurnEvent>,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    session_id: SessionId,
    child_pid: Arc<Mutex<Option<u32>>>,
) -> super::activity::TurnActivity {
    let mut activity = super::activity::TurnActivity::default();
    let mut active_progress: Option<String> = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TurnEvent::Activity(event) => activity.observe_live(event, &app_event_tx, &session_id),
            TurnEvent::ThoughtDelta(thought) => {
                let Some(thought) = normalize_thinking_stream_text(&thought) else {
                    continue;
                };
                let thought = coalesce_ready_turn_progress_events(
                    &mut event_rx,
                    child_pid.as_ref(),
                    thought,
                    &session_id,
                    &app_event_tx,
                    &mut activity,
                );
                if active_progress.as_deref() == Some(thought.as_str()) {
                    continue;
                }

                active_progress = Some(thought.clone());
                SessionTaskService::set_session_progress(&app_event_tx, &session_id, Some(thought));
            }
            TurnEvent::PidUpdate(pid) => {
                set_child_pid(child_pid.as_ref(), pid);
            }
            TurnEvent::Completed { .. } | TurnEvent::Failed(_) => {
                // Completion is signalled by run_turn's return value; these
                // variants are reserved for future use and ignored here.
            }
        }
    }

    activity.finish_live(&app_event_tx, &session_id);
    if active_progress.take().is_some() {
        SessionTaskService::clear_session_progress(&app_event_tx, &session_id);
    }

    activity
}

/// Coalesces immediately ready turn progress events before app-event enqueue.
///
/// PID updates are still applied as they are encountered, while repeated
/// thought deltas collapse to the newest normalized message. Completion events
/// remain ignored here because turn completion is handled by the channel
/// result.
fn coalesce_ready_turn_progress_events(
    event_rx: &mut mpsc::UnboundedReceiver<TurnEvent>,
    child_pid: &Mutex<Option<u32>>,
    initial_thought: String,
    session_id: &SessionId,
    app_event_tx: &mpsc::UnboundedSender<AppEvent>,
    activity: &mut super::activity::TurnActivity,
) -> String {
    let mut latest_thought = initial_thought;
    let mut coalesced_events = 0;

    for _ in 1..TURN_EVENT_PROGRESS_COALESCE_BUDGET {
        let Ok(event) = event_rx.try_recv() else {
            break;
        };

        coalesced_events += 1;
        match event {
            TurnEvent::Activity(event) => activity.observe_live(event, app_event_tx, session_id),
            TurnEvent::ThoughtDelta(thought) => {
                if let Some(thought) = normalize_thinking_stream_text(&thought) {
                    latest_thought = thought;
                }
            }
            TurnEvent::PidUpdate(pid) => set_child_pid(child_pid, pid),
            TurnEvent::Completed { .. } | TurnEvent::Failed(_) => {}
        }
    }

    let remaining_events = event_rx.len();
    if remaining_events > 0 {
        debug!(
            budget = TURN_EVENT_PROGRESS_COALESCE_BUDGET,
            coalesced_events,
            remaining_events,
            session_id = session_id.as_str(),
            "turn event progress coalesce budget exhausted with queued events remaining"
        );
    }

    latest_thought
}

/// Records the latest child process id observed from a provider turn stream.
fn set_child_pid(child_pid: &Mutex<Option<u32>>, pid: Option<u32>) {
    // Sync critical section (single assignment, no `.await`);
    // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
    // Selection".
    if let Ok(mut guard) = child_pid.lock() {
        *guard = pid;
    }
}

/// Converts post-turn main-checkout tracked-file changes into transcript
/// warnings while preserving the successful provider result.
///
/// When no main checkout was captured (bare shared repository), the provider
/// result passes through unchanged because there is no main checkout to guard.
async fn add_main_checkout_warning(
    context: &SessionWorkerContext,
    main_checkout_snapshot: Option<&MainCheckoutSnapshot>,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<TurnResult, AgentError> {
    let result = turn_result?;
    let Some(main_checkout_snapshot) = main_checkout_snapshot else {
        return Ok(result);
    };
    match main_checkout_snapshot.dirty_warning(context).await {
        Ok(Some(warning)) => {
            append_main_checkout_warning(context, warning).await;

            Ok(result)
        }
        Ok(None) => Ok(result),
        Err(error) => Err(AgentError::Backend(error.to_string())),
    }
}

/// Replaces the shared cancellation token for a new turn and returns the
/// token used by the running channel future.
pub(super) fn fresh_turn_cancel_token(
    context: &SessionWorkerContext,
) -> Result<CancellationToken, SessionError> {
    // Sync critical section (assignment + clone, no `.await`);
    // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
    // Selection".
    let mut guard = context
        .cancel_token
        .lock()
        .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))?;
    *guard = CancellationToken::new();

    Ok(guard.clone())
}

/// Appends one main-checkout warning to the live and persisted transcript.
async fn append_main_checkout_warning(context: &SessionWorkerContext, warning: String) {
    SessionTaskService::append_workflow_notice(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &warning,
    )
    .await;
}

/// Spawns read-only title generation while a session still has its
/// provisional title.
pub(super) async fn spawn_request_title_generation(
    input: RequestTitleInput,
    session_project_id: Option<i64>,
) {
    let RequestTitleInput {
        app_event_tx,
        available_agent_kinds,
        db,
        folder,
        prompt,
        run_client,
        session_agent,
        session_id,
    } = input;
    let title_agent = setting::load_session_utility_agent_selection_from_repositories(
        &db,
        session_project_id,
        session_agent,
        &available_agent_kinds,
    )
    .await;
    let title_reasoning_level = load_title_reasoning_level(&db, session_project_id).await;
    let title_speed_mode = setting::load_project_speed_mode_setting(
        &db,
        session_project_id,
        SettingName::DefaultFastSpeedMode,
    )
    .await;
    let title_speed_mode = if title_agent.kind().supports_speed_mode() {
        title_speed_mode
    } else {
        crate::domain::agent::SpeedMode::Normal
    };
    let title_agent = title_agent.compatible_with_speed_mode(title_speed_mode);

    let _title_generation_task =
        SessionManager::spawn_session_title_generation_task(SessionTitleGenerationTaskInput {
            app_event_tx,
            db,
            folder,
            latest_request: prompt,
            run_client,
            requires_provisional_title: true,
            reasoning_level: title_reasoning_level,
            session_agent: title_agent,
            session_id,
            speed_mode: title_speed_mode,
            tracked_generation: None,
        })
        .await;
}

/// Loads the Fast-role reasoning default used by detached title generation.
async fn load_title_reasoning_level(
    db: &AppRepositories,
    session_project_id: Option<i64>,
) -> ReasoningLevel {
    let Some(project_id) = session_project_id else {
        return ReasoningLevel::default();
    };

    db.settings()
        .load_project_reasoning_level(project_id, SettingName::DefaultFastReasoningLevel)
        .await
        .unwrap_or_default()
}

/// Returns one normalized thinking text line.
fn normalize_thinking_stream_text(text: &str) -> Option<String> {
    let trimmed_text = text.trim();
    if trimmed_text.is_empty() {
        return None;
    }

    Some(trimmed_text.to_string())
}

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
