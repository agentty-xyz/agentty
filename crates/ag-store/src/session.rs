//! Session-scoped persistence adapters and query helpers.

use std::sync::Arc;

use ag_agent::{
    self as agent, AgentKind, PermissionMode, ReasoningLevel, ResponseStyle, SessionStats,
    SpeedMode,
};
use ag_session::{FocusedReviewStatus, SessionMessageKind};
use async_trait::async_trait;
use sqlx::SqlitePool;
use tracing::warn;

use crate::review::{
    NewSessionReviewCommentResolution, SessionReviewRequestRow, insert_review_comment_resolutions,
};
use crate::session_message::SessionMessageStore;
use crate::session_snapshot::SessionSnapshotStore;
use crate::timestamp::TimestampSource;
use crate::{DbError, status};

/// Transactional turn-metadata payload persisted after one completed agent
/// turn.
///
/// Owns its fields so the persistence trait method stays lifetime-free. A
/// borrowed variant (`SessionTurnMetadata<'a>`) forced the persist method to
/// carry a generic lifetime, which `mockall::automock` drops in the generated
/// mock and newer `clippy` then rejects via `extra_unused_lifetimes`. Owning
/// the data is allocation-cheap on this once-per-turn path and keeps the trait
/// signature stable across toolchains.
pub struct SessionTurnMetadata {
    /// Personality id successfully delivered for this turn, or `None` when
    /// the turn cleared or had no personality.
    pub applied_personality_id: Option<String>,
    /// Fingerprint of the personality prompt successfully delivered for this
    /// turn.
    pub applied_personality_prompt_hash: Option<String>,
    /// Session-scoped instruction bootstrap marker for app-server providers.
    pub instruction_conversation_id: Option<String>,
    /// Model identifier used for per-model usage aggregation.
    pub model: String,
    /// Persisted provider-native conversation identifier for future resumes.
    pub provider_conversation_id: Option<String>,
    /// Serialized clarification-question payload stored on the session row.
    pub questions_json: String,
    /// Review-comment operations committed with this completed turn.
    pub review_comment_resolutions: Vec<NewSessionReviewCommentResolution>,
    /// Token-usage delta attributed to the completed turn.
    pub token_usage_delta: SessionStats,
}

/// Borrowed values used to persist a newly created session with explicit
/// provider identity and reasoning configuration.
pub struct PersistedSessionCreation<'a> {
    /// Persisted agent provider kind for the session.
    pub agent: &'a str,
    /// Base branch or parent branch used for future worktree materialization.
    pub base_branch: &'a str,
    /// Stable session identifier.
    pub id: &'a str,
    /// Whether the row was created through explicit draft staging.
    pub is_draft: bool,
    /// Persisted model identifier for the session.
    pub model: &'a str,
    /// Orchestration task that owns this child session, when applicable.
    pub orchestration_task_id: Option<i64>,
    /// Optional parent session id for one-level stacked drafts.
    pub parent_session_id: Option<&'a str>,
    /// Provider permission mode captured for the session.
    pub permission_mode: PermissionMode,
    /// Workspace personality selected for future turns, when present.
    pub personality_id: Option<&'a str>,
    /// Owning project identifier.
    pub project_id: i64,
    /// Reasoning level captured from the project default at creation.
    pub reasoning_level: ReasoningLevel,
    /// Response style captured from the project default at creation.
    pub response_style: ResponseStyle,
    /// Persisted session role, or `None` for the default worker role.
    pub role: Option<&'a str>,
    /// Response-speed preference captured for the session.
    pub speed_mode: SpeedMode,
    /// Initial lifecycle status string.
    pub status: &'a str,
}

/// Borrowed identifiers used to persist one forked session snapshot.
pub struct ForkSessionSnapshot<'a> {
    /// Stable id assigned to the newly forked session.
    pub new_session_id: &'a str,
    /// Stable id of the source session whose metadata and transcript are
    /// copied.
    pub source_session_id: &'a str,
    /// Initial lifecycle status for the forked session.
    pub status: &'a str,
}

/// Row returned when loading a session from the `session` table.
///
/// Includes optional normalized forge review-request linkage metadata loaded
/// through the `session_review_request` table when the session has been
/// published for remote review.
pub struct SessionRow {
    /// Persisted added-line count from the latest diff stats refresh.
    pub added_lines: i64,
    /// Persisted agent provider kind selected for this session.
    pub agent: String,
    /// Base branch used to create the session worktree.
    pub base_branch: String,
    /// Session creation timestamp in Unix seconds.
    pub created_at: i64,
    /// Persisted deleted-line count from the latest diff stats refresh.
    pub deleted_lines: i64,
    /// Whether the latest successful diff refresh returned content, or
    /// `None` when diff availability is unknown.
    pub has_diff: Option<bool>,
    /// Stable session identifier.
    pub id: String,
    /// Open active-work interval start timestamp, if any.
    pub in_progress_started_at: Option<i64>,
    /// Completed active-work duration in whole seconds.
    pub in_progress_total_seconds: i64,
    /// Total input tokens accumulated for the session.
    pub input_tokens: i64,
    /// Whether the session is still an explicit draft.
    pub is_draft: bool,
    /// Persisted agent model identifier.
    pub model: String,
    /// Total output tokens accumulated for the session.
    pub output_tokens: i64,
    /// Parent session id when this is a one-level stacked draft.
    pub parent_session_id: Option<String>,
    /// Persisted provider permission mode for future turns.
    pub permission_mode: String,
    /// Workspace personality selected for future turns, when present.
    pub personality_id: Option<String>,
    /// Owning project identifier, when present.
    pub project_id: Option<i64>,
    /// Initial or staged prompt text.
    pub prompt: String,
    /// Published upstream branch reference, when present.
    pub published_upstream_ref: Option<String>,
    /// Serialized clarification-question payload, when present.
    pub questions: Option<String>,
    /// Persisted session-specific reasoning override, when present.
    pub reasoning_level_override: Option<String>,
    /// Persisted session response style.
    pub response_style: String,
    /// Joined forge review-request metadata, when present and complete.
    pub review_request: Option<SessionReviewRequestRow>,
    /// Persisted session role string, or `None` for the default worker role.
    pub role: Option<String>,
    /// Persisted size bucket string.
    pub size: String,
    /// Persisted session response-speed preference.
    pub speed_mode: String,
    /// Persisted lifecycle status string.
    pub status: String,
    /// Optional display title.
    pub title: Option<String>,
    /// Last update timestamp in Unix seconds.
    pub updated_at: i64,
}

/// Lightweight row returned when loading session-list metadata.
///
/// Omits transcript-scale fields (`prompt` and `questions`) so
/// list refreshes scale with visible metadata instead of the cumulative size
/// of every saved conversation.
pub struct SessionListRow {
    /// Persisted added-line count from the latest diff stats refresh.
    pub added_lines: i64,
    /// Persisted agent provider kind for this session.
    pub agent: String,
    /// Base branch used to create the session worktree.
    pub base_branch: String,
    /// Session creation timestamp in Unix seconds.
    pub created_at: i64,
    /// Persisted deleted-line count from the latest diff stats refresh.
    pub deleted_lines: i64,
    /// Whether the latest successful diff refresh returned content, or
    /// `None` when diff availability is unknown.
    pub has_diff: Option<bool>,
    /// Stable session identifier.
    pub id: String,
    /// Open active-work interval start timestamp, if any.
    pub in_progress_started_at: Option<i64>,
    /// Completed active-work duration in whole seconds.
    pub in_progress_total_seconds: i64,
    /// Total input tokens accumulated for the session.
    pub input_tokens: i64,
    /// Whether the session is still an explicit draft.
    pub is_draft: bool,
    /// Persisted agent model identifier.
    pub model: String,
    /// Total output tokens accumulated for the session.
    pub output_tokens: i64,
    /// Parent session id when this row is a one-level stacked draft.
    pub parent_session_id: Option<String>,
    /// Persisted provider permission mode for future turns.
    pub permission_mode: String,
    /// Workspace personality selected for future turns, when present.
    pub personality_id: Option<String>,
    /// Owning project identifier, when present.
    pub project_id: Option<i64>,
    /// Published upstream branch reference, when present.
    pub published_upstream_ref: Option<String>,
    /// Persisted session-specific reasoning override, when present.
    pub reasoning_level_override: Option<String>,
    /// Persisted session response style.
    pub response_style: String,
    /// Joined forge review-request metadata, when present and complete.
    pub review_request: Option<SessionReviewRequestRow>,
    /// Persisted session role string, or `None` for the default worker role.
    pub role: Option<String>,
    /// Persisted size bucket string.
    pub size: String,
    /// Persisted session response-speed preference.
    pub speed_mode: String,
    /// Persisted lifecycle status string.
    pub status: String,
    /// Optional display title.
    pub title: Option<String>,
    /// Last update timestamp in Unix seconds.
    pub updated_at: i64,
}

