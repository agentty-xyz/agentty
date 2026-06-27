//! Event types and reducer helpers for the app core module.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use app::branch_publish::{
    BranchPublishActionUpdate, BranchPublishTaskResult, BranchPublishTaskSuccess,
    branch_publish_loading_label as branch_publish_loading_label_text,
    branch_publish_loading_message as branch_publish_loading_message_text,
    branch_publish_loading_title as branch_publish_loading_title_text,
    branch_publish_success_title as branch_publish_success_title_text,
    detected_forge_kind_from_git_push_error, git_push_authentication_message,
    is_git_push_authentication_error,
    pull_request_publish_success_message as pull_request_publish_success_message_text,
};
use app::reducer::AppEventReducer;
use app::review::{
    FocusedReviewPersistence, ReviewUpdate, apply_review_updates, auto_start_reviews,
};

use super::state::{
    App, RequestedReviewCommentFetchKey, SyncPopupContext, SyncReviewRequestTaskResult,
    UpdateStatus,
};
use crate::app::session::{
    SessionTaskService, SyncMainOutcome, SyncSessionStartError, TurnAppliedState,
};
use crate::app::session_state::SessionGitStatus;
use crate::app::{self, session};
use crate::domain::agent::AgentCliInfo;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::domain::session::{
    PublishBranchAction, PublishedBranchSyncStatus, SessionId, SessionSize, Status,
};
use crate::domain::system_log::{SystemLogCategory, SystemLogEvent, SystemLogLevel};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::runtime::mode::{at_mention, question, sync_blocked};
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, QuestionFocus};
use crate::ui::state::prompt::PromptAtMentionState;

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Indicates background-loaded prompt at-mention entries for one session.
    AtMentionEntriesLoaded {
        entries: Vec<FileEntry>,
        session_id: SessionId,
    },
    /// Indicates the latest project-branch and session-branch ahead/behind
    /// information from the git status worker.
    GitStatusUpdated {
        /// Sync-context generation used to reject stale completions.
        generation: u64,
        session_statuses: HashMap<SessionId, SessionGitStatus>,
        status: Option<(u32, u32)>,
    },
    /// Indicates whether a newer stable `agentty` release is available.
    VersionAvailabilityUpdated {
        latest_available_version: Option<String>,
    },
    /// Indicates locally available agent CLI versions finished loading.
    AgentCliVersionsUpdated { agent_clis: Vec<AgentCliInfo> },
    /// Indicates progress of the background auto-update.
    UpdateStatusChanged { update_status: UpdateStatus },
    /// Records one process-local system log event.
    SystemLog { event: SystemLogEvent },
    /// Indicates a session agent/model selection has been persisted.
    SessionModelUpdated {
        session_id: SessionId,
        session_agent: crate::domain::agent::AgentSelection,
    },
    /// Indicates a session reasoning override selection has been persisted.
    SessionReasoningLevelUpdated {
        reasoning_level_override: Option<crate::domain::agent::ReasoningLevel>,
        session_id: SessionId,
    },
    /// Requests a DB-backed session list refresh.
    RefreshSessions,
    /// Requests a DB-backed project list refresh, including aggregate session
    /// counts shown on the projects tab.
    RefreshProjects,
    /// Requests an immediate git-status refresh outside the periodic poll
    /// cadence.
    RefreshGitStatus,
    /// Indicates completion of one requested-review list refresh.
    RequestedReviewsLoaded {
        /// Refresh generation assigned when the task was spawned.
        generation: u64,
        /// Project id whose requested reviews were loaded.
        project_id: i64,
        /// Forge result from the background requested-review task.
        result: Result<Vec<ag_forge::RequestedReview>, String>,
    },
    /// Indicates completion of one requested-review detail comment load.
    RequestedReviewCommentSnapshotLoaded {
        /// Provider display id such as GitHub `#123` or GitLab `!123`.
        display_id: String,
        /// Requested-review list generation visible when the comment load
        /// began.
        generation: u64,
        /// Project id whose selected review comment snapshot was loaded.
        project_id: i64,
        /// Comment snapshot result from the background detail task.
        result: Result<ag_forge::ReviewCommentSnapshot, String>,
        /// Browser-openable review-request URL used to disambiguate rows.
        web_url: String,
    },
    /// Indicates compact live thinking text for an in-progress session.
    SessionProgressUpdated {
        progress_message: Option<String>,
        session_id: SessionId,
    },
    /// Indicates completion of a list-mode sync workflow.
    SyncMainCompleted {
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    },
    /// Indicates recomputed diff-derived size and line-count totals for one
    /// session.
    SessionSizeUpdated {
        added_lines: u64,
        deleted_lines: u64,
        session_id: SessionId,
        session_size: SessionSize,
    },
    /// Indicates one tracked draft-title generation task reached a terminal
    /// outcome and can be pruned from in-memory task tracking.
    SessionTitleGenerationFinished {
        generation: u64,
        session_id: SessionId,
    },
    /// Indicates completion of a session-view branch-publish action.
    BranchPublishActionCompleted {
        restore_view: ConfirmationViewMode,
        result: Box<BranchPublishTaskResult>,
        session_id: SessionId,
    },
    /// Indicates review assist output became available for a session.
    ReviewPrepared {
        diff_hash: u64,
        review_text: String,
        session_id: SessionId,
    },
    /// Indicates review assist failed for a session.
    ReviewPreparationFailed {
        diff_hash: u64,
        error: String,
        session_id: SessionId,
    },
    /// Indicates that a session handle snapshot changed in-memory and carries
    /// the latest observable handle version for redraw deduplication.
    SessionUpdated { session_id: SessionId, version: u64 },
    /// Indicates that an agent turn completed and persisted one reducer-ready
    /// projection.
    AgentResponseReceived {
        session_id: SessionId,
        turn_applied_state: TurnAppliedState,
    },
    /// Indicates one review-ready parent turn finished and any materialized
    /// stacked child branches should sync onto the refreshed parent branch.
    StackedParentTurnCompleted { session_id: SessionId },
    /// Indicates one review-ready parent sync finished and any materialized
    /// stacked child branches should sync onto the refreshed parent branch.
    StackedParentSyncCompleted { session_id: SessionId },
    /// Indicates a transient workflow notice changed for one session.
    SessionWorkflowNoticeUpdated {
        notice: String,
        session_id: SessionId,
    },
    /// Indicates that one published session branch started or finished a
    /// background auto-push after a completed turn.
    PublishedBranchSyncUpdated {
        session_id: SessionId,
        sync_operation_id: String,
        sync_status: PublishedBranchSyncStatus,
    },
    /// Indicates completion of one background review-request status refresh.
    ReviewRequestStatusUpdated {
        /// Sync-context generation used to reject stale completions.
        generation: u64,
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: SessionId,
    },
    /// Indicates that the inline review-comment cache for one session now
    /// exposes updated thread content.
    ///
    /// Emitted only when the background sync task observes a content change
    /// (see `ReviewCommentCache::record_snapshot`) so the UI can redraw without
    /// being woken on every 60-second tick.
    ReviewCommentsUpdated { session_id: SessionId },
}

