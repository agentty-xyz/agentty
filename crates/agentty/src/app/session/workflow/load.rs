//! Session loading and derived snapshot attributes from persisted rows.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use ag_git::GitClient;
use ag_orchestration as orchestration;
use tracing::warn;

use super::{draft, session_folder};
use crate::app::session::SessionError;
use crate::app::{AppServices, SessionManager};
use crate::domain::agent::{
    AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
    parse_persisted_session_agent_model,
};
use crate::domain::permission::PermissionMode;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    DailyActivity, QueuedMessage, ReviewRequest, ReviewRequestSummary, Session, SessionDiffState,
    SessionDiffStats, SessionFollowUpTask, SessionHandles, SessionId, SessionRole, SessionSize,
    SessionStats, Status, activity_day_key_with_offset,
};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot, TransientMessageStore,
};
use crate::infra::clock::Clock;
use crate::infra::db::{
    AppRepositories, DbError, SessionDetailRow, SessionListRow, SessionMessageRow,
    SessionPreparationRow, SessionPreparationState,
};
use crate::infra::fs::FsClient;

/// Inputs required to load one project's session and activity snapshots.
pub(crate) struct SessionLoadInput<'a> {
    /// Project identifier used to scope persisted session rows.
    pub(crate) active_project_id: i64,
    /// Session whose transcript-scale details should be loaded.
    pub(crate) active_session_id: Option<&'a str>,
    /// Root directory containing Agentty-managed session worktrees.
    pub(crate) base: &'a Path,
    /// Clock used to resolve the local offset for each activity event.
    pub(crate) clock: &'a dyn Clock,
    /// Repository bundle used to load persisted session state.
    pub(crate) db: &'a AppRepositories,
    /// Filesystem boundary used to check session worktree availability.
    pub(crate) fs_client: &'a dyn FsClient,
    /// Active project directory used to derive display metadata.
    pub(crate) working_dir: &'a Path,
}

/// Mutable context threaded through the per-row session-load helper.
///
/// Keeps the per-row helper signature short while still letting it append
/// loaded sessions, mutate handles, and update worktree availability.
struct LoadSessionContext<'a> {
    active_session_id: Option<&'a str>,
    base: &'a Path,
    db: &'a AppRepositories,
    fs_client: &'a dyn FsClient,
    handles: &'a mut HashMap<SessionId, SessionHandles>,
    orchestration_metadata: &'a HashMap<String, orchestration::OrchestrationSessionMetadata>,
    preparations: &'a HashMap<String, SessionPreparationRow>,
    project_name: &'a str,
    session_worktree_availability: &'a mut HashMap<SessionId, bool>,
    sessions: &'a mut Vec<Session>,
}

/// Precomputed fields needed to assemble one loaded session snapshot.
struct LoadedSessionInput {
    controller_session_id: Option<SessionId>,
    draft_attachments: Vec<crate::domain::turn_prompt::TurnPromptAttachment>,
    folder: std::path::PathBuf,
    follow_up_tasks: Vec<SessionFollowUpTask>,
    orchestration_progress: Option<String>,
    parent_session_id: Option<SessionId>,
    permission_mode: PermissionMode,
    project_name: String,
    reasoning_level_override: Option<ReasoningLevel>,
    response_style: ResponseStyle,
    review_request: Option<ReviewRequest>,
    role: SessionRole,
    row: SessionListRow,
    session_agent: AgentSelection,
    session_id: SessionId,
    session_prompt: String,
    session_questions: Vec<QuestionItem>,
    session_queued_actions: Vec<TransientMessage>,
    session_queued_messages: Vec<QueuedMessage>,
    session_status: Status,
    session_transcript: Option<SessionTranscript>,
    size: SessionSize,
    speed_mode: SpeedMode,
}

/// Migrates every non-terminal session across all saved projects away from
/// retired persisted model ids.
///
/// Query and persistence failures are best-effort so startup remains usable
/// with a degraded database. Individual UI and API loads repeat the same
/// migration for the row they read.
pub(crate) async fn migrate_active_sessions_off_retired_models(db: &AppRepositories) {
    let Ok(rows) = db.sessions().load_active_session_agent_models().await else {
        return;
    };

    for row in rows {
        let session_status = row.status.parse::<Status>().unwrap_or(Status::Done);
        migrate_session_off_retired_model(db, &row.id, &row.agent, &row.model, session_status)
            .await;
    }
}