impl From<SessionRow> for SessionListRow {
    fn from(row: SessionRow) -> Self {
        Self {
            added_lines: row.added_lines,
            agent: row.agent,
            base_branch: row.base_branch,
            created_at: row.created_at,
            deleted_lines: row.deleted_lines,
            has_diff: row.has_diff,
            id: row.id,
            in_progress_started_at: row.in_progress_started_at,
            in_progress_total_seconds: row.in_progress_total_seconds,
            input_tokens: row.input_tokens,
            is_draft: row.is_draft,
            model: row.model,
            output_tokens: row.output_tokens,
            parent_session_id: row.parent_session_id,
            permission_mode: row.permission_mode,
            personality_id: row.personality_id,
            project_id: row.project_id,
            published_upstream_ref: row.published_upstream_ref,
            reasoning_level_override: row.reasoning_level_override,
            response_style: row.response_style,
            review_request: row.review_request,
            role: row.role,
            size: row.size,
            speed_mode: row.speed_mode,
            status: row.status,
            title: row.title,
            updated_at: row.updated_at,
        }
    }
}

/// Minimal provider/model row used to migrate active sessions across projects.
#[derive(sqlx::FromRow)]
pub struct SessionAgentModelRow {
    /// Persisted agent provider kind for this session.
    pub agent: String,
    /// Stable session identifier.
    pub id: String,
    /// Persisted agent model identifier.
    pub model: String,
    /// Persisted lifecycle status string.
    pub status: String,
}

/// Transcript-detail row loaded lazily for the session being viewed.
pub struct SessionDetailRow {
    /// Initial or staged prompt text.
    pub prompt: String,
    /// Serialized clarification-question payload, when present.
    pub questions: Option<String>,
}

/// Row returned when loading one persisted `session_message`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMessageRow {
    /// Canonical transcript text for this message.
    pub content: String,
    /// Stable message-kind string.
    pub kind: String,
    /// Monotonic position within the owning session transcript.
    pub position: i64,
}

/// Row returned when hydrating persisted focused-review cache entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFocusedReviewRow {
    /// Diff-content hash captured when the focused review was generated.
    pub diff_hash: String,
    /// Stable session identifier.
    pub session_id: String,
    /// Generated focused-review markdown text.
    pub text: String,
}

/// Persisted selected and successfully applied personality state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPersonalityState {
    /// Personality id delivered during the latest successful turn.
    pub applied_personality_id: Option<String>,
    /// Fingerprint of the personality prompt delivered during that turn.
    pub applied_personality_prompt_hash: Option<String>,
    /// Personality id selected for the session's next turn.
    pub personality_id: Option<String>,
}

/// Session-focused persistence boundary used by app orchestration and tests.
#[async_trait]
pub trait SessionRepository: crate::SessionPreparationRepository + Send + Sync {
    /// Appends one typed transcript message and refreshes session ordering
    /// metadata.
    async fn append_session_message(
        &self,
        id: &str,
        kind: SessionMessageKind,
        content: &str,
    ) -> Result<(), DbError>;

    /// Sets `project_id` for sessions that do not yet reference a project.
    async fn backfill_session_project(&self, project_id: i64) -> Result<(), DbError>;

    /// Persists an automatic focused-review trigger when the session still
    /// exists and is eligible for a worker review.
    async fn defer_session_focused_review(&self, id: &str) -> Result<bool, DbError>;

    /// Deletes a session row by identifier.
    async fn delete_session(&self, id: &str) -> Result<(), DbError>;

    /// Returns the persisted base branch for a session, when present.
    async fn get_session_base_branch(&self, id: &str) -> Result<Option<String>, DbError>;

    /// Returns the parent session id for a stacked session, when present.
    async fn get_session_parent_session_id(&self, id: &str) -> Result<Option<String>, DbError>;

    /// Returns the parent/base commit hash that a stacked child branch was
    /// last known to contain.
    async fn get_session_stack_base_commit_hash(&self, id: &str)
    -> Result<Option<String>, DbError>;

    /// Returns the persisted app-server instruction bootstrap marker for a
    /// session, when present.
    async fn get_session_instruction_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Returns the provider conversation identifier for a session, when
    /// present.
    async fn get_session_provider_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Inserts a newly created draft-session row.
    async fn insert_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError>;