/// Reduced representation of all app events currently queued for one tick.
#[derive(Default)]
pub(super) struct AppEventBatch {
    pub(super) applied_turns: HashMap<SessionId, TurnAppliedState>,
    pub(super) agent_cli_updates: Option<Vec<AgentCliInfo>>,
    pub(super) at_mention_entries_updates: HashMap<SessionId, Vec<FileEntry>>,
    pub(super) branch_publish_action_update: Option<BranchPublishActionUpdate>,
    pub(super) git_status_update: Option<GitStatusBatchUpdate>,
    pub(super) latest_available_version_update: Option<LatestAvailableVersionUpdate>,
    pub(super) published_branch_sync_updates: Vec<(SessionId, PublishedBranchSyncUpdate)>,
    pub(super) review_updates: HashMap<SessionId, ReviewUpdate>,
    pub(super) session_git_status_updates: HashMap<SessionId, SessionGitStatus>,
    pub(super) session_ids: HashSet<SessionId>,
    pub(super) session_update_versions: HashMap<SessionId, u64>,
    pub(super) session_model_updates: HashMap<SessionId, crate::domain::agent::AgentSelection>,
    pub(super) session_reasoning_level_updates:
        HashMap<SessionId, Option<crate::domain::agent::ReasoningLevel>>,
    pub(super) session_progress_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_size_updates: HashMap<SessionId, (u64, u64, SessionSize)>,
    pub(super) stacked_parent_syncs_completed: HashSet<SessionId>,
    pub(super) stacked_parent_turns_completed: HashSet<SessionId>,
    pub(super) session_title_generation_finished: HashMap<SessionId, u64>,
    pub(super) session_workflow_notice_updates: HashMap<SessionId, Vec<String>>,
    pub(super) should_refresh_git_status: bool,
    /// Whether this batch should reload project list snapshots from
    /// persistence.
    pub(super) should_reload_projects: bool,
    /// Whether this batch should reload session list snapshots from
    /// persistence.
    pub(super) should_reload_sessions: bool,
    /// Ordered process-local system log events to append during this reducer
    /// batch.
    pub(super) system_log_events: Vec<SystemLogEvent>,
    pub(super) review_request_status_updates: Vec<ReviewRequestStatusUpdate>,
    pub(super) review_comment_session_ids: HashSet<SessionId>,
    /// Latest requested-review task result collected for this reducer batch,
    /// including its generation for stale-result rejection.
    pub(super) requested_reviews:
        Option<(u64, i64, Result<Vec<ag_forge::RequestedReview>, String>)>,
    pub(super) requested_review_comment_snapshots: Vec<RequestedReviewCommentSnapshotUpdate>,
    pub(super) sync_main_result: Option<Result<SyncMainOutcome, SyncSessionStartError>>,
    pub(super) update_status: Option<UpdateStatus>,
}

/// Optional aggregate git status payload from the latest status event in one
/// reducer batch.
pub(super) struct GitStatusBatchUpdate {
    /// Sync-context generation that produced this status snapshot.
    generation: u64,
    /// Main worktree added/deleted line counts, when available.
    status: Option<(u32, u32)>,
}

/// Optional version-availability payload from the latest updater event in one
/// reducer batch.
pub(super) struct LatestAvailableVersionUpdate {
    /// Latest available version string, or `None` when no update is available.
    latest_available_version: Option<String>,
}

/// One ordered published-branch sync update queued for one session.
pub(super) struct PublishedBranchSyncUpdate {
    /// Operation identifier used to ignore stale terminal auto-push updates.
    sync_operation_id: String,
    /// Auto-push state carried by this update.
    sync_status: PublishedBranchSyncStatus,
}

/// Completed review-request status refresh payload ready for reducer
/// application.
pub(super) struct ReviewRequestStatusUpdate {
    pub(super) generation: u64,
    pub(super) result: Result<SyncReviewRequestTaskResult, String>,
    pub(super) session_id: SessionId,
}

/// Completed requested-review comment snapshot load ready for reducer
/// application.
pub(super) struct RequestedReviewCommentSnapshotUpdate {
    pub(super) display_id: String,
    pub(super) generation: u64,
    pub(super) project_id: i64,
    pub(super) result: Result<ag_forge::ReviewCommentSnapshot, String>,
    pub(super) web_url: String,
}