/// Resolves one persisted provider/model pair and persists the replacement
/// when its model is retired and the session is still active.
///
/// Terminal rows (`Merged`, `Done`, `Canceled`) keep the retired model id in
/// the database as a historical record. Persistence failures are ignored so
/// session reads still return the in-memory replacement.
pub(crate) async fn migrate_session_off_retired_model(
    db: &AppRepositories,
    session_id: &str,
    persisted_agent: &str,
    persisted_model: &str,
    session_status: Status,
) -> AgentSelection {
    let session_agent = parse_persisted_session_agent_model(Some(persisted_agent), persisted_model);
    if matches!(
        session_status,
        Status::Merged | Status::Done | Status::Canceled
    ) || AgentModel::retired_replacement(persisted_model).is_none()
    {
        return session_agent;
    }

    let session_agent_kind = session_agent.kind().to_string();
    db.sessions()
        .update_active_session_agent_model(
            session_id,
            &session_agent_kind,
            session_agent.model().as_str(),
        )
        .await
        .ok();

    session_agent
}

impl SessionManager {
    /// Registers only the newly persisted row; creation never reloads every
    /// session.
    pub(crate) async fn register_created_session(
        &mut self,
        services: &AppServices,
        session_id: &str,
        working_dir: &Path,
    ) -> Result<(), SessionError> {
        if self.session_for_id(session_id).is_some() {
            return Ok(());
        }
        let row = services
            .db()
            .sessions()
            .load_session(session_id)
            .await?
            .ok_or(SessionError::NotFound)?;
        let permission_mode = row
            .permission_mode
            .parse()
            .map_err(|_| SessionError::Workflow("Invalid session permission mode".to_string()))?;
        let metadata = orchestration::session_metadata_for_project(
            services.db(),
            row.project_id.unwrap_or_default(),
        )
        .await;
        let preparation = services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?;
        let preparations = preparation
            .into_iter()
            .map(|row| (row.session_id.clone(), row))
            .collect();
        let mut sessions = Vec::new();
        let mut availability = HashMap::new();
        let fs_client = services.fs_client();
        Self::push_loaded_session_row(
            &mut LoadSessionContext {
                active_session_id: Some(session_id),
                base: services.base_path(),
                db: services.db(),
                fs_client: fs_client.as_ref(),
                handles: self.state.handles_mut(),
                orchestration_metadata: &metadata,
                preparations: &preparations,
                project_name: working_dir
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or_default(),
                session_worktree_availability: &mut availability,
                sessions: &mut sessions,
            },
            row.into(),
            permission_mode,
        )
        .await;
        for session in sessions {
            self.state.push_session(session);
        }
        for (id, available) in availability {
            self.set_session_worktree_available(&id, available);
        }

        Ok(())
    }