    /// Inserts a newly created stacked draft-session row.
    async fn insert_stacked_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        parent_session_id: &str,
        project_id: i64,
    ) -> Result<(), DbError>;

    /// Inserts a newly created session row.
    async fn insert_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError>;

    /// Inserts a newly created session row with explicit provider identity.
    async fn insert_session_with_agent(
        &self,
        session: PersistedSessionCreation<'_>,
    ) -> Result<(), DbError>;

    /// Inserts a new session by snapshotting source metadata and ordered
    /// transcript messages while clearing source-specific runtime linkage.
    async fn fork_session_snapshot(&self, snapshot: ForkSessionSnapshot<'_>)
    -> Result<(), DbError>;

    /// Atomically snapshots a fork and its frozen workspace start commit.
    async fn reserve_fork_session_snapshot(
        &self,
        snapshot: ForkSessionSnapshot<'_>,
        start_ref: &str,
    ) -> Result<(), DbError>;

    /// Loads one complete persisted session row by stable identifier.
    async fn load_session(&self, session_id: &str) -> Result<Option<SessionRow>, DbError>;

    /// Loads provider/model metadata for every non-terminal session across
    /// projects.
    async fn load_active_session_agent_models(&self) -> Result<Vec<SessionAgentModelRow>, DbError>;

    /// Loads all sessions ordered by most recent update.
    async fn load_sessions(&self) -> Result<Vec<SessionRow>, DbError>;

    /// Loads lightweight session-list metadata ordered by most recent update
    /// for one project.
    async fn load_sessions_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionListRow>, DbError>;

    /// Loads transcript-scale detail for one session when it becomes active.
    async fn load_session_detail(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionDetailRow>, DbError>;

    /// Loads ordered transcript messages for one session.
    async fn load_session_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessageRow>, DbError>;

    /// Loads persisted focused-review cache rows for one project.
    async fn load_session_focused_reviews_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionFocusedReviewRow>, DbError>;

    /// Loads lightweight session metadata used for cheap change detection.
    async fn load_sessions_metadata(&self) -> Result<(i64, i64), DbError>;

    /// Loads the project identifier associated with one session.
    async fn load_session_project_id(&self, session_id: &str) -> Result<Option<i64>, DbError>;

    /// Loads selected and last-applied personality state for one session.
    async fn load_session_personality_state(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionPersonalityState>, DbError>;

    /// Loads parentless review-ready sessions that still need their recorded
    /// stack-base commit replayed onto their current base branch.
    async fn load_pending_stack_restack_session_ids(
        &self,
        project_id: i64,
    ) -> Result<Vec<String>, DbError>;

    /// Loads eligible worker sessions with a durable automatic focused-review
    /// trigger for one project.
    async fn load_pending_focused_review_session_ids(
        &self,
        project_id: i64,
    ) -> Result<Vec<String>, DbError>;

    /// Returns the persisted upstream reference for a published session
    /// branch, when present.
    async fn load_session_published_upstream_ref(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Loads the persisted merged commit hash for one session, when present.
    async fn load_session_merged_commit_hash(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Loads the immutable diff archived before managed-session cleanup.
    async fn load_session_archived_diff(&self, session_id: &str)
    -> Result<Option<String>, DbError>;

    /// Clears parent links for children after their parent session merges
    /// into its base branch, returning materialized children that may need a
    /// follow-up branch restack.
    async fn restack_child_sessions_after_parent_merge(
        &self,
        parent_session_id: &str,
        base_branch: &str,
        parent_commit_hash: Option<String>,
    ) -> Result<Vec<String>, DbError>;

    /// Loads the persisted session reasoning level.
    async fn load_session_reasoning_level(
        &self,
        session_id: &str,
    ) -> Result<ReasoningLevel, DbError>;

    /// Loads the persisted session response style.
    async fn load_session_response_style(&self, session_id: &str)
    -> Result<ResponseStyle, DbError>;

    /// Loads the persisted provider permission mode for future turns.
    async fn load_session_permission_mode(
        &self,
        session_id: &str,
    ) -> Result<PermissionMode, DbError>;

    /// Loads the persisted session response-speed preference.
    async fn load_session_speed_mode(&self, session_id: &str) -> Result<SpeedMode, DbError>;

    /// Returns `(created_at, updated_at)` timestamps for a session.
    async fn load_session_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, i64)>, DbError>;

    /// Persists all canonical turn metadata for one completed agent turn in a
    /// single transaction.
    async fn persist_session_turn_metadata(
        &self,
        session_id: &str,
        turn_metadata: &SessionTurnMetadata,
    ) -> Result<(), DbError>;

    /// Marks persisted diff availability unknown while retaining the last
    /// known size and line counts.
    async fn mark_session_diff_unknown(&self, id: &str) -> Result<(), DbError>;

    /// Updates persisted diff-derived presence, size, and line-count fields
    /// for a session row.
    async fn update_session_diff_stats(
        &self,
        added_lines: u64,
        deleted_lines: u64,
        has_diff: bool,
        id: &str,
        size: &str,
    ) -> Result<(), DbError>;

    /// Updates the persisted app-server instruction bootstrap marker for a
    /// session.
    async fn update_session_instruction_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the persisted model for a session.
    async fn update_session_model(&self, id: &str, model: &str) -> Result<(), DbError>;

    /// Updates or clears the personality selected for future turns.
    async fn update_session_personality_id(
        &self,
        id: &str,
        personality_id: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the persisted agent provider and model for a session.
    async fn update_session_agent_model(
        &self,
        id: &str,
        agent: &str,
        model: &str,
    ) -> Result<(), DbError>;

    /// Updates the persisted agent provider and model only while the session
    /// remains non-terminal, without changing its activity timestamp.
    async fn update_active_session_agent_model(
        &self,
        id: &str,
        agent: &str,
        model: &str,
    ) -> Result<(), DbError>;

    /// Clears the draft flag for a session row once its staged draft bundle
    /// starts the first live turn.
    async fn clear_session_draft_flag(&self, id: &str) -> Result<(), DbError>;

    /// Updates the persisted merged commit hash for a session row.
    async fn update_session_merged_commit_hash(
        &self,
        id: &str,
        merged_commit_hash: Option<String>,
    ) -> Result<(), DbError>;

    /// Persists or clears the immutable diff retained for archived sessions.
    async fn update_session_archived_diff(
        &self,
        id: &str,
        archived_diff: Option<String>,
    ) -> Result<(), DbError>;

    /// Persists or clears the parent/base commit hash used for deterministic
    /// stacked-child rebases.
    async fn update_session_stack_base_commit_hash(
        &self,
        id: &str,
        stack_base_commit_hash: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the parent link, rebase target, and deterministic old-base
    /// marker for one session as one atomic row mutation.
    async fn update_session_stack_membership(
        &self,
        id: &str,
        parent_session_id: Option<&str>,
        base_branch: &str,
        stack_base_commit_hash: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the saved prompt for a session row.
    async fn update_session_prompt(&self, id: &str, prompt: &str) -> Result<(), DbError>;

    /// Updates the persisted provider conversation identifier for a session.
    async fn update_session_provider_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError>;

    /// Updates the model clarification questions for a session row.
    async fn update_session_questions(&self, id: &str, questions: &str) -> Result<(), DbError>;

    /// Updates the persisted session reasoning level.
    async fn update_session_reasoning_level(
        &self,
        id: &str,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError>;

    /// Updates the persisted session response style.
    async fn update_session_response_style(
        &self,
        id: &str,
        response_style: ResponseStyle,
    ) -> Result<(), DbError>;

    /// Updates the persisted provider permission mode for future turns.
    async fn update_session_permission_mode(
        &self,
        id: &str,
        permission_mode: PermissionMode,
    ) -> Result<(), DbError>;

    /// Updates the persisted session response-speed preference.
    async fn update_session_speed_mode(
        &self,
        id: &str,
        speed_mode: SpeedMode,
    ) -> Result<(), DbError>;

    /// Updates the persisted upstream reference for a published session
    /// branch.
    async fn update_session_published_upstream_ref(
        &self,
        id: &str,
        published_upstream_ref: Option<String>,
    ) -> Result<(), DbError>;

    /// Accumulates token statistics for a session.
    async fn update_session_stats(&self, id: &str, stats: &SessionStats) -> Result<(), DbError>;

    /// Updates the status for a session row and opens or closes the persisted
    /// cumulative active-work interval when crossing the `InProgress`
    /// boundary.
    async fn update_session_status_with_timing_at(
        &self,
        id: &str,
        status: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError>;

    /// Updates or clears the persisted focused-review cache for a session.
    async fn update_session_focused_review(
        &self,
        id: &str,
        status: Option<FocusedReviewStatus>,
        diff_hash: Option<String>,
        text: Option<String>,
    ) -> Result<(), DbError>;

    /// Loads the prior diff baseline independently of focused-review output.
    /// An unfinished generated review returns no baseline so restart recovery
    /// can regenerate it instead of treating it as an unchanged completed turn.
    async fn load_session_review_diff_hash(&self, id: &str) -> Result<Option<String>, DbError>;

    /// Persists the observed diff baseline, atomically claiming a pending
    /// focused review when `claim_review` is true. Callers must commit this
    /// claim before starting review generation.
    async fn update_session_review_diff_hash(
        &self,
        id: &str,
        diff_hash: &str,
        claim_review: bool,
    ) -> Result<(), DbError>;

    /// Updates the display title for a session row.
    async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError>;

    /// Stores a fallback title that can be refined by a later substantive
    /// user prompt.
    async fn update_session_provisional_title(&self, id: &str, title: &str) -> Result<(), DbError>;

    /// Claims the next ordered title candidate for a session.
    ///
    /// When `requires_provisional_title` is true, no candidate is claimed
    /// after a generated or commit-derived title becomes authoritative.
    async fn begin_session_title_generation(
        &self,
        id: &str,
        requires_provisional_title: bool,
    ) -> Result<Option<i64>, DbError>;

    /// Applies one generated title unless a newer candidate or authoritative
    /// title has already been accepted.
    async fn update_session_title_for_generation(
        &self,
        id: &str,
        expected_generation: i64,
        title: &str,
    ) -> Result<bool, DbError>;

    /// Overrides the `created_at` timestamp for one session row.
    async fn update_session_created_at(&self, id: &str, created_at: i64) -> Result<(), DbError>;

    /// Overrides the `updated_at` timestamp for one session row.
    async fn update_session_updated_at(&self, id: &str, updated_at: i64) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`SessionRepository`].
#[derive(Clone)]
pub(crate) struct SqliteSessionRepository(
    pub(super) SqlitePool,
    Arc<dyn TimestampSource>,
    SessionMessageStore,
    SessionSnapshotStore,
);

impl SqliteSessionRepository {
    /// Creates a session repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self(
            pool.clone(),
            Arc::clone(&timestamp_source),
            SessionMessageStore::new(pool.clone(), Arc::clone(&timestamp_source)),
            SessionSnapshotStore::new(pool, timestamp_source),
        )
    }

    /// Returns the shared persistence timestamp in Unix seconds.
    pub(super) fn now(&self) -> i64 {
        self.1.now_timestamp_seconds()
    }

    /// Inserts one newly created session row with explicit draft-mode
    /// persistence.
    pub(super) async fn insert_with_draft_mode<'executor>(
        executor: impl sqlx::Executor<'executor, Database = sqlx::Sqlite>,
        timestamp_seconds: i64,
        row: PersistedSessionCreation<'_>,
    ) -> Result<(), DbError> {
        let PersistedSessionCreation {
            agent,
            base_branch,
            id,
            is_draft,
            model,
            orchestration_task_id,
            parent_session_id,
            permission_mode,
            personality_id,
            project_id,
            reasoning_level,
            response_style,
            role,
            speed_mode,
            status,
        } = row;
        status::validate_session(status)?;

        sqlx::query(
            r"
    INSERT INTO session (
        id,
        agent,
        model,
        base_branch,
        status,
        has_diff,
        is_draft,
        parent_session_id,
        permission_mode,
        personality_id,
        project_id,
        reasoning_level,
        response_style,
        role,
        speed_mode,
        orchestration_task_id,
        prompt,
        created_at,
        updated_at
    )
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ",
        )
        .bind(id)
        .bind(agent)
        .bind(model)
        .bind(base_branch)
        .bind(status)
        // Diff availability remains unknown until the worktree is refreshed.
        .bind(Option::<bool>::None)
        .bind(is_draft)
        .bind(parent_session_id)
        .bind(permission_mode.label())
        .bind(personality_id)
        .bind(project_id)
        .bind(reasoning_level.as_str())
        .bind(response_style.as_str())
        .bind(role)
        .bind(speed_mode.as_str())
        .bind(orchestration_task_id)
        .bind("")
        .bind(timestamp_seconds)
        .bind(timestamp_seconds)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Infers the legacy provider from its model family, retaining Antigravity
    /// as the fallback for Gemini and unrecognized model identifiers.
    fn persisted_agent_for_model(model: &str) -> String {
        let agent_kind = if model.starts_with("claude-") {
            AgentKind::Claude
        } else if model.starts_with("gpt-") {
            AgentKind::Codex
        } else {
            AgentKind::Antigravity
        };

        agent_kind.to_string()
    }
}

#[async_trait]
impl SessionRepository for SqliteSessionRepository {
    async fn append_session_message(
        &self,
        id: &str,
        kind: SessionMessageKind,
        content: &str,
    ) -> Result<(), DbError> {
        self.2.append(id, kind, content).await
    }

    async fn backfill_session_project(&self, project_id: i64) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET project_id = ?,
    updated_at = ?
WHERE project_id IS NULL
",
            project_id,
            now
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn defer_session_focused_review(&self, id: &str) -> Result<bool, DbError> {
        let now = self.now();
        let result = sqlx::query!(
            r#"
UPDATE session
SET focused_review_status = 'Pending',
    focused_review_diff_hash = NULL,
    focused_review_text = NULL,
    updated_at = ?
WHERE id = ?
  AND status IN ('InProgress', 'Review', 'AgentReview')
  AND (role IS NULL OR role <> 'Orchestrator')
"#,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn delete_session(&self, id: &str) -> Result<(), DbError> {
        let now = self.now();
        let mut transaction = self.0.begin().await?;

        // Retarget any stacked children onto this session's base branch before
        // the row is removed. The `ON DELETE SET NULL` foreign key clears the
        // child parent link automatically, but it leaves children pointing at
        // the deleted parent's worktree branch, which no longer exists. Mirror
        // the post-merge restack so a surviving child rebases against the
        // parent's base branch instead of an orphaned `wt/<parent>` ref.
        sqlx::query!(
            r"
UPDATE session
SET parent_session_id = NULL,
    base_branch = COALESCE((SELECT base_branch FROM session WHERE id = ?), base_branch),
    updated_at = ?
WHERE parent_session_id = ?
  AND status <> 'Canceled'
",
            id,
            now,
            id
        )
        .execute(&mut *transaction)
        .await?;

        sqlx::query!(
            r"
DELETE FROM session
WHERE id = ?
",
            id
        )
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;

        Ok(())
    }

    async fn get_session_base_branch(&self, id: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            RequiredStringValueRow,
            r#"
SELECT base_branch AS "value!: _"
FROM session
WHERE id = ?
"#,
            id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| row.value))
    }

    async fn get_session_parent_session_id(&self, id: &str) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar!(
            r"
SELECT parent_session_id
FROM session
WHERE id = ?
",
            id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value)
    }

    async fn get_session_stack_base_commit_hash(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar!(
            r"
SELECT stack_base_commit_hash
FROM session
WHERE id = ?
",
            id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value)
    }

    async fn get_session_instruction_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query_as!(
            SessionInstructionStateRow,
            r"
SELECT app_server_instruction_provider_conversation_id
FROM session
WHERE id = ?
",
            id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.and_then(SessionInstructionStateRow::into_instruction_conversation_id))
    }

    async fn get_session_provider_conversation_id(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT provider_conversation_id FROM session WHERE id = ?",
            id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value)
    }

    async fn insert_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        let agent = Self::persisted_agent_for_model(model);

        Self::insert_with_draft_mode(
            &self.0,
            self.now(),
            PersistedSessionCreation {
                agent: &agent,
                base_branch,
                id,
                is_draft: true,
                model,
                orchestration_task_id: None,
                parent_session_id: None,
                permission_mode: PermissionMode::AutoEdit,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                response_style: ResponseStyle::default(),
                role: None,
                speed_mode: SpeedMode::Normal,
                status,
            },
        )
        .await
    }

    async fn insert_stacked_draft_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        parent_session_id: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        let agent = Self::persisted_agent_for_model(model);

        Self::insert_with_draft_mode(
            &self.0,
            self.now(),
            PersistedSessionCreation {
                agent: &agent,
                base_branch,
                id,
                is_draft: true,
                model,
                orchestration_task_id: None,
                parent_session_id: Some(parent_session_id),
                permission_mode: PermissionMode::AutoEdit,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                response_style: ResponseStyle::default(),
                role: None,
                speed_mode: SpeedMode::Normal,
                status,
            },
        )
        .await
    }

    async fn insert_session(
        &self,
        id: &str,
        model: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) -> Result<(), DbError> {
        let agent = Self::persisted_agent_for_model(model);

        Self::insert_with_draft_mode(
            &self.0,
            self.now(),
            PersistedSessionCreation {
                agent: &agent,
                base_branch,
                id,
                is_draft: false,
                model,
                orchestration_task_id: None,
                parent_session_id: None,
                permission_mode: PermissionMode::AutoEdit,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                response_style: ResponseStyle::default(),
                role: None,
                speed_mode: SpeedMode::Normal,
                status,
            },
        )
        .await
    }

    async fn insert_session_with_agent(
        &self,
        session: PersistedSessionCreation<'_>,
    ) -> Result<(), DbError> {
        let PersistedSessionCreation {
            agent,
            base_branch,
            id,
            is_draft,
            model,
            orchestration_task_id,
            parent_session_id,
            permission_mode,
            personality_id,
            project_id,
            reasoning_level,
            response_style,
            role,
            speed_mode,
            status,
        } = session;

        Self::insert_with_draft_mode(
            &self.0,
            self.now(),
            PersistedSessionCreation {
                agent,
                base_branch,
                id,
                is_draft,
                model,
                orchestration_task_id,
                parent_session_id,
                permission_mode,
                personality_id,
                project_id,
                reasoning_level,
                response_style,
                role,
                speed_mode,
                status,
            },
        )
        .await
    }

    async fn fork_session_snapshot(
        &self,
        snapshot: ForkSessionSnapshot<'_>,
    ) -> Result<(), DbError> {
        self.3.fork(snapshot, None).await
    }

    async fn reserve_fork_session_snapshot(
        &self,
        snapshot: ForkSessionSnapshot<'_>,
        start_ref: &str,
    ) -> Result<(), DbError> {
        self.3.fork(snapshot, Some(start_ref)).await
    }

    async fn load_session(&self, session_id: &str) -> Result<Option<SessionRow>, DbError> {
        let row = sqlx::query_as::<_, SessionJoinRow>(
            r"
SELECT session.base_branch AS base_branch,
       session.added_lines AS added_lines,
       session.agent AS agent,
       session.created_at AS created_at,
       session.deleted_lines AS deleted_lines,
       session.has_diff AS has_diff,
       session.id AS id,
       session.in_progress_started_at,
       session.in_progress_total_seconds AS in_progress_total_seconds,
       session.input_tokens AS input_tokens,
       session.is_draft AS is_draft,
       session.model AS model,
       session.output_tokens AS output_tokens,
       session.parent_session_id,
       session.permission_mode AS permission_mode,
       session.personality_id,
       session.project_id,
       session.prompt AS prompt,
       session.reasoning_level AS reasoning_level_override,
       session.response_style AS response_style,
       session.speed_mode AS speed_mode,
       session.published_upstream_ref,
       session.questions,
       session_review_request.display_id AS review_request_display_id,
       session_review_request.forge_kind AS review_request_forge_kind,
       session_review_request.last_refreshed_at AS review_request_last_refreshed_at,
       session_review_request.source_branch AS review_request_source_branch,
       session_review_request.state AS review_request_state,
       session_review_request.status_summary AS review_request_status_summary,
       session_review_request.target_branch AS review_request_target_branch,
       session_review_request.title AS review_request_title,
       session_review_request.web_url AS review_request_web_url,
       session.role,
       session.size AS size,
       session.status AS status,
       session.title,
       session.updated_at AS updated_at
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
WHERE session.id = ?
",
        )
        .bind(session_id)
        .fetch_optional(&self.0)
        .await?;

        let row = row.map(SessionJoinRow::into_session_row);
        if let Some(row) = &row {
            status::validate_session(&row.status)?;
        }

        Ok(row)
    }

    async fn load_active_session_agent_models(&self) -> Result<Vec<SessionAgentModelRow>, DbError> {
        let rows = sqlx::query_as::<_, SessionAgentModelRow>(
            r"
SELECT agent,
       id,
       model,
       status
FROM session
WHERE status NOT IN ('Merged', 'Done', 'Canceled')
ORDER BY id
",
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
    }

    async fn load_sessions(&self) -> Result<Vec<SessionRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionJoinRow,
            r#"
SELECT session.base_branch AS base_branch,
       session.added_lines AS added_lines,
       session.agent AS agent,
       session.created_at AS created_at,
       session.deleted_lines AS deleted_lines,
       session.has_diff AS "has_diff: bool",
       session.id AS id,
       session.in_progress_started_at,
       session.in_progress_total_seconds AS in_progress_total_seconds,
       session.input_tokens AS input_tokens,
       session.is_draft AS "is_draft: bool",
       session.model AS model,
       session.output_tokens AS output_tokens,
       session.parent_session_id,
       session.permission_mode AS permission_mode,
       session.personality_id,
       session.project_id,
       session.prompt AS prompt,
       session.reasoning_level AS reasoning_level_override,
       session.response_style AS response_style,
       session.speed_mode AS speed_mode,
       session.published_upstream_ref,
       session.questions,
       session_review_request.display_id AS review_request_display_id,
       session_review_request.forge_kind AS review_request_forge_kind,
       session_review_request.last_refreshed_at AS review_request_last_refreshed_at,
       session_review_request.source_branch AS review_request_source_branch,
       session_review_request.state AS review_request_state,
       session_review_request.status_summary AS review_request_status_summary,
       session_review_request.target_branch AS review_request_target_branch,
       session_review_request.title AS review_request_title,
       session_review_request.web_url AS review_request_web_url,
       session.role,
       session.size AS size,
       session.status AS status,
       session.title,
       session.updated_at AS updated_at
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
ORDER BY session.updated_at DESC, session.created_at DESC, session.id
"#
        )
        .fetch_all(&self.0)
        .await?;

        let rows = rows
            .into_iter()
            .filter(SessionJoinRow::has_loadable_status)
            .map(SessionJoinRow::into_session_row)
            .collect::<Vec<_>>();

        Ok(rows)
    }

    async fn load_sessions_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionListRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionJoinRow,
            r#"