impl AppEventBatch {
    /// Collects one app event into the coalesced batch state.
    ///
    /// Most per-session projections use latest-wins semantics, but queued
    /// `AgentResponseReceived` events merge token-usage deltas so one reducer
    /// tick preserves cumulative usage from multiple completed turns.
    pub(super) fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => self.collect_at_mention_entries_loaded(session_id, entries),
            AppEvent::GitStatusUpdated {
                generation,
                session_statuses,
                status,
            } => self.collect_git_status_updated(generation, session_statuses, status),
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            } => self.collect_version_availability_updated(latest_available_version),
            AppEvent::AgentCliVersionsUpdated { agent_clis } => {
                self.collect_agent_cli_versions_updated(agent_clis);
            }
            AppEvent::UpdateStatusChanged { update_status } => {
                self.collect_update_status_changed(update_status);
            }
            AppEvent::SystemLog { event } => self.collect_system_log(event),
            AppEvent::SessionModelUpdated {
                session_id,
                session_agent,
            } => self.collect_session_model_updated(session_id, session_agent),
            AppEvent::SessionReasoningLevelUpdated {
                reasoning_level_override,
                session_id,
            } => self.collect_session_reasoning_level_updated(session_id, reasoning_level_override),
            AppEvent::RefreshSessions => self.collect_refresh_sessions(),
            AppEvent::RefreshProjects => self.collect_refresh_projects(),
            AppEvent::RefreshGitStatus => self.collect_refresh_git_status(),
            AppEvent::RequestedReviewsLoaded {
                generation,
                project_id,
                result,
            } => self.collect_requested_reviews_loaded(generation, project_id, result),
            AppEvent::RequestedReviewCommentSnapshotLoaded {
                display_id,
                generation,
                project_id,
                result,
                web_url,
            } => self.collect_requested_review_comment_snapshot_loaded(
                display_id, generation, project_id, result, web_url,
            ),
            event => self.collect_runtime_event(event),
        }
    }

    /// Collects session, workflow, and runtime events after top-level app
    /// refresh events have been handled.
    fn collect_runtime_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => self.collect_session_progress_updated(session_id, progress_message),
            AppEvent::SyncMainCompleted { result } => self.collect_sync_main_completed(result),
            AppEvent::SessionSizeUpdated {
                added_lines,
                deleted_lines,
                session_id,
                session_size,
            } => {
                self.session_size_updates
                    .insert(session_id, (added_lines, deleted_lines, session_size));
            }
            AppEvent::SessionTitleGenerationFinished {
                generation,
                session_id,
            } => {
                self.session_title_generation_finished
                    .insert(session_id, generation);
            }
            AppEvent::BranchPublishActionCompleted {
                restore_view,
                result,
                session_id,
            } => self.collect_branch_publish_action_completed(restore_view, *result, session_id),
            AppEvent::ReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            } => self.collect_review_prepared(diff_hash, review_text, session_id),
            AppEvent::ReviewPreparationFailed {
                diff_hash,
                error,
                session_id,
            } => self.collect_review_preparation_failed(diff_hash, error, session_id),
            AppEvent::SessionUpdated {
                session_id,
                version,
            } => self.collect_session_updated(session_id, version),
            AppEvent::AgentResponseReceived {
                session_id,
                turn_applied_state,
            } => self.collect_agent_response_received(session_id, turn_applied_state),
            AppEvent::StackedParentTurnCompleted { session_id } => {
                self.collect_stacked_parent_turn_completed(session_id);
            }
            AppEvent::StackedParentSyncCompleted { session_id } => {
                self.collect_stacked_parent_sync_completed(session_id);
            }
            AppEvent::SessionWorkflowNoticeUpdated { notice, session_id } => {
                self.collect_session_workflow_notice_updated(session_id, notice);
            }
            AppEvent::PublishedBranchSyncUpdated {
                session_id,
                sync_operation_id,
                sync_status,
            } => self.collect_published_branch_sync_updated(
                session_id,
                sync_operation_id,
                sync_status,
            ),
            AppEvent::ReviewRequestStatusUpdated {
                generation,
                result,
                session_id,
            } => self.collect_review_request_status_updated(generation, result, session_id),
            AppEvent::ReviewCommentsUpdated { session_id } => {
                self.collect_review_comments_updated(session_id);
            }
            _ => unreachable!("top-level app event should be collected before runtime events"),
        }
    }

    /// Keeps the freshest requested-review result when multiple refreshes
    /// complete during one drained event batch.
    fn collect_requested_reviews_loaded(
        &mut self,
        generation: u64,
        project_id: i64,
        result: Result<Vec<ag_forge::RequestedReview>, String>,
    ) {
        if self
            .requested_reviews
            .as_ref()
            .is_none_or(|(batched_generation, _, _)| generation >= *batched_generation)
        {
            self.requested_reviews = Some((generation, project_id, result));
        }
    }

    /// Stores every requested-review comment snapshot result because users can
    /// open multiple review details while earlier background loads are still
    /// in flight.
    fn collect_requested_review_comment_snapshot_loaded(
        &mut self,
        display_id: String,
        generation: u64,
        project_id: i64,
        result: Result<ag_forge::ReviewCommentSnapshot, String>,
        web_url: String,
    ) {
        self.requested_review_comment_snapshots
            .push(RequestedReviewCommentSnapshotUpdate {
                display_id,
                generation,
                project_id,
                result,
                web_url,
            });
    }

    /// Stores a session agent/model update for reducer application.
    fn collect_session_model_updated(
        &mut self,
        session_id: SessionId,
        session_agent: crate::domain::agent::AgentSelection,
    ) {
        self.session_model_updates.insert(session_id, session_agent);
    }

    /// Stores a session reasoning-level update for reducer application.
    fn collect_session_reasoning_level_updated(
        &mut self,
        session_id: SessionId,
        reasoning_level_override: Option<crate::domain::agent::ReasoningLevel>,
    ) {
        self.session_reasoning_level_updates
            .insert(session_id, reasoning_level_override);
    }

    /// Stores a workflow notice update and marks its session as touched.
    fn collect_session_workflow_notice_updated(&mut self, session_id: SessionId, notice: String) {
        self.session_ids.insert(session_id.clone());
        self.session_workflow_notice_updates
            .entry(session_id)
            .or_default()
            .push(notice);
    }

    /// Tracks one review-comment cache update so the reducer redraws and
    /// clears stale diff scroll metrics for that session.
    fn collect_review_comments_updated(&mut self, session_id: SessionId) {
        self.review_comment_session_ids.insert(session_id);
    }

    /// Stores loaded at-mention entries for one session.
    fn collect_at_mention_entries_loaded(
        &mut self,
        session_id: SessionId,
        entries: Vec<FileEntry>,
    ) {
        self.at_mention_entries_updates.insert(session_id, entries);
    }

    /// Stores one pending status-bar update.
    fn collect_update_status_changed(&mut self, update_status: UpdateStatus) {
        self.update_status = Some(update_status);
    }

    /// Stores completed agent CLI version rows for reducer application.
    fn collect_agent_cli_versions_updated(&mut self, agent_clis: Vec<AgentCliInfo>) {
        self.agent_cli_updates = Some(agent_clis);
    }

    /// Stores an active session progress message update for reducer
    /// application.
    fn collect_session_progress_updated(
        &mut self,
        session_id: SessionId,
        progress_message: Option<String>,
    ) {
        self.session_progress_updates
            .insert(session_id, progress_message);
    }

    /// Marks the next reducer application as a session-list refresh.
    fn collect_refresh_sessions(&mut self) {
        self.should_reload_sessions = true;
    }

    /// Marks the next reducer application as a project-list refresh.
    fn collect_refresh_projects(&mut self) {
        self.should_reload_projects = true;
    }

    /// Marks git status polling for restart.
    fn collect_refresh_git_status(&mut self) {
        self.should_refresh_git_status = true;
    }

    /// Stores the latest git status event for this reducer batch.
    fn collect_git_status_updated(
        &mut self,
        generation: u64,
        session_statuses: HashMap<SessionId, SessionGitStatus>,
        status: Option<(u32, u32)>,
    ) {
        if self
            .git_status_update
            .as_ref()
            .is_none_or(|batched_update| generation >= batched_update.generation)
        {
            self.git_status_update = Some(GitStatusBatchUpdate { generation, status });
            self.session_git_status_updates = session_statuses;
        }
    }

    /// Stores the latest version availability event for this reducer batch.
    fn collect_version_availability_updated(&mut self, latest_available_version: Option<String>) {
        self.latest_available_version_update = Some(LatestAvailableVersionUpdate {
            latest_available_version,
        });
    }

    /// Stores the latest default-branch sync result for this reducer batch.
    fn collect_sync_main_completed(
        &mut self,
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    ) {
        if result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.sync_main_result = Some(result);
    }

    /// Stores the latest branch-publish action result for this reducer batch.
    fn collect_branch_publish_action_completed(
        &mut self,
        restore_view: ConfirmationViewMode,
        result: BranchPublishTaskResult,
        session_id: SessionId,
    ) {
        if result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.branch_publish_action_update = Some(BranchPublishActionUpdate {
            restore_view,
            result,
            session_id,
        });
    }

    /// Stores a successful focused-review preparation result.
    fn collect_review_prepared(
        &mut self,
        diff_hash: u64,
        review_text: String,
        session_id: SessionId,
    ) {
        self.review_updates.insert(
            session_id,
            ReviewUpdate {
                diff_hash,
                result: Ok(review_text),
            },
        );
    }

    /// Stores a failed focused-review preparation result.
    fn collect_review_preparation_failed(
        &mut self,
        diff_hash: u64,
        error: String,
        session_id: SessionId,
    ) {
        self.review_updates.insert(
            session_id,
            ReviewUpdate {
                diff_hash,
                result: Err(error),
            },
        );
    }

    /// Queues one published-branch sync state transition for ordered
    /// reducer application.
    fn collect_published_branch_sync_updated(
        &mut self,
        session_id: SessionId,
        sync_operation_id: String,
        sync_status: PublishedBranchSyncStatus,
    ) {
        if matches!(
            sync_status,
            PublishedBranchSyncStatus::Idle | PublishedBranchSyncStatus::Succeeded
        ) {
            self.should_refresh_git_status = true;
        }

        self.published_branch_sync_updates.push((
            session_id,
            PublishedBranchSyncUpdate {
                sync_operation_id,
                sync_status,
            },
        ));
    }

    /// Queues one review-request status refresh result for reducer
    /// application.
    fn collect_review_request_status_updated(
        &mut self,
        generation: u64,
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: SessionId,
    ) {
        self.review_request_status_updates
            .push(ReviewRequestStatusUpdate {
                generation,
                result,
                session_id,
            });
    }

    /// Stores the latest reduced handle version for one touched session.
    fn collect_session_updated(&mut self, session_id: SessionId, version: u64) {
        self.session_ids.insert(session_id.clone());
        self.session_update_versions.insert(session_id, version);
    }

    /// Merges one completed-turn projection into the per-session batch.
    ///
    /// Agent responses also mark the session as touched so the reducer still
    /// synchronizes handle-backed status and evaluates auto-review startup
    /// even when the matching `SessionUpdated` event lands in a later tick.
    /// Latest reducer-facing fields replace the older projection, while token
    /// deltas accumulate to preserve usage across multiple queued completions
    /// for the same session.
    fn collect_agent_response_received(
        &mut self,
        session_id: SessionId,
        turn_applied_state: TurnAppliedState,
    ) {
        self.session_ids.insert(session_id.clone());

        match self.applied_turns.entry(session_id) {
            Entry::Occupied(mut occupied_entry) => {
                occupied_entry.get_mut().merge_newer(turn_applied_state);
            }
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(turn_applied_state);
            }
        }
    }

    /// Stores one completed parent turn for stacked-child auto-sync fan-out.
    fn collect_stacked_parent_turn_completed(&mut self, session_id: SessionId) {
        self.stacked_parent_turns_completed.insert(session_id);
    }

    /// Stores one completed parent sync for stacked-child auto-sync fan-out.
    fn collect_stacked_parent_sync_completed(&mut self, session_id: SessionId) {
        self.stacked_parent_syncs_completed.insert(session_id);
    }

    /// Collects one structured system log event for ordered reducer
    /// application.
    fn collect_system_log(&mut self, event: SystemLogEvent) {
        self.system_log_events.push(event);
    }
}