    /// Shows saved-prompt readiness and setup failures without flashing a
    /// preparation notice while the user is still composing their first prompt.
    pub(crate) fn apply_workspace_preparation(
        session: &mut Session,
        preparation: Option<&SessionPreparationRow>,
    ) {
        session
            .transient_messages
            .retract(TransientMessageSlot::WorkspacePreparation);
        let Some(preparation) = preparation else {
            return;
        };
        let mut text = match preparation.state {
            SessionPreparationState::Preparing if preparation.prompt.is_none() => String::new(),
            SessionPreparationState::Preparing => "Preparing workspace. You can keep typing; \
                                                   submitted prompts wait for setup."
                .to_string(),
            SessionPreparationState::Failed => format!(
                "Workspace setup failed: {}\nPress s to retry. Your saved prompt is retained.",
                preparation.error.as_deref().unwrap_or("Unknown error")
            ),
            SessionPreparationState::Ready | SessionPreparationState::Canceled => return,
        };
        if let Some(prompt) = preparation.prompt.as_deref().and_then(|value| {
            serde_json::from_str::<crate::domain::turn_prompt::TurnPrompt>(value).ok()
        }) {
            text.push_str("\n\nSaved prompt:\n");
            text.push_str(&prompt.transcript_text());
        }
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Plain(text),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::WorkspacePreparation,
            turn_position: None,
        });
    }

    /// Loads session models from the database using the provided filesystem
    /// boundary to decide which session folders exist.
    ///
    /// Existing handles are reused in place to preserve `Arc` identity so
    /// that background workers holding cloned references continue to work.
    ///
    /// When a handle already exists, live handle output is treated as
    /// authoritative for the returned in-memory snapshot to avoid clobbering
    /// fresh runtime output with stale persisted rows. Active statuses are also
    /// preserved from live handles, while terminal persisted statuses (`Done`,
    /// `Canceled`) override stale in-memory status.
    ///
    /// Retired persisted model ids are upgraded to their current replacement
    /// models while rows are loaded. Sessions that are still active also have
    /// the replacement persisted so future turns run on the current model;
    /// terminal sessions keep their retired model id in the database as a
    /// historical record.
    ///
    /// New handles are inserted for sessions that don't have entries yet.
    ///
    /// Transcript-scale fields are loaded only for `active_session_id`; other
    /// rows receive empty detail fields until the session is opened.
    ///
    /// Rows with unsupported permission modes are logged and skipped so one
    /// corrupt session cannot hide valid siblings or appear write-capable.
    ///
    /// Returns loaded sessions, local-day activity counts aggregated from
    /// persisted session-creation activity history, and cached worktree
    /// availability keyed by session id.
    pub(crate) async fn load_sessions_with_fs_client(
        input: SessionLoadInput<'_>,
        handles: &mut HashMap<SessionId, SessionHandles>,
    ) -> (Vec<Session>, Vec<DailyActivity>, HashMap<SessionId, bool>) {
        Self::try_load_sessions_with_fs_client(input, handles)
            .await
            .unwrap_or_default()
    }

    /// Loads session snapshots while preserving a session-list read failure.
    ///
    /// Refresh callers use this fallible path so a transient database error
    /// cannot be mistaken for an empty project and tear down live workers.
    ///
    /// # Errors
    /// Returns an error when the project's session rows cannot be loaded.
    pub(crate) async fn try_load_sessions_with_fs_client(
        input: SessionLoadInput<'_>,
        handles: &mut HashMap<SessionId, SessionHandles>,
    ) -> Result<(Vec<Session>, Vec<DailyActivity>, HashMap<SessionId, bool>), DbError> {
        let SessionLoadInput {
            active_project_id,
            active_session_id,
            base,
            clock,
            db,
            fs_client,
            working_dir,
        } = input;
        let project_name = working_dir
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_string();

        let db_rows = db
            .sessions()
            .load_sessions_for_project(active_project_id)
            .await?;
        let activity_timestamps = db
            .activity()
            .load_session_activity_timestamps()
            .await
            .unwrap_or_default();
        let stats_activity = Self::daily_activity_from_timestamps(activity_timestamps, clock);
        let orchestration_metadata =
            orchestration::session_metadata_for_project(db, active_project_id).await;
        let mut sessions: Vec<Session> = Vec::new();
        let mut session_worktree_availability = HashMap::new();

        let preparations = db
            .sessions()
            .load_session_preparations(active_project_id)
            .await?
            .into_iter()
            .map(|row| (row.session_id.clone(), row))
            .collect();
        let mut load_context = LoadSessionContext {
            preparations: &preparations,
            base,
            db,
            project_name: &project_name,
            handles,
            fs_client,
            active_session_id,
            orchestration_metadata: &orchestration_metadata,
            sessions: &mut sessions,
            session_worktree_availability: &mut session_worktree_availability,
        };
        for row in db_rows {
            let Ok(permission_mode) = row.permission_mode.parse() else {
                warn!(
                    session_id = %row.id,
                    permission_mode = %row.permission_mode,
                    "skipping session with unsupported permission mode"
                );

                continue;
            };
            Self::push_loaded_session_row(&mut load_context, row, permission_mode).await;
        }

        Ok((sessions, stats_activity, session_worktree_availability))
    }

    /// Aggregates persisted activity timestamps using the clock-provided
    /// offset active for each event.
    fn daily_activity_from_timestamps(
        timestamps: Vec<i64>,
        clock: &dyn Clock,
    ) -> Vec<DailyActivity> {
        let mut activity_by_day = BTreeMap::<i64, u32>::new();
        for timestamp_seconds in timestamps {
            let utc_offset_seconds = clock.local_utc_offset_seconds(timestamp_seconds);
            let day_key = activity_day_key_with_offset(timestamp_seconds, utc_offset_seconds);
            let session_count = activity_by_day.entry(day_key).or_default();
            *session_count = session_count.saturating_add(1);
        }

        activity_by_day
            .into_iter()
            .map(|(day_key, session_count)| DailyActivity {
                day_key,
                session_count,
            })
            .collect()
    }

    /// Loads one persisted session row into `sessions`, reusing existing
    /// handles when present and registering a new handle otherwise.
    async fn push_loaded_session_row(
        load_context: &mut LoadSessionContext<'_>,
        row: SessionListRow,
        permission_mode: PermissionMode,
    ) {
        let LoadSessionContext {
            base,
            db,
            project_name,
            handles,
            orchestration_metadata,
            fs_client,
            active_session_id,
            sessions,
            session_worktree_availability,
            preparations,
        } = load_context;
        let session_id = SessionId::from(row.id.clone());
        let folder = session_folder(base, &session_id);
        let persisted_status = row.status.parse::<Status>().unwrap_or(Status::Done);
        let persisted_size = row.size.parse::<SessionSize>().unwrap_or_default();
        let has_session_folder = fs_client.is_dir(folder.clone());
        let live_handle_status = handles
            .get(&session_id)
            .and_then(|existing| existing.status.lock().ok().map(|status| *status));

        if should_skip_missing_folder_session(
            has_session_folder || preparations.contains_key(&row.id),
            row.is_draft,
            persisted_status,
            live_handle_status,
        ) {
            return;
        }

        let workspace_ready = preparations
            .get(&row.id)
            .is_none_or(|preparation| preparation.state == SessionPreparationState::Ready);
        session_worktree_availability
            .insert(session_id.clone(), has_session_folder && workspace_ready);

        let (session_detail, session_status, session_transcript) =
            Self::load_session_detail_and_transcript(
                db,
                *active_session_id,
                handles,
                &session_id,
                &row.id,
                persisted_status,
            )
            .await;
        let session_agent =
            migrate_session_off_retired_model(db, &row.id, &row.agent, &row.model, session_status)
                .await;
        let draft_attachments =
            draft::load_staged_draft_attachments(*fs_client, base, &session_id).await;
        let questions = Self::loaded_session_questions(session_detail.as_ref());
        let reasoning_level_override = row
            .reasoning_level_override
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok());
        let speed_mode = row.speed_mode.parse::<SpeedMode>().unwrap_or_default();
        let response_style = row
            .response_style
            .parse::<ResponseStyle>()
            .unwrap_or_default();
        let (session_queued_messages, session_queued_actions) =
            Self::loaded_queue_snapshots(handles.get(&session_id));
        let (role, orchestration_metadata) =
            Self::loaded_orchestration_metadata(&row, orchestration_metadata);
        let preparation = preparations.get(&row.id);
        let mut session = Self::build_loaded_session(LoadedSessionInput {
            controller_session_id: orchestration_metadata.controller_session_id,
            draft_attachments,
            follow_up_tasks: Vec::new(),
            folder,
            parent_session_id: row.parent_session_id.clone().map(SessionId::from),
            orchestration_progress: orchestration_metadata.progress,
            permission_mode,
            project_name: (*project_name).to_string(),
            reasoning_level_override,
            response_style,
            review_request: parse_review_request(&row),
            role,
            row,
            session_agent,
            session_id,
            session_prompt: session_detail
                .as_ref()
                .map(|detail| detail.prompt.clone())
                .unwrap_or_default(),
            session_queued_actions,
            session_queued_messages,
            session_questions: questions,
            session_status,
            session_transcript,
            size: persisted_size,
            speed_mode,
        });
        Self::apply_workspace_preparation(&mut session, preparation);
        sessions.push(session);
    }

    async fn load_session_detail_and_transcript(
        db: &AppRepositories,
        active_session_id: Option<&str>,
        handles: &mut HashMap<SessionId, SessionHandles>,
        session_id: &SessionId,
        row_id: &str,
        persisted_status: Status,
    ) -> (Option<SessionDetailRow>, Status, Option<SessionTranscript>) {
        let (session_detail, loaded_transcript) =
            load_active_session_detail(db, active_session_id, row_id).await;
        let (session_status, session_transcript) =
            if let Some(existing_handle) = handles.get(session_id) {
                status_and_transcript_from_existing_handle(
                    existing_handle,
                    persisted_status,
                    loaded_transcript.as_ref(),
                )
            } else {
                let transcript = insert_loaded_session_handle(
                    handles,
                    session_id.clone(),
                    persisted_status,
                    loaded_transcript,
                );

                (persisted_status, transcript)
            };

        (session_detail, session_status, session_transcript)
    }

    fn loaded_session_questions(session_detail: Option<&SessionDetailRow>) -> Vec<QuestionItem> {
        session_detail
            .and_then(|detail| detail.questions.as_deref())
            .and_then(parse_questions_json)
            .unwrap_or_default()
    }

    fn loaded_queue_snapshots(
        handles: Option<&SessionHandles>,
    ) -> (Vec<QueuedMessage>, Vec<TransientMessage>) {
        handles
            .map(|handles| {
                (
                    handles.queued_message_snapshot(),
                    handles.queued_action_snapshot(),
                )
            })
            .unwrap_or_default()
    }

    fn loaded_orchestration_metadata(
        row: &SessionListRow,
        metadata: &HashMap<String, orchestration::OrchestrationSessionMetadata>,
    ) -> (SessionRole, orchestration::OrchestrationSessionMetadata) {
        let role = row
            .role
            .as_deref()
            .and_then(|value| value.parse::<SessionRole>().ok())
            .unwrap_or_default();
        let metadata = metadata.get(&row.id).cloned().unwrap_or_default();

        (role, metadata)
    }

    /// Computes diff-derived session metadata from one worktree folder using
    /// the injected filesystem and Git boundaries.
    ///
    /// Missing folders and Git failures return [`SessionDiffStats::Unknown`]
    /// so callers retain diagnostic diff access without overwriting the last
    /// known line totals.
    pub(crate) async fn session_diff_stats_for_folder(
        fs_client: &dyn FsClient,
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
    ) -> SessionDiffStats {
        if !fs_client.is_dir(folder.to_path_buf()) {
            return SessionDiffStats::Unknown;
        }

        let folder = folder.to_path_buf();
        let base_branch = base_branch.to_string();
        let Ok(diff) = git_client.diff(folder, base_branch).await else {
            return SessionDiffStats::Unknown;
        };

        SessionDiffStats::from_diff(&diff)
    }

    /// Loads transcript-scale detail for one session into the in-memory
    /// snapshot and runtime handles when the user opens that session.
    pub(crate) async fn load_session_detail_into_state(
        &mut self,
        db: &AppRepositories,
        session_id: &str,
    ) {
        let Some(detail) = db
            .sessions()
            .load_session_detail(session_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Ok(transcript) = load_session_transcript(db, session_id).await else {
            return;
        };

        self.apply_session_detail(session_id, detail, transcript);
    }

    /// Builds one in-memory session snapshot from a database row plus the
    /// transient fields computed during reload.
    fn build_loaded_session(input: LoadedSessionInput) -> Session {
        let mut session = Session {
            agent: input.session_agent,
            base_branch: input.row.base_branch,
            created_at: input.row.created_at,
            controller_session_id: input.controller_session_id,
            draft_attachments: input.draft_attachments,
            folder: input.folder,
            follow_up_tasks: input.follow_up_tasks,
            id: input.session_id,
            in_progress_started_at: input.row.in_progress_started_at,
            in_progress_total_seconds: input.row.in_progress_total_seconds,
            is_draft: input.row.is_draft,
            orchestration_progress: input.orchestration_progress,
            parent_session_id: input.parent_session_id,
            permission_mode: input.permission_mode,
            personality_id: input.row.personality_id,
            project_name: input.project_name,
            prompt: input.session_prompt,
            queued_messages: input.session_queued_messages,
            reasoning_level_override: input.reasoning_level_override,
            response_style: input.response_style,
            published_upstream_ref: input.row.published_upstream_ref,
            questions: input.session_questions,
            review_request: input.review_request,
            role: input.role,
            size: input.size,
            speed_mode: input.speed_mode,
            stats: SessionStats {
                added_lines: input.row.added_lines.cast_unsigned(),
                deleted_lines: input.row.deleted_lines.cast_unsigned(),
                diff_state: match input.row.has_diff {
                    Some(true) => SessionDiffState::Present,
                    Some(false) => SessionDiffState::Empty,
                    None => SessionDiffState::Unknown,
                },
                input_tokens: input.row.input_tokens.cast_unsigned(),
                output_tokens: input.row.output_tokens.cast_unsigned(),
            },
            status: input.session_status,
            title: input.row.title,
            transcript: input.session_transcript,
            updated_at: input.row.updated_at,
            transient_messages: TransientMessageStore::default(),
        };
        for queued_action in input.session_queued_actions {
            session.transient_messages.upsert(queued_action);
        }
        session
    }

    /// Applies one lazily loaded detail row and message transcript to the
    /// session snapshot and its shared runtime handle without clobbering live
    /// in-process transcript messages.
    fn apply_session_detail(
        &mut self,
        session_id: &str,
        detail: SessionDetailRow,
        transcript: SessionTranscript,
    ) {
        let session_transcript = self
            .state
            .handle(session_id)
            .and_then(|handles| sync_handle_transcript_with_loaded(handles, Some(&transcript)))
            .or_else(|| Some(transcript).filter(|transcript| !transcript.is_empty()));

        let Some(session) = self.state.session_mut_for_id(session_id) else {
            return;
        };

        session.prompt = detail.prompt;
        if let Some(questions) = detail.questions {
            session.questions = parse_questions_json(&questions).unwrap_or_default();
        }
        session.transcript = session_transcript;
    }
}