SELECT session.base_branch AS base_branch,
       session.added_lines AS added_lines,
       session.agent AS agent,
       session.created_at AS created_at,
       session.deleted_lines AS deleted_lines,
       session.has_diff AS "has_diff: bool",
       session.id AS id,
       session.in_progress_started_at,
       session.in_progress_total_seconds AS in_progress_total_seconds,
       session.input_tokens AS input_tokens,
       session.is_draft AS "is_draft: bool",
       session.model AS model,
       session.output_tokens AS output_tokens,
       session.parent_session_id,
       session.permission_mode AS permission_mode,
       session.personality_id,
       session.project_id,
       '' AS "prompt!: String",
       session.reasoning_level AS reasoning_level_override,
       session.response_style AS response_style,
       session.speed_mode AS speed_mode,
       session.published_upstream_ref,
       NULL AS "questions: String",
       session_review_request.display_id AS review_request_display_id,
       session_review_request.forge_kind AS review_request_forge_kind,
       session_review_request.last_refreshed_at AS review_request_last_refreshed_at,
       session_review_request.source_branch AS review_request_source_branch,
       session_review_request.state AS review_request_state,
       session_review_request.status_summary AS review_request_status_summary,
       session_review_request.target_branch AS review_request_target_branch,
       session_review_request.title AS review_request_title,
       session_review_request.web_url AS review_request_web_url,
       session.role,
       session.size AS size,
       session.status AS status,
       session.title,
       session.updated_at AS updated_at