impl App {
    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = AppEventReducer::drain(&mut self.event_rx, first_event);
        let mut event_batch = AppEventBatch::default();
        for event in drained_events {
            event_batch.collect_event(event);
        }

        self.apply_app_event_batch(event_batch).await;
    }

    /// Processes currently queued app events without waiting.
    ///
    /// The foreground runtime calls this before draw so queued
    /// `SessionUpdated` events can synchronize only the touched sessions into
    /// render snapshots without polling every live handle each frame.
    pub(crate) async fn process_pending_app_events(&mut self) {
        let Ok(first_event) = self.event_rx.try_recv() else {
            return;
        };

        self.apply_app_events(first_event).await;
    }

    /// Waits for the next internal app event.
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Applies one reduced app-event batch to in-memory app state.
    ///
    /// The reducer first records whether the batch changes any render-visible
    /// state, applies global runtime updates, and then synchronizes touched
    /// session snapshots from their live handles. Any touched session that
    /// reached terminal status (`Done`, `Canceled`) then drops its worker queue
    /// so background workers can shut down provider runtimes.
    async fn apply_app_event_batch(&mut self, mut event_batch: AppEventBatch) {
        let sync_generation_for_review_updates = self.sync_handle.current_generation();
        let mut should_mark_dirty = Self::app_event_batch_changes_observable_state(&event_batch);
        let previous_session_states = self.previous_session_states(&event_batch.session_ids);

        self.apply_system_log_events(std::mem::take(&mut event_batch.system_log_events));

        should_mark_dirty |=
            self.update_session_redraw_versions(&event_batch.session_update_versions);

        self.apply_batch_runtime_updates(&mut event_batch).await;

        self.apply_batch_session_snapshot_updates(&mut event_batch);

        let focused_review_persistence = apply_review_updates(
            &mut self.review_cache,
            &mut self.mode,
            self.sessions.state_mut(),
            event_batch.review_updates,
        );
        self.persist_focused_review_updates(focused_review_persistence)
            .await;

        if let Some(branch_publish_action_update) = event_batch.branch_publish_action_update {
            self.apply_branch_publish_action_update(branch_publish_action_update);
        }

        let applied_review_request_status_update = event_batch
            .review_request_status_updates
            .iter()
            .any(|update| update.generation == sync_generation_for_review_updates);
        for review_request_status_update in event_batch.review_request_status_updates {
            if review_request_status_update.generation != sync_generation_for_review_updates {
                continue;
            }
            self.apply_review_request_status_update(review_request_status_update)
                .await;
        }
        if applied_review_request_status_update {
            self.publish_sync_context();
        }

        self.invalidate_diff_scroll_cache_for_review_comments(
            &event_batch.review_comment_session_ids,
        );

        self.apply_session_progress_updates(std::mem::take(
            &mut event_batch.session_progress_updates,
        ));

        for (session_id, turn_applied_state) in event_batch.applied_turns {
            self.apply_agent_response_received(&session_id, &turn_applied_state);
        }
        for (session_id, sync_update) in event_batch.published_branch_sync_updates {
            self.apply_published_branch_sync_update(&session_id, sync_update);
        }

        self.sync_touched_sessions(&event_batch.session_ids);
        for (session_id, notices) in
            std::mem::take(&mut event_batch.session_workflow_notice_updates)
        {
            for notice in notices {
                self.sessions.append_workflow_notice(&session_id, notice);
            }
        }
        event_batch
            .stacked_parent_turns_completed
            .extend(std::mem::take(
                &mut event_batch.stacked_parent_syncs_completed,
            ));
        let auto_rebase_log_events = self
            .start_stacked_child_rebases_after_parent_turns(std::mem::take(
                &mut event_batch.stacked_parent_turns_completed,
            ))
            .await;
        if !auto_rebase_log_events.is_empty() {
            should_mark_dirty = true;
            self.apply_system_log_events(auto_rebase_log_events);
        }

        auto_start_reviews(
            &mut self.review_cache,
            &event_batch.session_ids,
            self.sessions.state_mut(),
            &mut self.mode,
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_model,
        )
        .await;

        if let Some(sync_main_result) = event_batch.sync_main_result {
            let sync_popup_context = self.sync_popup_context();
            self.apply_system_log_events(vec![Self::sync_main_result_log_event(
                &sync_main_result,
                &sync_popup_context,
            )]);

            self.mode = Self::sync_main_popup_mode(sync_main_result, &sync_popup_context);
        }

        let session_status_log_events =
            self.session_status_transition_log_events(&previous_session_states);
        if !session_status_log_events.is_empty() {
            should_mark_dirty = true;
            self.apply_system_log_events(session_status_log_events);
        }

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.retain_valid_session_progress_messages();
        self.sessions.retain_active_prompt_outputs();

        if should_mark_dirty {
            self.mark_dirty();
        }
    }

    /// Starts automatic sync rebases for stacked children after their parent
    /// has returned to a review-ready state.
    async fn start_stacked_child_rebases_after_parent_turns(
        &mut self,
        parent_session_ids: HashSet<SessionId>,
    ) -> Vec<SystemLogEvent> {
        let mut events = Vec::new();
        for parent_session_id in parent_session_ids {
            let failures = self
                .sessions
                .rebase_stacked_children_after_parent_turn(
                    &self.services,
                    parent_session_id.as_str(),
                )
                .await;
            for (child_session_id, error) in failures {
                events.push(
                    SystemLogEvent::new(
                        SystemLogLevel::Warning,
                        SystemLogCategory::Session,
                        "Stacked child auto-sync failed",
                    )
                    .with_detail(format!("{child_session_id}: {error}")),
                );
            }
        }

        events
    }

    /// Clears the diff scroll limit cache when review comments change for the
    /// currently visible diff session.
    fn invalidate_diff_scroll_cache_for_review_comments(
        &mut self,
        review_comment_session_ids: &HashSet<SessionId>,
    ) {
        let AppMode::Diff {
            scroll_cache,
            session_id,
            ..
        } = &mut self.mode
        else {
            return;
        };

        if review_comment_session_ids.contains(session_id) {
            *scroll_cache = None;
        }
    }

    /// Applies reducer-batch updates that affect global app runtime state
    /// before session-local projections are synchronized.
    async fn apply_batch_runtime_updates(&mut self, event_batch: &mut AppEventBatch) {
        if event_batch.should_reload_sessions {
            self.refresh_sessions_now().await;
        }

        if event_batch.should_reload_projects {
            self.reload_projects().await;
        }

        if event_batch.should_refresh_git_status {
            self.restart_git_status_task();
        }

        if let Some(agent_clis) = event_batch.agent_cli_updates.take() {
            self.services.replace_available_agent_clis(agent_clis);
        }

        if let Some(git_status_update) = &event_batch.git_status_update
            && git_status_update.generation == self.sync_handle.current_generation()
        {
            self.projects.set_git_status(git_status_update.status);
            self.sessions
                .replace_session_git_statuses(event_batch.session_git_status_updates.clone());
        }

        if let Some((generation, project_id, result)) = event_batch.requested_reviews.take()
            && project_id == self.projects.active_project_id()
            && self
                .requested_reviews
                .matches_loading_request(project_id, generation)
        {
            match result {
                Ok(items) => {
                    self.replace_requested_reviews(project_id, items);
                }
                Err(message) => {
                    self.reset_requested_review_table_state();
                    self.requested_review_selected_index = None;

                    self.requested_reviews = app::RequestedReviewState::Failed {
                        message,
                        project_id,
                    };
                }
            }
        }

        for requested_review_comment_snapshot in
            std::mem::take(&mut event_batch.requested_review_comment_snapshots)
        {
            self.apply_requested_review_comment_snapshot_update(requested_review_comment_snapshot);
        }

        self.apply_status_bar_updates(
            event_batch.latest_available_version_update.as_ref(),
            event_batch.update_status.take(),
        );
    }

    /// Clears the in-flight marker for one requested-review comment snapshot
    /// result, then applies it to the current detail page and cached Review
    /// tab row when it still matches the active project.
    fn apply_requested_review_comment_snapshot_update(
        &mut self,
        update: RequestedReviewCommentSnapshotUpdate,
    ) {
        let RequestedReviewCommentSnapshotUpdate {
            display_id,
            generation,
            project_id,
            result,
            web_url,
        } = update;
        let was_in_flight =
            self.requested_review_comment_fetches
                .remove(&RequestedReviewCommentFetchKey {
                    display_id: display_id.clone(),
                    generation,
                    project_id,
                    web_url: web_url.clone(),
                });
        if !was_in_flight {
            return;
        }
        if project_id != self.projects.active_project_id() {
            return;
        }

        match result {
            Ok(comment_snapshot) => {
                self.cache_requested_review_comment_snapshot(
                    &display_id,
                    &web_url,
                    &comment_snapshot,
                );
                self.apply_requested_review_detail_comment_success(
                    &display_id,
                    &web_url,
                    comment_snapshot,
                );
            }
            Err(error) => {
                self.apply_requested_review_detail_comment_error(
                    &display_id,
                    &web_url,
                    format!("Failed to load review comments: {error}"),
                );
            }
        }
    }

    /// Applies a successful comment snapshot only when the same review detail
    /// page is still visible.
    fn apply_requested_review_detail_comment_success(
        &mut self,
        display_id: &str,
        web_url: &str,
        comment_snapshot: ag_forge::ReviewCommentSnapshot,
    ) {
        let AppMode::ReviewDetail {
            comment_error,
            is_loading_comments,
            review,
            ..
        } = &mut self.mode
        else {
            return;
        };
        if review.display_id != display_id || review.web_url != web_url {
            return;
        }

        review.comment_snapshot = Some(comment_snapshot);
        *comment_error = None;
        *is_loading_comments = false;
    }

    /// Applies a comment-load error only when the same review detail page is
    /// still visible.
    fn apply_requested_review_detail_comment_error(
        &mut self,
        display_id: &str,
        web_url: &str,
        error: String,
    ) {
        let AppMode::ReviewDetail {
            comment_error,
            is_loading_comments,
            review,
            ..
        } = &mut self.mode
        else {
            return;
        };
        if review.display_id != display_id || review.web_url != web_url {
            return;
        }
        if review.comment_snapshot.is_some() {
            return;
        }

        *comment_error = Some(error);
        *is_loading_comments = false;
    }

    /// Synchronizes touched sessions from their runtime handles and drops
    /// worker queues for sessions that reached a terminal status.
    fn sync_touched_sessions(&mut self, session_ids: &HashSet<SessionId>) {
        for session_id in session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }

        self.sessions.clear_terminal_session_workers(session_ids);
    }

    /// Applies status-bar state updates carried by one reducer batch.
    fn apply_status_bar_updates(
        &mut self,
        latest_available_version_update: Option<&LatestAvailableVersionUpdate>,
        update_status: Option<UpdateStatus>,
    ) {
        if let Some(latest_available_version_update) = latest_available_version_update {
            self.latest_available_version
                .clone_from(&latest_available_version_update.latest_available_version);
        }

        if let Some(update_status) = update_status {
            self.update_status = Some(update_status);
        }
    }

    /// Returns status snapshots for sessions touched before applying a
    /// reducer batch.
    fn previous_session_states(
        &self,
        session_ids: &HashSet<SessionId>,
    ) -> HashMap<SessionId, Status> {
        session_ids
            .iter()
            .filter_map(|session_id| {
                self.sessions
                    .sessions()
                    .iter()
                    .find(|session| session.id == *session_id)
                    .map(|session| (session_id.clone(), session.status))
            })
            .collect()
    }

    /// Returns whether one reduced event batch changes any render-visible
    /// application state before `SessionUpdated` version deduplication.
    fn app_event_batch_changes_observable_state(event_batch: &AppEventBatch) -> bool {
        event_batch.should_reload_sessions
            || event_batch.should_reload_projects
            || event_batch.agent_cli_updates.is_some()
            || event_batch.git_status_update.is_some()
            || event_batch.latest_available_version_update.is_some()
            || event_batch.update_status.is_some()
            || !event_batch.applied_turns.is_empty()
            || !event_batch.at_mention_entries_updates.is_empty()
            || event_batch.branch_publish_action_update.is_some()
            || !event_batch.published_branch_sync_updates.is_empty()
            || !event_batch.review_request_status_updates.is_empty()
            || !event_batch.review_comment_session_ids.is_empty()
            || event_batch.requested_reviews.is_some()
            || !event_batch.requested_review_comment_snapshots.is_empty()
            || !event_batch.review_updates.is_empty()
            || !event_batch.session_model_updates.is_empty()
            || !event_batch.session_progress_updates.is_empty()
            || !event_batch.session_reasoning_level_updates.is_empty()
            || !event_batch.session_size_updates.is_empty()
            || !event_batch.session_title_generation_finished.is_empty()
            || !event_batch.session_workflow_notice_updates.is_empty()
            || !event_batch.stacked_parent_syncs_completed.is_empty()
            || !event_batch.stacked_parent_turns_completed.is_empty()
            || event_batch.sync_main_result.is_some()
            || !event_batch.system_log_events.is_empty()
    }

    /// Records reducer-batched system log events with the current app clock.
    fn apply_system_log_events(&mut self, system_log_events: Vec<SystemLogEvent>) {
        if system_log_events.is_empty() {
            return;
        }

        let timestamp_unix_seconds =
            session::unix_timestamp_from_system_time(self.services.clock().now_system_time());
        for system_log_event in system_log_events {
            self.system_logs
                .push(timestamp_unix_seconds, system_log_event);
        }
    }

    /// Builds the system log event for one completed manual sync workflow.
    fn sync_main_result_log_event(
        sync_main_result: &Result<SyncMainOutcome, SyncSessionStartError>,
        sync_popup_context: &SyncPopupContext,
    ) -> SystemLogEvent {
        match sync_main_result {
            Ok(sync_main_outcome) => SystemLogEvent::new(
                SystemLogLevel::Success,
                SystemLogCategory::Sync,
                "Manual project sync completed",
            )
            .with_detail(format!(
                "{} on {}: pulled {}, pushed {}, resolved {} conflicts",
                sync_popup_context.project_name,
                sync_popup_context.default_branch,
                sync_main_outcome.pulled_commits.unwrap_or(0),
                sync_main_outcome.pushed_commits.unwrap_or(0),
                sync_main_outcome.resolved_conflict_files.len(),
            )),
            Err(SyncSessionStartError::MainHasUncommittedChanges { default_branch }) => {
                SystemLogEvent::new(
                    SystemLogLevel::Warning,
                    SystemLogCategory::Sync,
                    "Manual project sync blocked",
                )
                .with_detail(format!(
                    "{} on {default_branch}: uncommitted changes",
                    sync_popup_context.project_name
                ))
            }
            Err(sync_error @ SyncSessionStartError::Other(_)) => SystemLogEvent::new(
                SystemLogLevel::Error,
                SystemLogCategory::Sync,
                "Manual project sync failed",
            )
            .with_detail(format!(
                "{} on {}: {}",
                sync_popup_context.project_name,
                sync_popup_context.default_branch,
                sync_error.detail_message()
            )),
        }
    }

    /// Builds system log events for sessions whose rendered status changed
    /// during the current reducer batch.
    fn session_status_transition_log_events(
        &self,
        previous_session_states: &HashMap<SessionId, Status>,
    ) -> Vec<SystemLogEvent> {
        let mut events = Vec::new();

        for (session_id, previous_status) in previous_session_states {
            let Some(session) = self
                .sessions
                .sessions()
                .iter()
                .find(|session| session.id == *session_id)
            else {
                continue;
            };
            if session.status == *previous_status {
                continue;
            }

            events.push(
                SystemLogEvent::new(
                    SystemLogLevel::Info,
                    SystemLogCategory::Session,
                    "Session status changed",
                )
                .with_detail(format!(
                    "{}: {} -> {} ({})",
                    session.id,
                    previous_status,
                    session.status,
                    session.display_title()
                )),
            );
        }

        events
    }

    /// Updates the last-seen session-handle versions and returns whether any
    /// carried version is newer than the reduced value already applied.
    fn update_session_redraw_versions(
        &mut self,
        session_update_versions: &HashMap<SessionId, u64>,
    ) -> bool {
        let mut did_change = false;

        for (session_id, version) in session_update_versions {
            let previous_version = self
                .last_seen_session_update_versions
                .insert(session_id.clone(), *version);

            if previous_version != Some(*version) {
                did_change = true;
            }
        }

        did_change
    }

    /// Applies reducer batch updates that mutate cached session snapshots or
    /// auxiliary session-view lookup state.
    fn apply_batch_session_snapshot_updates(&mut self, event_batch: &mut AppEventBatch) {
        for (session_id, session_agent) in std::mem::take(&mut event_batch.session_model_updates) {
            self.sessions
                .apply_session_model_updated(&session_id, session_agent);
        }

        for (session_id, reasoning_level_override) in
            std::mem::take(&mut event_batch.session_reasoning_level_updates)
        {
            self.sessions
                .apply_session_reasoning_level_updated(&session_id, reasoning_level_override);
        }

        for (session_id, (added_lines, deleted_lines, session_size)) in
            std::mem::take(&mut event_batch.session_size_updates)
        {
            self.sessions.apply_session_size_updated(
                &session_id,
                added_lines,
                deleted_lines,
                session_size,
            );
        }

        for (session_id, generation) in
            std::mem::take(&mut event_batch.session_title_generation_finished)
        {
            self.sessions
                .clear_title_generation_task_if_matches(&session_id, generation);
        }

        for (session_id, entries) in std::mem::take(&mut event_batch.at_mention_entries_updates) {
            self.sessions.set_at_mention_index_for_root(
                self.at_mention_lookup_root(&session_id),
                entries.clone(),
            );

            self.apply_prompt_at_mention_entries(&session_id, entries);
        }
    }

    /// Applies active progress message updates from one reducer batch.
    fn apply_session_progress_updates(
        &mut self,
        session_progress_updates: HashMap<SessionId, Option<String>>,
    ) {
        for (session_id, progress_message) in session_progress_updates {
            if let Some(progress_message) = progress_message {
                self.session_progress_messages
                    .insert(session_id, progress_message);
            } else {
                self.session_progress_messages.remove(&session_id);
            }
        }
    }

    /// Routes one persisted turn projection to the currently focused session
    /// UI.
    ///
    /// The session worker persists the canonical summary, clarification
    /// questions, summary, and token-usage delta before sending this
    /// event, so the reducer can apply the exact same projection in memory
    /// without waiting for a forced reload.
    fn apply_agent_response_received(
        &mut self,
        session_id: &str,
        turn_applied_state: &TurnAppliedState,
    ) {
        if !self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.sessions
            .apply_turn_applied_state(session_id, turn_applied_state);
        let questions = turn_applied_state.questions.clone();
        if questions.is_empty() {
            return;
        }

        if self.is_viewing_session(session_id) {
            let (review_status_message, review_text) = self.question_mode_review_state(session_id);
            self.mode = AppMode::Question {
                at_mention_state: None,
                selected_option_index: question::default_option_index(&questions, 0),
                session_id: session_id.into(),
                questions,
                review_status_message,
                review_text,
                responses: Vec::new(),
                current_index: 0,
                focus: QuestionFocus::Answer,
                input: InputState::default(),
                scroll_offset: None,
            };
        }
    }

    /// Returns whether the active UI mode currently shows the provided
    /// session.
    fn is_viewing_session(&self, session_id: &str) -> bool {
        match &self.mode {
            AppMode::View {
                session_id: view_id,
                ..
            }
            | AppMode::Prompt {
                session_id: view_id,
                ..
            }
            | AppMode::Diff {
                session_id: view_id,
                ..
            }
            | AppMode::Question {
                session_id: view_id,
                ..
            }
            | AppMode::OpenCommandSelector {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            }
            | AppMode::PublishBranchInput {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            }
            | AppMode::ViewInfoPopup {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            } => view_id == session_id,
            AppMode::List
            | AppMode::ReviewDetail { .. }
            | AppMode::SessionCreation { .. }
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Help { .. } => false,
        }
    }

    /// Returns the focused-review state that should remain visible when the
    /// UI enters clarification-question mode for the provided session.
    fn question_mode_review_state(&self, session_id: &str) -> (Option<String>, Option<String>) {
        match &self.mode {
            AppMode::View {
                review_status_message,
                review_text,
                ..
            }
            | AppMode::Prompt {
                review_status_message,
                review_text,
                ..
            }
            | AppMode::Question {
                review_status_message,
                review_text,
                ..
            } => (review_status_message.clone(), review_text.clone()),
            AppMode::OpenCommandSelector { restore_view, .. }
            | AppMode::PublishBranchInput { restore_view, .. }
            | AppMode::ViewInfoPopup { restore_view, .. } => (
                restore_view.review_status_message.clone(),
                restore_view.review_text.clone(),
            ),
            AppMode::Diff {
                session_id: diff_session_id,
                ..
            } if diff_session_id == session_id => self.review_view_state(session_id),
            AppMode::List
            | AppMode::ReviewDetail { .. }
            | AppMode::SessionCreation { .. }
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => (None, None),
        }
    }

    /// Routes one published-branch auto-push update to the matching in-memory
    /// session snapshot.
    fn apply_published_branch_sync_update(
        &mut self,
        session_id: &str,
        sync_update: PublishedBranchSyncUpdate,
    ) {
        let PublishedBranchSyncUpdate {
            sync_operation_id,
            sync_status,
        } = sync_update;

        match sync_status {
            PublishedBranchSyncStatus::InProgress => {
                self.sessions
                    .start_published_branch_sync(session_id, sync_operation_id);
            }
            PublishedBranchSyncStatus::Idle
            | PublishedBranchSyncStatus::Succeeded
            | PublishedBranchSyncStatus::Failed => {
                self.sessions.finish_published_branch_sync(
                    session_id,
                    &sync_operation_id,
                    sync_status,
                );
            }
        }
    }

    /// Returns the lookup root for the session associated with an at-mention
    /// event.
    fn at_mention_lookup_root(&self, session_id: &str) -> PathBuf {
        let project_working_dir = self.working_dir().to_path_buf();

        self.sessions.session_for_id(session_id).map_or_else(
            || project_working_dir.clone(),
            |session| {
                let project_working_dir = project_working_dir.clone();
                let session_folder = session.folder.clone();
                let has_session_folder = self.services.fs_client().is_dir(session_folder.clone());

                at_mention::lookup_root(
                    project_working_dir,
                    Some(session_folder),
                    has_session_folder,
                )
            },
        )
    }

    /// Applies loaded at-mention entries to the currently focused prompt or
    /// question session, if the mention query is still active.
    fn apply_prompt_at_mention_entries(&mut self, session_id: &str, entries: Vec<FileEntry>) {
        let (at_mention_state, has_query) = match &mut self.mode {
            AppMode::Prompt {
                at_mention_state,
                input,
                session_id: mode_session_id,
                ..
            } if mode_session_id == session_id => {
                (at_mention_state, input.at_mention_query().is_some())
            }
            AppMode::Question {
                at_mention_state,
                input,
                session_id: mode_session_id,
                ..
            } if mode_session_id == session_id => {
                (at_mention_state, input.at_mention_query().is_some())
            }
            _ => return,
        };

        if !has_query {
            return;
        }

        if let Some(state) = at_mention_state.as_mut() {
            state.all_entries = entries;
            state.selected_index = 0;

            return;
        }

        *at_mention_state = Some(PromptAtMentionState::new(entries));
    }

    /// Applies one review assist update to cache and focused render state.
    #[cfg(test)]
    pub(super) fn apply_review_update(
        &mut self,
        session_id: &str,
        review_update: app::review::ReviewUpdate,
    ) {
        let mut review_updates = HashMap::new();
        review_updates.insert(SessionId::from(session_id), review_update);
        apply_review_updates(
            &mut self.review_cache,
            &mut self.mode,
            self.sessions.state_mut(),
            review_updates,
        );
    }

    /// Persists successful focused reviews and clears stale saved review text
    /// after failed regeneration attempts.
    async fn persist_focused_review_updates(
        &self,
        focused_review_persistence: Vec<FocusedReviewPersistence>,
    ) {
        for persistence_update in focused_review_persistence {
            let diff_hash = persistence_update
                .diff_hash
                .map(|diff_hash| diff_hash.to_string());

            let _ = self
                .services
                .db()
                .sessions()
                .update_session_focused_review(
                    persistence_update.session_id.as_str(),
                    diff_hash,
                    persistence_update.text,
                )
                .await;
        }
    }

    /// Starts focused review generation for sessions that just entered review.
    #[cfg(test)]
    pub(super) async fn auto_start_reviews(&mut self, session_ids: &HashSet<SessionId>) {
        auto_start_reviews(
            &mut self.review_cache,
            session_ids,
            self.sessions.state_mut(),
            &mut self.mode,
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_model,
        )
        .await;
    }

    /// Applies one completed branch-publish action and updates the popup.
    pub(super) fn apply_branch_publish_action_update(
        &mut self,
        branch_publish_action_update: BranchPublishActionUpdate,
    ) {
        let BranchPublishActionUpdate {
            restore_view,
            result,
            session_id,
        } = branch_publish_action_update;

        let popup_mode = match result {
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name,
                review_request_creation,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);

                Self::view_info_popup_mode(
                    Self::branch_publish_success_title(PublishBranchAction::Push),
                    Self::branch_publish_success_message(
                        &branch_name,
                        review_request_creation.as_ref(),
                    ),
                    false,
                    String::new(),
                    restore_view,
                )
            }
            Ok(BranchPublishTaskSuccess::PullRequestPublished {
                branch_name,
                review_request,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);
                self.sessions
                    .apply_review_request(&session_id, review_request.clone());

                Self::view_info_popup_mode(
                    Self::review_request_publish_success_title(&review_request),
                    Self::pull_request_publish_success_message(&branch_name, &review_request),
                    false,
                    String::new(),
                    restore_view,
                )
            }
            Err(failure) => Self::view_info_popup_mode(
                failure.title,
                failure.message,
                false,
                String::new(),
                restore_view,
            ),
        };
        self.mode = popup_mode;
    }

    /// Applies one background review-request status refresh.
    pub(super) async fn apply_review_request_status_update(
        &mut self,
        review_request_status_update: ReviewRequestStatusUpdate,
    ) {
        let ReviewRequestStatusUpdate {
            generation: _,
            result,
            session_id,
        } = review_request_status_update;

        let Ok(task_result) = result else {
            return;
        };

        if let Some(summary) = task_result.summary {
            let _ = self
                .sessions
                .store_review_request_summary(&self.services, &session_id, summary)
                .await;
        }

        match task_result.outcome {
            crate::app::session::SyncReviewRequestOutcome::Merged {
                session_head_hash, ..
            } => {
                if let Some(warning) = self
                    .complete_externally_merged_session(&session_id, session_head_hash)
                    .await
                {
                    self.append_output_for_session(
                        &session_id,
                        &TranscriptNotice::ReviewRequestSyncWarning.format(warning),
                    )
                    .await;
                }
            }
            crate::app::session::SyncReviewRequestOutcome::Closed { .. } => {
                self.cancel_externally_closed_session(&session_id).await;
            }
            crate::app::session::SyncReviewRequestOutcome::Open { .. }
            | crate::app::session::SyncReviewRequestOutcome::NoReviewRequest => {}
        }
    }

    /// Marks one externally merged session `Done`, persists continuation
    /// commit-hash metadata when available, and returns any warning from
    /// worktree cleanup.
    ///
    /// The session is still moved to `Done` when cleanup fails because the
    /// merge already happened upstream, but the caller should surface the
    /// warning to the user.
    async fn complete_externally_merged_session(
        &self,
        session_id: &str,
        session_head_hash: Option<String>,
    ) -> Option<String> {
        let Ok(session) = self.sessions.session_or_err(session_id) else {
            return None;
        };
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return None;
        };

        let mut warnings = Vec::new();

        let commit_hash_persistence_error = if let Some(session_head_hash) = session_head_hash {
            self.services
                .db()
                .sessions()
                .update_session_merged_commit_hash(session_id, Some(session_head_hash))
                .await
                .err()
        } else {
            None
        };
        if let Some(error) = commit_hash_persistence_error {
            warnings.push(format!("Merged commit hash persistence failed: {error}"));
        }

        let folder = session.folder.clone();
        let base_branch = session.base_branch.clone();
        let source_branch = crate::app::session::session_branch(session_id);

        if let Err(error) = crate::app::session::SessionManager::cleanup_merged_session_worktree(
            folder,
            self.services.fs_client(),
            self.services.git_client(),
            source_branch,
            None,
        )
        .await
        {
            warnings.push(format!("Worktree cleanup failed: {error}"));
        }
        if let Err(error) =
            crate::app::session::SessionManager::restack_child_sessions_after_parent_merge(
                self.services.db(),
                session_id,
                &base_branch,
            )
            .await
        {
            warnings.push(format!("Stacked child restack failed: {error}"));
        }

        let app_event_tx = self.services.event_sender();

        SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            &self.services.session_update_versions(),
            session_id,
            Status::Done,
        )
        .await;

        (!warnings.is_empty()).then(|| warnings.join("\n"))
    }

    /// Transitions one externally closed review session to `Canceled`.
    async fn cancel_externally_closed_session(&self, session_id: &str) {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = self.services.event_sender();

        let _ = SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            &self.services.session_update_versions(),
            session_id,
            Status::Canceled,
        )
        .await;
        self.sessions
            .cancel_stacked_child_sessions(&self.services, session_id)
            .await;
    }

    /// Builds a session-view info popup mode with explicit loading metadata.
    pub(super) fn view_info_popup_mode(
        title: String,
        message: String,
        is_loading: bool,
        loading_label: String,
        restore_view: ConfirmationViewMode,
    ) -> AppMode {
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            restore_view,
            title,
        }
    }

    /// Returns the loading popup title for one branch-publish action.
    pub(super) fn branch_publish_loading_title(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_loading_title_text(publish_branch_action)
    }

    /// Returns the loading popup body for one branch-publish action.
    pub(super) fn branch_publish_loading_message(
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<&str>,
    ) -> String {
        branch_publish_loading_message_text(publish_branch_action, remote_branch_name)
    }

    /// Returns the loading spinner label for one branch-publish action.
    pub(super) fn branch_publish_loading_label(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_loading_label_text(publish_branch_action)
    }

    /// Returns the success popup title for a completed branch-publish action.
    pub(super) fn branch_publish_success_title(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_success_title_text(publish_branch_action)
    }

    /// Returns the success popup body for one completed branch push.
    pub(super) fn branch_publish_success_message(
        branch_name: &str,
        review_request_creation: Option<&crate::app::branch_publish::ReviewRequestCreationInfo>,
    ) -> String {
        crate::app::branch_publish::branch_push_success_message(
            branch_name,
            review_request_creation,
        )
    }

    /// Returns the success popup title for one completed review-request
    /// publish.
    pub(super) fn review_request_publish_success_title(
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        crate::app::branch_publish::review_request_publish_success_title(review_request)
    }

    /// Returns the success popup body for one completed review-request
    /// publish.
    pub(super) fn pull_request_publish_success_message(
        branch_name: &str,
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        pull_request_publish_success_message_text(branch_name, review_request)
    }

    /// Builds final sync popup mode from background sync completion result.
    ///
    /// Authentication-related push failures are normalized to actionable
    /// authorization guidance so users can recover quickly.
    pub(super) fn sync_main_popup_mode(
        sync_main_result: Result<SyncMainOutcome, SyncSessionStartError>,
        sync_popup_context: &SyncPopupContext,
    ) -> AppMode {
        match sync_main_result {
            Ok(sync_main_outcome) => AppMode::SyncBlockedPopup {
                project_name: Some(sync_popup_context.project_name.clone()),
                default_branch: Some(sync_popup_context.default_branch.clone()),
                is_loading: false,
                message: Self::sync_success_message(&sync_main_outcome),
                title: "Sync complete".to_string(),
            },
            Err(sync_error @ SyncSessionStartError::MainHasUncommittedChanges { .. }) => {
                AppMode::SyncBlockedPopup {
                    project_name: Some(sync_popup_context.project_name.clone()),
                    default_branch: Some(sync_popup_context.default_branch.clone()),
                    is_loading: false,
                    message: sync_error.detail_message(),
                    title: "Sync blocked".to_string(),
                }
            }
            Err(sync_error @ SyncSessionStartError::Other(_)) => AppMode::SyncBlockedPopup {
                project_name: Some(sync_popup_context.project_name.clone()),
                default_branch: Some(sync_popup_context.default_branch.clone()),
                is_loading: false,
                message: Self::sync_failure_message(&sync_error),
                title: "Sync failed".to_string(),
            },
        }
    }

    /// Builds success copy for sync completion with pull/push/conflict metrics
    /// rendered as markdown sections with empty lines separating pull, push,
    /// and conflict blocks.
    fn sync_success_message(sync_main_outcome: &SyncMainOutcome) -> String {
        let pulled_summary = Self::sync_commit_summary("pulled", sync_main_outcome.pulled_commits);
        let pulled_titles =
            Self::sync_pulled_commit_titles_summary(&sync_main_outcome.pulled_commit_titles);
        let pushed_titles =
            Self::sync_pushed_commit_titles_summary(&sync_main_outcome.pushed_commit_titles);
        let pushed_summary = Self::sync_commit_summary("pushed", sync_main_outcome.pushed_commits);
        let conflict_summary =
            Self::sync_conflict_summary(&sync_main_outcome.resolved_conflict_files);

        sync_blocked::format_sync_success_message(
            &pulled_summary,
            &pulled_titles,
            &pushed_summary,
            &pushed_titles,
            &conflict_summary,
        )
    }

    /// Returns pulled commit titles formatted as an indented list.
    fn sync_pulled_commit_titles_summary(pulled_commit_titles: &[String]) -> String {
        if pulled_commit_titles.is_empty() {
            return String::new();
        }

        pulled_commit_titles
            .iter()
            .map(|title| format!("  - {title}"))
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Returns pushed commit titles formatted as an indented list.
    fn sync_pushed_commit_titles_summary(pushed_commit_titles: &[String]) -> String {
        if pushed_commit_titles.is_empty() {
            return String::new();
        }

        pushed_commit_titles
            .iter()
            .map(|title| format!("  - {title}"))
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Returns sync failure copy with actionable guidance for auth failures.
    ///
    /// Authentication failures show a dismiss-only message so users can fix
    /// credentials first, then restart sync from the list. When the failing
    /// remote host is recognizable, the guidance names the matching forge CLI.
    fn sync_failure_message(sync_error: &SyncSessionStartError) -> String {
        let detail_message = sync_error.detail_message();
        if !is_git_push_authentication_error(&detail_message) {
            return detail_message;
        }

        git_push_authentication_message(
            detected_forge_kind_from_git_push_error(&detail_message),
            "run sync again",
        )
    }

    /// Returns one brief pull/push sentence fragment for sync completion.
    fn sync_commit_summary(direction: &str, commit_count: Option<u32>) -> String {
        match commit_count {
            Some(1) => format!("1 commit {direction}"),
            Some(commit_count) => format!("{commit_count} commits {direction}"),
            None => format!("commits {direction}: unknown"),
        }
    }

    /// Returns one brief conflict-resolution sentence fragment for sync
    /// completion.
    fn sync_conflict_summary(resolved_conflict_files: &[String]) -> String {
        if resolved_conflict_files.is_empty() {
            return "no conflicts fixed".to_string();
        }

        format!("conflicts fixed: {}", resolved_conflict_files.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_refresh_sessions_batch_sets_only_session_reload_scope() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::RefreshSessions);

        // Assert
        assert!(event_batch.should_reload_sessions);
        assert!(!event_batch.should_reload_projects);
    }

    #[test]
    fn test_refresh_projects_batch_sets_only_project_reload_scope() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::RefreshProjects);

        // Assert
        assert!(event_batch.should_reload_projects);
        assert!(!event_batch.should_reload_sessions);
    }

    #[test]
    fn test_requested_review_batch_keeps_newer_generation_when_stale_event_arrives_later() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        let newer_event = AppEvent::RequestedReviewsLoaded {
            generation: 2,
            project_id: 42,
            result: Ok(Vec::new()),
        };
        let stale_event = AppEvent::RequestedReviewsLoaded {
            generation: 1,
            project_id: 42,
            result: Err("stale failure".to_string()),
        };

        // Act
        event_batch.collect_event(newer_event);
        event_batch.collect_event(stale_event);

        // Assert
        let (generation, project_id, result) = event_batch
            .requested_reviews
            .expect("newer requested-review event should be retained");
        assert_eq!(generation, 2);
        assert_eq!(project_id, 42);
        assert_eq!(result.expect("newer result should be successful").len(), 0);
    }
}