/// Loads active-session detail metadata and transcript text for the selected
/// row only.
async fn load_active_session_detail(
    db: &AppRepositories,
    active_session_id: Option<&str>,
    row_id: &str,
) -> (Option<SessionDetailRow>, Option<SessionTranscript>) {
    if active_session_id.is_none_or(|active_id| active_id != row_id) {
        return (None, None);
    }

    let Some(detail) = db
        .sessions()
        .load_session_detail(row_id)
        .await
        .ok()
        .flatten()
    else {
        return (None, None);
    };
    let transcript = load_session_transcript(db, row_id).await.ok();

    (Some(detail), transcript)
}

/// Reads status/transcript from an existing handle while hydrating an empty
/// transcript from lazily loaded detail when the session has become active.
fn status_and_transcript_from_existing_handle(
    existing_handle: &SessionHandles,
    persisted_status: Status,
    loaded_transcript: Option<&SessionTranscript>,
) -> (Status, Option<SessionTranscript>) {
    let status_from_handle = existing_handle
        .status
        .lock()
        .ok()
        .map_or(persisted_status, |status| *status);
    let merged_status = merge_loaded_session_status(persisted_status, status_from_handle);

    if let Ok(mut handle_status) = existing_handle.status.lock() {
        *handle_status = merged_status;
    }
    let transcript_from_handle =
        sync_handle_transcript_with_loaded(existing_handle, loaded_transcript);

    (merged_status, transcript_from_handle)
}