FROM session
LEFT JOIN session_review_request
ON session_review_request.session_id = session.id
WHERE session.project_id = ?
ORDER BY session.updated_at DESC, session.created_at DESC, session.id
"#,
            project_id
        )
        .fetch_all(&self.0)
        .await?;

        let rows = rows
            .into_iter()
            .filter(SessionJoinRow::has_loadable_status)
            .map(SessionJoinRow::into_session_list_row)
            .collect::<Vec<_>>();

        Ok(rows)
    }

    async fn load_session_detail(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionDetailRow>, DbError> {
        let row = sqlx::query_as!(
            SessionDetailRow,
            r"
SELECT prompt,
       questions
FROM session
WHERE id = ?
",
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row)
    }

    async fn load_session_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessageRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionMessageRow,
            r"
SELECT content,
       kind,
       position
FROM session_message
WHERE session_id = ?
ORDER BY position, id
",
            session_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
    }

    async fn load_session_focused_reviews_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionFocusedReviewRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionFocusedReviewRow,
            r#"
SELECT id AS session_id,
       focused_review_diff_hash AS "diff_hash!: String",
       focused_review_text AS "text!: String"
FROM session
WHERE project_id = ?
  AND focused_review_diff_hash IS NOT NULL
  AND focused_review_text IS NOT NULL
  AND focused_review_text <> ''