/// Inserts a new runtime handle using active-session detail when it is
/// available and returns the transcript snapshot stored in that handle.
fn insert_loaded_session_handle(
    handles: &mut HashMap<SessionId, SessionHandles>,
    session_id: SessionId,
    persisted_status: Status,
    loaded_transcript: Option<SessionTranscript>,
) -> Option<SessionTranscript> {
    let session_transcript = loaded_transcript.filter(|transcript| !transcript.is_empty());
    let session_handle = if let Some(transcript) = session_transcript.clone() {
        SessionHandles::new_with_transcript(persisted_status, transcript)
    } else {
        SessionHandles::new_unloaded(persisted_status)
    };
    handles.insert(session_id, session_handle);

    session_transcript
}

/// Loads ordered session messages into the render transcript snapshot.
pub(crate) async fn load_session_transcript(
    db: &AppRepositories,
    session_id: &str,
) -> Result<SessionTranscript, DbError> {
    let messages = db.sessions().load_session_messages(session_id).await?;

    Ok(SessionTranscript::new(session_messages_from_rows(messages)))
}

/// Synchronizes a handle from loaded rows while preserving complete live
/// transcripts and replacing partial unhydrated snapshots.
fn sync_handle_transcript_with_loaded(
    handles: &SessionHandles,
    loaded_transcript: Option<&SessionTranscript>,
) -> Option<SessionTranscript> {
    handles.transcript_snapshot_with_loaded(loaded_transcript)
}

/// Converts database message rows into domain messages, skipping unknown
/// message kinds left by older database revisions.
fn session_messages_from_rows(rows: Vec<SessionMessageRow>) -> Vec<SessionMessage> {
    rows.into_iter()
        .filter_map(|row| {
            row.kind
                .parse::<SessionMessageKind>()
                .ok()
                .map(|kind| SessionMessage::new(row.position, kind, row.content))
        })
        .collect()
}

/// Returns whether one persisted session row should be skipped because its
/// worktree folder is missing and no merge-cleanup transition is still active.
fn should_skip_missing_folder_session(
    has_session_folder: bool,
    is_draft_session: bool,
    persisted_status: Status,
    live_handle_status: Option<Status>,
) -> bool {
    if has_session_folder {
        return false;
    }

    if matches!(
        persisted_status,
        Status::Merged | Status::Done | Status::Canceled
    ) {
        return false;
    }

    if is_draft_session && persisted_status == Status::Draft {
        return false;
    }

    !matches!(
        live_handle_status,
        Some(Status::Merging | Status::Merged | Status::Done | Status::Canceled)
    )
}

/// Merges one loaded status with the existing live-handle status.
///
/// Existing handle status is kept for active transitions to prevent stale DB
/// snapshots from clobbering in-memory updates. Persisted read-only and
/// terminal statuses (`Merged`, `Done`, `Canceled`) take precedence so remote
/// merge truth and explicit terminal transitions still appear after refresh.
fn merge_loaded_session_status(status_from_db: Status, status_from_handle: Status) -> Status {
    if matches!(
        status_from_db,
        Status::Merged | Status::Done | Status::Canceled
    ) {
        return status_from_db;
    }

    status_from_handle
}