ORDER BY updated_at DESC, id
"#,
            project_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
    }

    async fn load_sessions_metadata(&self) -> Result<(i64, i64), DbError> {
        let row = sqlx::query_as!(
            SessionStatsMetadataRow,
            r#"
SELECT (SELECT COUNT(*) FROM session) AS "session_count!: _",
       COALESCE(
           (
               SELECT updated_at
               FROM session
               ORDER BY updated_at DESC, id
               LIMIT 1
           ),
           0
       ) AS "max_updated_at!: _"
"#
        )
        .fetch_one(&self.0)
        .await?;

        Ok((row.session_count, row.max_updated_at))
    }

    async fn load_session_project_id(&self, session_id: &str) -> Result<Option<i64>, DbError> {
        let row = sqlx::query_as!(
            OptionalI64ValueRow,
            r#"
SELECT project_id AS "value: _"
FROM session
WHERE id = ?
"#,
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.and_then(|row| row.value))
    }

    async fn load_session_personality_state(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionPersonalityState>, DbError> {
        let state = sqlx::query_as!(
            SessionPersonalityState,
            r"
SELECT applied_personality_id,
       applied_personality_prompt_hash,
       personality_id
FROM session
WHERE id = ?
",
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(state)
    }

    async fn load_pending_stack_restack_session_ids(
        &self,
        project_id: i64,
    ) -> Result<Vec<String>, DbError> {
        let session_ids = sqlx::query_scalar!(
            r"
SELECT id
FROM session
WHERE project_id = ?
  AND parent_session_id IS NULL
  AND stack_base_commit_hash IS NOT NULL
  AND status IN ('Review', 'AgentReview')
ORDER BY updated_at ASC, id ASC
",
            project_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(session_ids)
    }

    async fn load_pending_focused_review_session_ids(
        &self,
        project_id: i64,
    ) -> Result<Vec<String>, DbError> {
        let session_ids = sqlx::query_scalar!(
            r#"
SELECT id
FROM session
WHERE project_id = ?
  AND focused_review_status = 'Pending'
  AND status IN ('Review', 'AgentReview')
  AND (role IS NULL OR role <> 'Orchestrator')
ORDER BY updated_at DESC, id
"#,
            project_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(session_ids)
    }

    async fn load_session_published_upstream_ref(
        &self,
        id: &str,
    ) -> Result<Option<String>, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT published_upstream_ref FROM session WHERE id = ?",
            id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value)
    }

    async fn load_session_merged_commit_hash(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query_scalar!(
            r"
SELECT merged_commit_hash
FROM session
WHERE id = ?
",
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.flatten())
    }

    async fn load_session_archived_diff(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query_scalar!(
            r"
SELECT archived_diff
FROM session
WHERE id = ?
",
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.flatten())
    }

    async fn load_session_reasoning_level(
        &self,
        session_id: &str,
    ) -> Result<ReasoningLevel, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT reasoning_level FROM session WHERE id = ?",
            session_id
        )
        .fetch_optional(&self.0)
        .await?
        .flatten();

        Ok(value
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default())
    }

    async fn load_session_response_style(
        &self,
        session_id: &str,
    ) -> Result<ResponseStyle, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT response_style FROM session WHERE id = ?",
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(value
            .and_then(|value| value.parse::<ResponseStyle>().ok())
            .unwrap_or_default())
    }

    async fn load_session_permission_mode(
        &self,
        session_id: &str,
    ) -> Result<PermissionMode, DbError> {
        let value = sqlx::query_scalar!(
            r"SELECT permission_mode FROM session WHERE id = ?",
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(value
            .and_then(|value| value.parse::<PermissionMode>().ok())
            .unwrap_or_default())
    }

    async fn load_session_speed_mode(&self, session_id: &str) -> Result<SpeedMode, DbError> {
        let value = sqlx::query_scalar!(r"SELECT speed_mode FROM session WHERE id = ?", session_id)
            .fetch_optional(&self.0)
            .await?;

        Ok(value
            .and_then(|value| value.parse::<SpeedMode>().ok())
            .unwrap_or_default())
    }

    async fn restack_child_sessions_after_parent_merge(
        &self,
        parent_session_id: &str,
        base_branch: &str,
        parent_commit_hash: Option<String>,
    ) -> Result<Vec<String>, DbError> {
        let now = self.now();
        let mut transaction = self.0.begin().await?;
        let materialized_child_ids = sqlx::query_scalar!(
            r"
SELECT id
FROM session
WHERE parent_session_id = ?
  AND status NOT IN ('Canceled', 'Draft')
ORDER BY created_at ASC, id ASC
",
            parent_session_id
        )
        .fetch_all(&mut *transaction)
        .await?;

        sqlx::query!(
            r"
UPDATE session
SET parent_session_id = NULL,
    base_branch = ?,
    stack_base_commit_hash = CASE
        WHEN status = 'Draft' THEN NULL
        ELSE COALESCE(stack_base_commit_hash, ?)
    END,
    updated_at = ?
WHERE parent_session_id = ?
  AND status <> 'Canceled'
",
            base_branch,
            parent_commit_hash,
            now,
            parent_session_id
        )
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;

        Ok(materialized_child_ids)
    }

    async fn load_session_timestamps(
        &self,
        session_id: &str,
    ) -> Result<Option<(i64, i64)>, DbError> {
        let row = sqlx::query_as!(
            SessionTimestampsRow,
            r#"
SELECT created_at, updated_at
FROM session
WHERE id = ?
            "#,
            session_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| (row.created_at, row.updated_at)))
    }

    async fn persist_session_turn_metadata(
        &self,
        session_id: &str,
        turn_metadata: &SessionTurnMetadata,
    ) -> Result<(), DbError> {
        let now = self.now();
        let mut transaction = self.0.begin().await?;

        let session_update = sqlx::query!(
            r"
UPDATE session
SET questions = ?,
    provider_conversation_id = ?,
    app_server_instruction_provider_conversation_id = ?,
    applied_personality_id = ?,
    applied_personality_prompt_hash = ?,
    updated_at = ?
WHERE id = ?
",
            turn_metadata.questions_json.as_str(),
            turn_metadata.provider_conversation_id.as_deref(),
            turn_metadata.instruction_conversation_id.as_deref(),
            turn_metadata.applied_personality_id.as_deref(),
            turn_metadata.applied_personality_prompt_hash.as_deref(),
            now,
            session_id
        )
        .execute(&mut *transaction)
        .await?;
        if session_update.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound.into());
        }

        if turn_metadata.token_usage_delta.input_tokens != 0
            || turn_metadata.token_usage_delta.output_tokens != 0
        {
            sqlx::query!(
                r"
UPDATE session
SET input_tokens = input_tokens + ?,
    output_tokens = output_tokens + ?,
    updated_at = ?
WHERE id = ?
",
                turn_metadata.token_usage_delta.input_tokens.cast_signed(),
                turn_metadata.token_usage_delta.output_tokens.cast_signed(),
                now,
                session_id
            )
            .execute(&mut *transaction)
            .await?;

            sqlx::query!(
                r"
INSERT INTO session_usage (
    session_id, model, created_at, input_tokens, output_tokens, invocation_count
)
VALUES (?, ?, ?, ?, ?, 1)
ON CONFLICT(session_id, model) DO UPDATE SET
    input_tokens = input_tokens + excluded.input_tokens,
    output_tokens = output_tokens + excluded.output_tokens,
    invocation_count = invocation_count + 1
",
                session_id,
                turn_metadata.model.as_str(),
                now,
                turn_metadata.token_usage_delta.input_tokens.cast_signed(),
                turn_metadata.token_usage_delta.output_tokens.cast_signed()
            )
            .execute(&mut *transaction)
            .await?;
        }

        insert_review_comment_resolutions(
            &mut transaction,
            session_id,
            &turn_metadata.review_comment_resolutions,
        )
        .await?;

        transaction.commit().await?;

        Ok(())
    }

    async fn update_session_diff_stats(
        &self,
        added_lines: u64,
        deleted_lines: u64,
        has_diff: bool,
        id: &str,
        size: &str,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET added_lines = ?,
    deleted_lines = ?,
    has_diff = ?,
    size = ?,
    updated_at = ?
WHERE id = ?
  AND (
      added_lines <> ?
      OR deleted_lines <> ?
      OR has_diff IS NOT ?
      OR size <> ?
  )
",
            added_lines.cast_signed(),
            deleted_lines.cast_signed(),
            has_diff,
            size,
            now,
            id,
            added_lines.cast_signed(),
            deleted_lines.cast_signed(),
            has_diff,
            size
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_diff_unknown(&self, id: &str) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET has_diff = NULL,
    updated_at = ?
WHERE id = ?
  AND has_diff IS NOT NULL
",
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_instruction_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET app_server_instruction_provider_conversation_id = ?,
    updated_at = ?
WHERE id = ?
",
            provider_conversation_id.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_model(&self, id: &str, model: &str) -> Result<(), DbError> {
        let agent = Self::persisted_agent_for_model(model);
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET agent = ?,
    model = ?,
    updated_at = ?
WHERE id = ?
",
            agent,
            model,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_personality_id(
        &self,
        id: &str,
        personality_id: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET personality_id = ?,
    updated_at = ?
WHERE id = ?
",
            personality_id.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_agent_model(
        &self,
        id: &str,
        agent: &str,
        model: &str,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET agent = ?,
    model = ?,
    updated_at = ?
WHERE id = ?
",
            agent,
            model,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_active_session_agent_model(
        &self,
        id: &str,
        agent: &str,
        model: &str,
    ) -> Result<(), DbError> {
        sqlx::query!(
            r"
UPDATE session
SET agent = ?,
    model = ?
WHERE id = ?
  AND status NOT IN ('Merged', 'Done', 'Canceled')
",
            agent,
            model,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn clear_session_draft_flag(&self, id: &str) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET is_draft = 0,
    updated_at = ?
WHERE id = ?
",
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_merged_commit_hash(
        &self,
        id: &str,
        merged_commit_hash: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET merged_commit_hash = ?,
    updated_at = ?
WHERE id = ?
",
            merged_commit_hash.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_archived_diff(
        &self,
        id: &str,
        archived_diff: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET archived_diff = ?,
    updated_at = ?
WHERE id = ?
",
            archived_diff.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_stack_base_commit_hash(
        &self,
        id: &str,
        stack_base_commit_hash: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query(
            r"
UPDATE session
SET stack_base_commit_hash = ?,
    updated_at = ?
WHERE id = ?
",
        )
        .bind(stack_base_commit_hash)
        .bind(now)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_stack_membership(
        &self,
        id: &str,
        parent_session_id: Option<&str>,
        base_branch: &str,
        stack_base_commit_hash: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query(
            r"
UPDATE session
SET parent_session_id = ?,
    base_branch = ?,
    stack_base_commit_hash = ?,
    updated_at = ?
WHERE id = ?
",
        )
        .bind(parent_session_id)
        .bind(base_branch)
        .bind(stack_base_commit_hash)
        .bind(now)
        .bind(id)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_prompt(&self, id: &str, prompt: &str) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET prompt = ?,
    updated_at = ?
WHERE id = ?
",
            prompt,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_provider_conversation_id(
        &self,
        id: &str,
        provider_conversation_id: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET provider_conversation_id = ?,
    updated_at = ?
WHERE id = ?
",
            provider_conversation_id.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_questions(&self, id: &str, questions: &str) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET questions = ?,
    updated_at = ?
WHERE id = ?
",
            questions,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_reasoning_level(
        &self,
        id: &str,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r#"
UPDATE session
SET reasoning_level = ?,
    updated_at = ?
WHERE id = ?
            "#,
            reasoning_level.as_str(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_response_style(
        &self,
        id: &str,
        response_style: ResponseStyle,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r#"
UPDATE session
SET response_style = ?,
    updated_at = ?
WHERE id = ?
            "#,
            response_style.as_str(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_permission_mode(
        &self,
        id: &str,
        permission_mode: PermissionMode,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r#"
UPDATE session
SET permission_mode = ?,
    updated_at = ?
WHERE id = ?
            "#,
            permission_mode.label(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_speed_mode(
        &self,
        id: &str,
        speed_mode: SpeedMode,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r#"
UPDATE session
SET speed_mode = ?,
    updated_at = ?
WHERE id = ?
            "#,
            speed_mode.as_str(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_published_upstream_ref(
        &self,
        id: &str,
        published_upstream_ref: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET published_upstream_ref = ?,
    updated_at = ?
WHERE id = ?
",
            published_upstream_ref.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_stats(&self, id: &str, stats: &SessionStats) -> Result<(), DbError> {
        if stats.input_tokens == 0 && stats.output_tokens == 0 {
            return Ok(());
        }

        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET input_tokens = input_tokens + ?,
    output_tokens = output_tokens + ?,
    updated_at = ?
WHERE id = ?
",
            stats.input_tokens.cast_signed(),
            stats.output_tokens.cast_signed(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_status_with_timing_at(
        &self,
        id: &str,
        status: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError> {
        status::validate_session(status)?;
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET status = ?,
    in_progress_total_seconds = CASE
        WHEN ? = 'InProgress' OR in_progress_started_at IS NULL THEN in_progress_total_seconds
        ELSE in_progress_total_seconds + MAX(0, ? - in_progress_started_at)
    END,
    in_progress_started_at = CASE
        WHEN ? = 'InProgress' THEN COALESCE(in_progress_started_at, ?)
        ELSE NULL
    END,
    updated_at = ?
WHERE id = ?
",
            status,
            status,
            timestamp_seconds,
            status,
            timestamp_seconds,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_focused_review(
        &self,
        id: &str,
        status: Option<FocusedReviewStatus>,
        diff_hash: Option<String>,
        text: Option<String>,
    ) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r"
UPDATE session
SET focused_review_status = ?,
    focused_review_diff_hash = ?,
    focused_review_text = ?,
    updated_at = ?
WHERE id = ?
",
            status.map(|status| status.to_string()),
            diff_hash.as_deref(),
            text.as_deref(),
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn load_session_review_diff_hash(&self, id: &str) -> Result<Option<String>, DbError> {
        let previous = sqlx::query_scalar!(
            r#"SELECT CASE WHEN focused_review_status = 'Pending' AND focused_review_diff_hash IS NOT NULL
                    THEN NULL ELSE review_diff_hash END AS "review_diff_hash?: String"
               FROM session WHERE id = ?"#,
            id
        )
            .fetch_optional(&self.0)
            .await?
            .flatten();

        Ok(previous)
    }

    async fn update_session_review_diff_hash(
        &self,
        id: &str,
        diff_hash: &str,
        claim_review: bool,
    ) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;
        sqlx::query!(
            "UPDATE session SET review_diff_hash = ? WHERE id = ?",
            diff_hash,
            id
        )
        .execute(&mut *transaction)
        .await?;
        if claim_review {
            let now = self.now();
            sqlx::query!(
                r"
UPDATE session
SET focused_review_status = ?,
    focused_review_diff_hash = ?,
    focused_review_text = ?,
    updated_at = ?
WHERE id = ?
",
                "Pending",
                diff_hash,
                None::<String>,
                now,
                id
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r#"
UPDATE session
SET title = ?,
    is_title_provisional = 0,
    title_generation = title_generation + 1,
    applied_title_generation = title_generation + 1,
    updated_at = ?
WHERE id = ?
"#,
            title,
            now,
            id,
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_provisional_title(&self, id: &str, title: &str) -> Result<(), DbError> {
        let now = self.now();

        sqlx::query!(
            r#"
UPDATE session
SET title = ?,
    is_title_provisional = 1,
    title_generation = title_generation + 1,
    applied_title_generation = title_generation + 1,
    updated_at = ?
WHERE id = ?
"#,
            title,
            now,
            id,
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn begin_session_title_generation(
        &self,
        id: &str,
        requires_provisional_title: bool,
    ) -> Result<Option<i64>, DbError> {
        let now = self.now();
        let generation = if requires_provisional_title {
            sqlx::query_scalar!(
                r#"
UPDATE session
SET is_title_provisional = 1,
    title_generation = title_generation + 1,
    updated_at = ?
WHERE id = ?
  AND is_title_provisional = 1
RETURNING title_generation
"#,
                now,
                id,
            )
            .fetch_optional(&self.0)
            .await?
        } else {
            sqlx::query_scalar!(
                r#"
UPDATE session
SET is_title_provisional = 1,
    title_generation = title_generation + 1,
    updated_at = ?
WHERE id = ?
RETURNING title_generation
"#,
                now,
                id,
            )
            .fetch_optional(&self.0)
            .await?
        };

        Ok(generation)
    }

    async fn update_session_title_for_generation(
        &self,
        id: &str,
        expected_generation: i64,
        title: &str,
    ) -> Result<bool, DbError> {
        let now = self.now();

        let result = sqlx::query!(
            r#"
UPDATE session
SET title = ?,
    is_title_provisional = 0,
    applied_title_generation = ?,
    updated_at = ?
WHERE id = ?
  AND title_generation >= ?
  AND applied_title_generation < ?
"#,
            title,
            expected_generation,
            now,
            id,
            expected_generation,
            expected_generation,
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn update_session_created_at(&self, id: &str, created_at: i64) -> Result<(), DbError> {
        sqlx::query!(
            r"
UPDATE session
SET created_at = ?
WHERE id = ?
",
            created_at,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_session_updated_at(&self, id: &str, updated_at: i64) -> Result<(), DbError> {
        sqlx::query!(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
            updated_at,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

/// Row returned when loading a required string scalar value.
struct RequiredStringValueRow {
    value: String,
}

/// Row returned when loading session count and latest-update metadata.
struct SessionStatsMetadataRow {
    /// Latest `session.updated_at` timestamp across rows.
    max_updated_at: i64,
    /// Total number of persisted sessions.
    session_count: i64,
}

/// Row returned when loading an optional `i64` scalar value.
struct OptionalI64ValueRow {
    value: Option<i64>,
}

/// Row returned when loading the persisted instruction bootstrap marker for
/// one session.
struct SessionInstructionStateRow {
    app_server_instruction_provider_conversation_id: Option<String>,
}

impl SessionInstructionStateRow {
    /// Converts the optional stored provider conversation id into one
    /// normalized bootstrap conversation id when present and non-empty.
    fn into_instruction_conversation_id(self) -> Option<String> {
        agent::normalize_instruction_conversation_id(
            self.app_server_instruction_provider_conversation_id
                .as_deref(),
        )
    }
}

/// Row returned when loading both persisted timestamps for one session.
struct SessionTimestampsRow {
    created_at: i64,
    updated_at: i64,
}

/// Shared columns for session metadata rows used by both session and
/// session-list mappings.
struct SessionRowMetadata {
    added_lines: i64,
    agent: String,
    base_branch: String,
    created_at: i64,
    deleted_lines: i64,
    has_diff: Option<bool>,
    id: String,
    in_progress_started_at: Option<i64>,
    in_progress_total_seconds: i64,
    input_tokens: i64,
    is_draft: bool,
    model: String,
    output_tokens: i64,
    parent_session_id: Option<String>,
    permission_mode: String,
    personality_id: Option<String>,
    project_id: Option<i64>,
    published_upstream_ref: Option<String>,
    reasoning_level_override: Option<String>,
    response_style: String,
    role: Option<String>,
    size: String,
    speed_mode: String,
    status: String,
    title: Option<String>,
    updated_at: i64,
}

impl SessionRowMetadata {
    /// Converts shared metadata fields into a complete session row.
    fn into_session_row(
        self,
        prompt: String,
        questions: Option<String>,
        review_request: Option<SessionReviewRequestRow>,
    ) -> SessionRow {
        SessionRow {
            added_lines: self.added_lines,
            agent: self.agent,
            base_branch: self.base_branch,
            created_at: self.created_at,
            deleted_lines: self.deleted_lines,
            has_diff: self.has_diff,
            id: self.id,
            in_progress_started_at: self.in_progress_started_at,
            in_progress_total_seconds: self.in_progress_total_seconds,
            input_tokens: self.input_tokens,
            is_draft: self.is_draft,
            model: self.model,
            output_tokens: self.output_tokens,
            parent_session_id: self.parent_session_id,
            permission_mode: self.permission_mode,
            personality_id: self.personality_id,
            project_id: self.project_id,
            prompt,
            published_upstream_ref: self.published_upstream_ref,
            questions,
            reasoning_level_override: self.reasoning_level_override,
            response_style: self.response_style,
            review_request,
            role: self.role,
            size: self.size,
            speed_mode: self.speed_mode,
            status: self.status,
            title: self.title,
            updated_at: self.updated_at,
        }
    }

    /// Converts shared metadata fields into a session-list row.
    fn into_session_list_row(
        self,
        review_request: Option<SessionReviewRequestRow>,
    ) -> SessionListRow {
        SessionListRow {
            added_lines: self.added_lines,
            agent: self.agent,
            base_branch: self.base_branch,
            created_at: self.created_at,
            deleted_lines: self.deleted_lines,
            has_diff: self.has_diff,
            id: self.id,
            in_progress_started_at: self.in_progress_started_at,
            in_progress_total_seconds: self.in_progress_total_seconds,
            input_tokens: self.input_tokens,
            is_draft: self.is_draft,
            model: self.model,
            output_tokens: self.output_tokens,
            parent_session_id: self.parent_session_id,
            permission_mode: self.permission_mode,
            personality_id: self.personality_id,
            project_id: self.project_id,
            published_upstream_ref: self.published_upstream_ref,
            reasoning_level_override: self.reasoning_level_override,
            response_style: self.response_style,
            review_request,
            role: self.role,
            size: self.size,
            speed_mode: self.speed_mode,
            status: self.status,
            title: self.title,
            updated_at: self.updated_at,
        }
    }
}

/// Row returned when loading one complete `session` plus aliased
/// `session_review_request` join columns.
#[derive(sqlx::FromRow)]
struct SessionJoinRow {
    added_lines: i64,
    agent: String,
    base_branch: String,
    created_at: i64,
    deleted_lines: i64,
    has_diff: Option<bool>,
    id: String,
    in_progress_started_at: Option<i64>,
    in_progress_total_seconds: i64,
    input_tokens: i64,
    is_draft: bool,
    model: String,
    output_tokens: i64,
    parent_session_id: Option<String>,
    permission_mode: String,
    personality_id: Option<String>,
    project_id: Option<i64>,
    prompt: String,
    published_upstream_ref: Option<String>,
    questions: Option<String>,
    reasoning_level_override: Option<String>,
    response_style: String,
    review_request_display_id: Option<String>,
    review_request_forge_kind: Option<String>,
    review_request_last_refreshed_at: Option<i64>,
    review_request_source_branch: Option<String>,
    review_request_state: Option<String>,
    review_request_status_summary: Option<String>,
    review_request_target_branch: Option<String>,
    review_request_title: Option<String>,
    review_request_web_url: Option<String>,
    role: Option<String>,
    size: String,
    speed_mode: String,
    status: String,
    title: Option<String>,
    updated_at: i64,
}

impl SessionJoinRow {
    /// Returns whether this row can be included in a collection load.
    ///
    /// Invalid persisted statuses are logged and omitted so one corrupt row
    /// cannot hide every otherwise valid session in the project list.
    fn has_loadable_status(&self) -> bool {
        if let Err(error) = status::validate_session(&self.status) {
            warn!(
                session_id = %self.id,
                %error,
                "Skipping session with invalid persisted status"
            );

            return false;
        }

        true
    }

    /// Converts the query-mapped join row into a complete [`SessionRow`].
    fn into_session_row(self) -> SessionRow {
        let (metadata, detail, review_request) = self.into_parts();

        metadata.into_session_row(detail.prompt, detail.questions, review_request)
    }

    /// Converts placeholder-detail query rows into a lightweight
    /// [`SessionListRow`].
    fn into_session_list_row(self) -> SessionListRow {
        let (metadata, _, review_request) = self.into_parts();

        metadata.into_session_list_row(review_request)
    }

    /// Splits the flat query row into shared metadata, transcript detail, and
    /// normalized review-request data.
    fn into_parts(
        self,
    ) -> (
        SessionRowMetadata,
        SessionDetailRow,
        Option<SessionReviewRequestRow>,
    ) {
        let Self {
            added_lines,
            agent,
            base_branch,
            created_at,
            deleted_lines,
            has_diff,
            id,
            in_progress_started_at,
            in_progress_total_seconds,
            input_tokens,
            is_draft,
            model,
            output_tokens,
            parent_session_id,
            permission_mode,
            personality_id,
            project_id,
            prompt,
            published_upstream_ref,
            questions,
            reasoning_level_override,
            response_style,
            review_request_display_id,
            review_request_forge_kind,
            review_request_last_refreshed_at,
            review_request_source_branch,
            review_request_state,
            review_request_status_summary,
            review_request_target_branch,
            review_request_title,
            review_request_web_url,
            role,
            size,
            speed_mode,
            status,
            title,
            updated_at,
        } = self;

        let metadata = SessionRowMetadata {
            added_lines,
            agent,
            base_branch,
            created_at,
            deleted_lines,
            has_diff,
            id,
            in_progress_started_at,
            in_progress_total_seconds,
            input_tokens,
            is_draft,
            model,
            output_tokens,
            parent_session_id,
            permission_mode,
            personality_id,
            project_id,
            published_upstream_ref,
            reasoning_level_override,
            response_style,
            role,
            size,
            speed_mode,
            status,
            title,
            updated_at,
        };
        let detail = SessionDetailRow { prompt, questions };
        let review_request = SessionReviewRequestJoinRow {
            display_id: review_request_display_id,
            forge_kind: review_request_forge_kind,
            last_refreshed_at: review_request_last_refreshed_at,
            source_branch: review_request_source_branch,
            state: review_request_state,
            status_summary: review_request_status_summary,
            target_branch: review_request_target_branch,
            title: review_request_title,
            web_url: review_request_web_url,
        }
        .into_review_request_row();

        (metadata, detail, review_request)
    }
}

/// Aliased nullable `session_review_request` columns loaded through a joined
/// session query.
struct SessionReviewRequestJoinRow {
    display_id: Option<String>,
    forge_kind: Option<String>,
    last_refreshed_at: Option<i64>,
    source_branch: Option<String>,
    state: Option<String>,
    status_summary: Option<String>,
    target_branch: Option<String>,
    title: Option<String>,
    web_url: Option<String>,
}

impl SessionReviewRequestJoinRow {
    /// Converts the joined nullable columns into a review-request row only
    /// when every required field is present.
    fn into_review_request_row(self) -> Option<SessionReviewRequestRow> {
        let Self {
            display_id,
            forge_kind,
            last_refreshed_at,
            source_branch,
            state,
            status_summary,
            target_branch,
            title,
            web_url,
        } = self;

        Some(SessionReviewRequestRow {
            display_id: display_id?,
            forge_kind: forge_kind?,
            last_refreshed_at: last_refreshed_at?,
            source_branch: source_branch?,
            state: state?,
            status_summary,
            target_branch: target_branch?,
            title: title?,
            web_url: web_url?,
        })
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