/// Parses normalized review-request metadata from one loaded database row.
///
/// Incomplete or invalid persisted metadata is ignored so stale partial rows do
/// not block session loading.
fn parse_review_request(row: &SessionListRow) -> Option<ReviewRequest> {
    let review_request_row = row.review_request.as_ref()?;
    let forge_kind = parse_optional_enum(Some(review_request_row.forge_kind.as_str())).ok()?;
    let state = parse_optional_enum(Some(review_request_row.state.as_str())).ok()?;

    Some(ReviewRequest {
        last_refreshed_at: review_request_row.last_refreshed_at,
        summary: ReviewRequestSummary {
            display_id: review_request_row.display_id.clone(),
            forge_kind,
            source_branch: review_request_row.source_branch.clone(),
            state,
            status_summary: review_request_row.status_summary.clone(),
            target_branch: review_request_row.target_branch.clone(),
            title: review_request_row.title.clone(),
            web_url: review_request_row.web_url.clone(),
        },
    })
}

/// Converts one optional persisted string into a parsed enum value.
fn parse_optional_enum<T>(value: Option<&str>) -> Result<T, ()>
where
    T: std::str::FromStr,
{
    value.ok_or(())?.parse().map_err(|_| ())
}

/// Parses persisted question JSON with backward compatibility.
///
/// Attempts to deserialize as `Vec<QuestionItem>` first (new format). Falls
/// back to `Vec<String>` (legacy format) and converts each entry into a
/// `QuestionItem` without predefined options.
fn parse_questions_json(raw_json: &str) -> Option<Vec<QuestionItem>> {
    if raw_json.is_empty() {
        return None;
    }

    if let Ok(items) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Some(items);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .ok()
        .map(|texts| {
            texts
                .into_iter()
                .map(|text| QuestionItem {
                    options: Vec::new(),
                    text,
                })
                .collect()
        })
}

#[cfg(test)]
#[path = "load_test.rs"]
mod tests;
