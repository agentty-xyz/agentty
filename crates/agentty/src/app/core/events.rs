//! Event types and reducer helpers for the app core module.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::future::poll_fn;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::Poll;

use app::branch_publish::{
    BranchPublishActionUpdate, BranchPublishTaskResult, BranchPublishTaskSuccess,
    branch_publish_loading_label as branch_publish_loading_label_text,
    branch_publish_success_title as branch_publish_success_title_text,
    detected_forge_kind_from_git_push_error, git_push_authentication_message,
    is_git_push_authentication_error,
    review_request_created_notice as review_request_created_notice_text,
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
    SessionTaskService, StatusTransition, SyncMainOutcome, SyncSessionStartError, TurnAppliedState,
};
use crate::app::session_state::SessionGitStatus;
use crate::app::{self, SessionRuntimeCommand, sync_message};
use crate::domain::agent::AgentCliInfo;
use crate::domain::file_entry::{FileEntry, at_mention_lookup_root};
use crate::domain::input::InputState;
use crate::domain::question::default_option_index;
use crate::domain::session::{
    PublishBranchAction, PublishedBranchSyncStatus, SessionDiffStats, SessionHandles, SessionId,
    Status,
};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::TransientMessageBody;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationViewMode, DiffPreview, DiffPreviewUnavailableReason,
    HelpContext,
};
#[cfg(test)]
use crate::presentation::app_mode::{ReviewCommentAction, ReviewCommentActionSelection};
use crate::presentation::prompt::PromptAtMentionState;
use crate::presentation::review_comment as review_comment_selection;

/// Next foreground-owned runtime event accepted by the app.
pub(crate) enum AppRuntimeEvent {
    /// One event emitted by a background workflow.
    App(Box<AppEvent>),
    /// One API command accepted by the bounded session actor mailbox.
    Session(SessionRuntimeCommand),
}

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Indicates completion of one assigned GitHub issue list refresh.
    AssignedIssuesLoaded {
        /// Refresh generation assigned when the task was spawned.
        generation: u64,
        /// Project id whose assigned issues were loaded.
        project_id: i64,
        /// GitHub CLI result from the background issue task.
        result: Result<Vec<ag_forge::AssignedIssue>, String>,
    },
    /// Indicates completion of one selected GitHub issue detail load.
    IssueDetailLoaded {
        /// Provider display id such as GitHub `#123`.
        display_id: String,
        /// Assigned-issue list generation visible when loading began.
        generation: u64,
        /// Project id whose selected issue was loaded.
        project_id: i64,
        /// Base issue detail result, excluding comments.
        result: Result<ag_forge::IssueDetail, String>,
    },
    /// Indicates background-loaded prompt at-mention entries for one session.
    AtMentionEntriesLoaded {
        entries: Vec<FileEntry>,
        session_id: SessionId,
    },
    /// Indicates completion of one bounded diff-preview worktree read.
    DiffPreviewLoaded {
        /// Selected repository-relative markdown path.
        path: String,
        /// Request generation used to reject stale completions.
        request_id: u64,
        /// Bounded worktree-file result.
        result: Result<ag_git::WorktreeFileContent, String>,
        /// Session whose diff preview requested the file.
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
    /// Indicates a session agent/model selection has been persisted.
    SessionModelUpdated {
        session_id: SessionId,
        session_agent: crate::domain::agent::AgentSelection,
    },
    /// Indicates a session personality selection has been persisted.
    SessionPersonalityUpdated {
        personality_id: Option<String>,
        session_id: SessionId,
    },
    /// Indicates a session reasoning-level selection has been persisted.
    SessionReasoningLevelUpdated {
        reasoning_level: crate::domain::agent::ReasoningLevel,
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
    /// Indicates completion of a linked session review-request comment load.
    SessionReviewCommentSnapshotLoaded {
        /// Comment snapshot result from the background forge task.
        result: Result<ag_forge::ReviewCommentSnapshot, String>,
        /// Session whose comments were requested.
        session_id: SessionId,
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
    /// Indicates list-mode sync is resolving rebase conflicts.
    SyncMainConflictResolutionStarted { conflicted_files: Vec<String> },
    /// Indicates recomputed diff-derived metadata for one session.
    SessionDiffStatsUpdated {
        diff_stats: SessionDiffStats,
        session_id: SessionId,
    },
    /// Indicates one tracked draft-title generation task reached a terminal
    /// outcome and can be pruned from in-memory task tracking.
    SessionTitleGenerationFinished {
        generation: u64,
        session_id: SessionId,
    },
    /// Indicates completion of a session-view branch-publish action.
    BranchPublishActionCompleted {
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
    /// Indicates a parent session merged and its materialized children should
    /// run deterministic restack rebases against the parent's former base.
    StackedParentMergeCompleted { child_session_ids: Vec<SessionId> },
    /// Indicates a transient workflow notice changed for one session.
    SessionWorkflowNoticeUpdated {
        notice: String,
        session_id: SessionId,
    },
    /// Indicates that one published session branch started or finished a
    /// background auto-push after a completed turn.
    PublishedBranchSyncUpdated {
        /// Durable transcript notice promoted into place for a terminal
        /// operation, or `None` for progress-only updates.
        persistent_notice: Option<String>,
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
}

/// Reduced representation of all app events currently queued for one tick.
#[derive(Default)]
pub(super) struct AppEventBatch {
    /// Latest assigned-issue task result collected for this reducer batch.
    pub(super) assigned_issues: Option<(u64, i64, Result<Vec<ag_forge::AssignedIssue>, String>)>,
    /// Ordered selected issue-detail results collected for this reducer batch.
    pub(super) issue_details: Vec<IssueDetailUpdate>,
    pub(super) applied_turns: HashMap<SessionId, TurnAppliedState>,
    pub(super) agent_cli_updates: Option<Vec<AgentCliInfo>>,
    pub(super) at_mention_entries_updates: HashMap<SessionId, Vec<FileEntry>>,
    pub(super) branch_publish_action_updates: Vec<BranchPublishActionUpdate>,
    pub(super) diff_preview_updates: Vec<DiffPreviewUpdate>,
    pub(super) git_status_update: Option<GitStatusBatchUpdate>,
    pub(super) latest_available_version_update: Option<LatestAvailableVersionUpdate>,
    pub(super) published_branch_sync_updates: Vec<(SessionId, PublishedBranchSyncUpdate)>,
    pub(super) review_updates: HashMap<SessionId, ReviewUpdate>,
    pub(super) session_git_status_updates: HashMap<SessionId, SessionGitStatus>,
    pub(super) session_ids: HashSet<SessionId>,
    pub(super) session_update_versions: HashMap<SessionId, u64>,
    pub(super) session_model_updates: HashMap<SessionId, crate::domain::agent::AgentSelection>,
    pub(super) session_personality_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_reasoning_level_updates:
        HashMap<SessionId, crate::domain::agent::ReasoningLevel>,
    pub(super) session_progress_updates: HashMap<SessionId, Option<String>>,
    pub(super) session_review_comment_snapshots:
        HashMap<SessionId, Result<ag_forge::ReviewCommentSnapshot, String>>,
    pub(super) session_diff_stats_updates: HashMap<SessionId, SessionDiffStats>,
    pub(super) stacked_parent_merge_child_rebases: HashSet<SessionId>,
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
    pub(super) review_request_status_updates: Vec<ReviewRequestStatusUpdate>,
    /// Latest requested-review task result collected for this reducer batch,
    /// including its generation for stale-result rejection.
    pub(super) requested_reviews:
        Option<(u64, i64, Result<Vec<ag_forge::RequestedReview>, String>)>,
    pub(super) requested_review_comment_snapshots: Vec<RequestedReviewCommentSnapshotUpdate>,
    pub(super) sync_main_conflicted_files: Option<Vec<String>>,
    pub(super) sync_main_result: Option<Result<SyncMainOutcome, SyncSessionStartError>>,
    pub(super) update_status: Option<UpdateStatus>,
}

/// Completed selected issue-detail load ready for reducer application.
pub(super) struct IssueDetailUpdate {
    pub(super) display_id: String,
    pub(super) generation: u64,
    pub(super) project_id: i64,
    pub(super) result: Result<ag_forge::IssueDetail, String>,
}

/// Completed diff-preview file read ready for stale-safe reducer application.
pub(super) struct DiffPreviewUpdate {
    pub(super) path: String,
    pub(super) request_id: u64,
    pub(super) result: Result<ag_git::WorktreeFileContent, String>,
    pub(super) session_id: SessionId,
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
    /// Durable notice promoted while retracting the matching loading slot.
    persistent_notice: Option<String>,
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
            AppEvent::AssignedIssuesLoaded {
                generation,
                project_id,
                result,
            } => {
                self.collect_assigned_issues_loaded(generation, project_id, result);
            }
            AppEvent::IssueDetailLoaded {
                display_id,
                generation,
                project_id,
                result,
            } => self.issue_details.push(IssueDetailUpdate {
                display_id,
                generation,
                project_id,
                result,
            }),
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
            AppEvent::SessionModelUpdated {
                session_id,
                session_agent,
            } => self.collect_session_model_updated(session_id, session_agent),
            AppEvent::SessionPersonalityUpdated {
                personality_id,
                session_id,
            } => self.collect_session_personality_updated(session_id, personality_id),
            AppEvent::SessionReasoningLevelUpdated {
                reasoning_level,
                session_id,
            } => self.collect_session_reasoning_level_updated(session_id, reasoning_level),
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
            AppEvent::DiffPreviewLoaded {
                path,
                request_id,
                result,
                session_id,
            } => self.diff_preview_updates.push(DiffPreviewUpdate {
                path,
                request_id,
                result,
                session_id,
            }),
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => self.collect_session_progress_updated(session_id, progress_message),
            AppEvent::SessionReviewCommentSnapshotLoaded { result, session_id } => {
                self.session_review_comment_snapshots
                    .insert(session_id, result);
            }
            AppEvent::SyncMainCompleted { result } => self.collect_sync_main_completed(result),
            AppEvent::SyncMainConflictResolutionStarted { conflicted_files } => {
                self.collect_sync_main_conflict_resolution_started(conflicted_files);
            }
            AppEvent::SessionDiffStatsUpdated {
                diff_stats,
                session_id,
            } => {
                self.session_diff_stats_updates
                    .insert(session_id, diff_stats);
            }
            AppEvent::SessionTitleGenerationFinished {
                generation,
                session_id,
            } => {
                self.session_title_generation_finished
                    .insert(session_id, generation);
            }
            AppEvent::BranchPublishActionCompleted { result, session_id } => {
                self.collect_branch_publish_action_completed(*result, session_id);
            }
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
            AppEvent::StackedParentMergeCompleted { child_session_ids } => {
                self.collect_stacked_parent_merge_completed(child_session_ids);
            }
            AppEvent::SessionWorkflowNoticeUpdated { notice, session_id } => {
                self.collect_session_workflow_notice_updated(session_id, notice);
            }
            AppEvent::PublishedBranchSyncUpdated {
                persistent_notice,
                session_id,
                sync_operation_id,
                sync_status,
            } => self.collect_published_branch_sync_updated(
                session_id,
                sync_operation_id,
                sync_status,
                persistent_notice,
            ),
            AppEvent::ReviewRequestStatusUpdated {
                generation,
                result,
                session_id,
            } => self.collect_review_request_status_updated(generation, result, session_id),
            _ => unreachable!("top-level app event should be collected before runtime events"),
        }
    }

    /// Keeps the freshest assigned-issue result when one batch contains
    /// overlapping loads.
    fn collect_assigned_issues_loaded(
        &mut self,
        generation: u64,
        project_id: i64,
        result: Result<Vec<ag_forge::AssignedIssue>, String>,
    ) {
        if self
            .assigned_issues
            .as_ref()
            .is_none_or(|(current_generation, _, _)| generation >= *current_generation)
        {
            self.assigned_issues = Some((generation, project_id, result));
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

    /// Stores a session personality update for reducer application.
    fn collect_session_personality_updated(
        &mut self,
        session_id: SessionId,
        personality_id: Option<String>,
    ) {
        self.session_personality_updates
            .insert(session_id, personality_id);
    }

    /// Stores a session reasoning-level update for reducer application.
    fn collect_session_reasoning_level_updated(
        &mut self,
        session_id: SessionId,
        reasoning_level: crate::domain::agent::ReasoningLevel,
    ) {
        self.session_reasoning_level_updates
            .insert(session_id, reasoning_level);
    }

    /// Stores a workflow notice update and marks its session as touched.
    fn collect_session_workflow_notice_updated(&mut self, session_id: SessionId, notice: String) {
        self.session_ids.insert(session_id.clone());
        self.session_workflow_notice_updates
            .entry(session_id)
            .or_default()
            .push(notice);
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

    /// Stores the latest conflicted-file list for an in-progress sync batch.
    fn collect_sync_main_conflict_resolution_started(&mut self, conflicted_files: Vec<String>) {
        self.sync_main_conflicted_files = Some(conflicted_files);
    }

    /// Stores one branch-publish action result for this reducer batch.
    fn collect_branch_publish_action_completed(
        &mut self,
        result: BranchPublishTaskResult,
        session_id: SessionId,
    ) {
        if result.is_ok() {
            self.should_refresh_git_status = true;
        }

        self.branch_publish_action_updates
            .push(BranchPublishActionUpdate { result, session_id });
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
        persistent_notice: Option<String>,
    ) {
        if matches!(
            sync_status,
            PublishedBranchSyncStatus::Idle | PublishedBranchSyncStatus::Succeeded
        ) {
            self.should_refresh_git_status = true;
        }

        self.session_ids.insert(session_id.clone());
        self.published_branch_sync_updates.push((
            session_id,
            PublishedBranchSyncUpdate {
                persistent_notice,
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

    /// Stores child sessions that need post-merge deterministic restack
    /// rebases after their parent link has been cleared in persistence.
    fn collect_stacked_parent_merge_completed(&mut self, child_session_ids: Vec<SessionId>) {
        self.stacked_parent_merge_child_rebases
            .extend(child_session_ids);
    }
}

impl App {
    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains one bounded batch of currently queued app events,
    /// coalesces refresh and git-status updates within that batch, then applies
    /// session-handle sync for touched sessions. Events beyond the per-cycle
    /// budget remain queued so foreground redraws are not starved.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = AppEventReducer::drain(&mut self.event_rx, first_event);
        let mut event_batch = AppEventBatch::default();
        for event in drained_events {
            event_batch.collect_event(event);
        }

        self.apply_app_event_batch(event_batch).await;
    }

    /// Processes one bounded batch of currently queued app events without
    /// waiting.
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
    #[cfg(test)]
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Waits for either a background app event or a session actor command.
    pub(crate) async fn next_runtime_event(&mut self) -> AppRuntimeEvent {
        let event_rx = &mut self.event_rx;
        let sessions = &mut self.sessions;

        tokio::select! {
            event = poll_fn(|context| match event_rx.poll_recv(context) {
                Poll::Ready(Some(event)) => Poll::Ready(event),
                Poll::Ready(None) | Poll::Pending => Poll::Pending,
            }) => AppRuntimeEvent::App(Box::new(event)),
            command = sessions.next_command() => AppRuntimeEvent::Session(command),
        }
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

        should_mark_dirty |=
            self.update_session_redraw_versions(&event_batch.session_update_versions);

        self.apply_batch_runtime_updates(&mut event_batch).await;

        self.apply_batch_session_snapshot_updates(&mut event_batch);

        let focused_review_persistence = apply_review_updates(
            &mut self.review_cache,
            self.sessions.state_mut(),
            std::mem::take(&mut event_batch.review_updates),
        );
        self.persist_focused_review_updates(focused_review_persistence)
            .await;

        for branch_publish_action_update in
            std::mem::take(&mut event_batch.branch_publish_action_updates)
        {
            self.apply_branch_publish_action_update(branch_publish_action_update)
                .await;
        }

        self.apply_review_request_status_updates_and_synced_merges(
            &mut event_batch,
            sync_generation_for_review_updates,
        )
        .await;

        self.apply_session_progress_updates(std::mem::take(
            &mut event_batch.session_progress_updates,
        ));
        self.apply_session_review_comment_snapshot_updates(std::mem::take(
            &mut event_batch.session_review_comment_snapshots,
        ));

        for (session_id, turn_applied_state) in event_batch.applied_turns {
            self.apply_agent_response_received(&session_id, &turn_applied_state);
        }
        for (session_id, sync_update) in event_batch.published_branch_sync_updates {
            self.apply_published_branch_sync_update(&session_id, sync_update)
                .await;
        }

        if let Some(conflicted_files) = event_batch.sync_main_conflicted_files.as_deref() {
            self.apply_sync_main_conflict_resolution_started(conflicted_files);
        }

        self.sync_touched_sessions(&event_batch.session_ids);
        for (session_id, notices) in
            std::mem::take(&mut event_batch.session_workflow_notice_updates)
        {
            for notice in notices {
                self.sessions.append_workflow_notice(&session_id, notice);
            }
        }
        self.start_stacked_child_rebases_after_parent_merge(std::mem::take(
            &mut event_batch.stacked_parent_merge_child_rebases,
        ))
        .await;
        let mut turned_parent_session_ids =
            std::mem::take(&mut event_batch.stacked_parent_turns_completed);
        turned_parent_session_ids.extend(std::mem::take(
            &mut event_batch.stacked_parent_syncs_completed,
        ));
        self.start_stacked_child_rebases_after_parent_turns(turned_parent_session_ids)
            .await;
        auto_start_reviews(
            &mut self.review_cache,
            &event_batch.session_ids,
            self.sessions.state_mut(),
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_selection,
        )
        .await;
        app::review::hydrate_review_transients(
            &self.review_cache,
            self.sessions.state_mut(),
            self.settings.default_review_selection.model(),
        );

        if let Some(sync_main_result) = event_batch.sync_main_result {
            let sync_popup_context = self.sync_popup_context();
            self.mode = Self::sync_main_popup_mode(sync_main_result, &sync_popup_context);
        }

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.retain_valid_session_progress_messages();
        self.sessions.retain_active_prompt_outputs();

        if should_mark_dirty {
            self.mark_dirty();
        }
    }

    async fn apply_review_request_status_updates_and_synced_merges(
        &mut self,
        event_batch: &mut AppEventBatch,
        sync_generation: u64,
    ) {
        let review_request_status_updates =
            std::mem::take(&mut event_batch.review_request_status_updates);
        let applied_review_request_status_update = review_request_status_updates
            .iter()
            .any(|update| update.generation == sync_generation);
        for review_request_status_update in review_request_status_updates {
            if review_request_status_update.generation != sync_generation {
                continue;
            }
            self.apply_review_request_status_update(review_request_status_update)
                .await;
        }
        if applied_review_request_status_update {
            self.publish_sync_context();
        }

        if let Some(Ok(sync_main_outcome)) = event_batch.sync_main_result.as_mut() {
            let default_branch = sync_main_outcome.default_branch.clone();
            sync_main_outcome.deferred_merged_session_ids = self
                .finalize_merged_sessions_after_main_sync(&default_branch)
                .await;
        }
    }

    /// Applies completed linked-session comment loads only while the matching
    /// comments page remains visible.
    fn apply_session_review_comment_snapshot_updates(
        &mut self,
        updates: HashMap<SessionId, Result<ag_forge::ReviewCommentSnapshot, String>>,
    ) {
        for (loaded_session_id, result) in updates {
            let AppMode::ReviewComments {
                comment_actions,
                comment_error,
                comment_snapshot,
                is_loading_comments,
                selected_comment_index,
                session_id,
                ..
            } = &mut self.mode
            else {
                continue;
            };
            if *session_id != loaded_session_id {
                continue;
            }

            *is_loading_comments = false;
            match result {
                Ok(snapshot) => {
                    review_comment_selection::retain_actionable_selections(
                        comment_actions,
                        &snapshot,
                    );
                    *selected_comment_index = review_comment_selection::retarget_selected_index(
                        comment_snapshot.as_ref(),
                        *selected_comment_index,
                        &snapshot,
                    );
                    *comment_error = None;
                    *comment_snapshot = Some(snapshot);
                }
                Err(error) => {
                    *comment_error = Some(format!("Failed to load review comments: {error}"));
                    *comment_snapshot = None;
                }
            }
        }
    }

    /// Starts automatic sync rebases for stacked children after their parent
    /// has returned to a review-ready state.
    async fn start_stacked_child_rebases_after_parent_turns(
        &mut self,
        parent_session_ids: HashSet<SessionId>,
    ) {
        for parent_session_id in parent_session_ids {
            let failures = self
                .sessions
                .rebase_stacked_children_after_parent_turn(
                    &self.services,
                    parent_session_id.as_str(),
                )
                .await;
            self.sessions
                .append_stacked_rebase_failure_notices(failures, "Stacked child auto-sync failed");
        }
    }

    /// Starts deterministic sync rebases for children retargeted by a parent
    /// merge.
    async fn start_stacked_child_rebases_after_parent_merge(
        &mut self,
        child_session_ids: HashSet<SessionId>,
    ) {
        if child_session_ids.is_empty() {
            return;
        }

        let mut child_session_ids = child_session_ids.into_iter().collect::<Vec<_>>();
        child_session_ids.sort();

        let failures = self
            .sessions
            .rebase_sessions_after_parent_merge(&self.services, child_session_ids)
            .await;
        self.sessions.append_stacked_rebase_failure_notices(
            failures,
            "Stacked child post-merge sync failed",
        );
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

        if let Some((generation, project_id, result)) = event_batch.assigned_issues.take()
            && project_id == self.projects.active_project_id()
            && self
                .assigned_issues
                .matches_loading_request(project_id, generation)
        {
            match result {
                Ok(items) => self.replace_assigned_issues(project_id, items),
                Err(message) => {
                    self.assigned_issue_selected_index = None;
                    self.assigned_issues = app::AssignedIssueState::Failed {
                        message,
                        project_id,
                    };
                }
            }
        }

        for issue_detail in std::mem::take(&mut event_batch.issue_details) {
            self.apply_issue_detail_update(issue_detail);
        }

        for diff_preview_update in std::mem::take(&mut event_batch.diff_preview_updates) {
            self.apply_diff_preview_update(&diff_preview_update);
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

    /// Applies one issue-detail result only to the matching visible page.
    fn apply_issue_detail_update(&mut self, update: IssueDetailUpdate) {
        if update.project_id != self.projects.active_project_id()
            || update.generation != self.assigned_issue_generation
        {
            return;
        }

        let AppMode::IssueDetail {
            detail,
            error,
            issue,
            ..
        } = &mut self.mode
        else {
            return;
        };
        if issue.display_id != update.display_id {
            return;
        }

        match update.result {
            Ok(issue_detail) => {
                *detail = Some(issue_detail);
                *error = None;
            }
            Err(message) => {
                *detail = None;
                *error = Some(format!("Failed to load issue details: {message}"));
            }
        }
    }

    /// Applies a worktree read only to its still-loading diff selection.
    fn apply_diff_preview_update(&mut self, update: &DiffPreviewUpdate) {
        match &mut self.mode {
            AppMode::Diff {
                preview,
                scroll_cache,
                session_id,
                ..
            } if *session_id == update.session_id => {
                if Self::resolve_diff_preview(preview, update) {
                    *scroll_cache = None;
                }
            }
            AppMode::Help {
                context:
                    HelpContext::Diff {
                        preview,
                        session_id,
                        ..
                    },
                ..
            } if *session_id == update.session_id => {
                Self::resolve_diff_preview(preview, update);
            }
            _ => {}
        }
    }

    /// Resolves one matching loading state into ready or unavailable content.
    fn resolve_diff_preview(preview: &mut DiffPreview, update: &DiffPreviewUpdate) -> bool {
        if !matches!(
            preview,
            DiffPreview::Loading { path, request_id }
                if path == &update.path && *request_id == update.request_id
        ) {
            return false;
        }

        let unavailable = |reason| DiffPreview::Unavailable {
            path: update.path.clone(),
            reason,
            request_id: update.request_id,
        };
        *preview = match &update.result {
            Ok(ag_git::WorktreeFileContent::Text(content)) => DiffPreview::Ready {
                content: content.clone(),
                path: update.path.clone(),
                request_id: update.request_id,
            },
            Ok(ag_git::WorktreeFileContent::Missing) => {
                unavailable(DiffPreviewUnavailableReason::Deleted)
            }
            Ok(ag_git::WorktreeFileContent::Binary) => {
                unavailable(DiffPreviewUnavailableReason::Binary)
            }
            Ok(ag_git::WorktreeFileContent::TooLarge) => {
                unavailable(DiffPreviewUnavailableReason::TooLarge)
            }
            Err(error) => unavailable(DiffPreviewUnavailableReason::LoadFailed(error.clone())),
        };

        true
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
            || event_batch.assigned_issues.is_some()
            || event_batch.git_status_update.is_some()
            || !event_batch.issue_details.is_empty()
            || event_batch.latest_available_version_update.is_some()
            || event_batch.update_status.is_some()
            || !event_batch.applied_turns.is_empty()
            || !event_batch.at_mention_entries_updates.is_empty()
            || !event_batch.branch_publish_action_updates.is_empty()
            || !event_batch.diff_preview_updates.is_empty()
            || !event_batch.published_branch_sync_updates.is_empty()
            || !event_batch.review_request_status_updates.is_empty()
            || event_batch.requested_reviews.is_some()
            || !event_batch.requested_review_comment_snapshots.is_empty()
            || !event_batch.review_updates.is_empty()
            || !event_batch.session_model_updates.is_empty()
            || !event_batch.session_personality_updates.is_empty()
            || !event_batch.session_progress_updates.is_empty()
            || !event_batch.session_review_comment_snapshots.is_empty()
            || !event_batch.session_reasoning_level_updates.is_empty()
            || !event_batch.session_diff_stats_updates.is_empty()
            || !event_batch.session_title_generation_finished.is_empty()
            || !event_batch.session_workflow_notice_updates.is_empty()
            || !event_batch.stacked_parent_merge_child_rebases.is_empty()
            || !event_batch.stacked_parent_syncs_completed.is_empty()
            || !event_batch.stacked_parent_turns_completed.is_empty()
            || event_batch.sync_main_conflicted_files.is_some()
            || event_batch.sync_main_result.is_some()
    }

    /// Updates the loading sync popup with the current conflict-resolution
    /// status when the user is still viewing that in-progress sync.
    fn apply_sync_main_conflict_resolution_started(&mut self, conflicted_files: &[String]) {
        if !matches!(
            self.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ..
            }
        ) {
            return;
        }

        let sync_popup_context = self.sync_popup_context();
        self.mode =
            Self::sync_main_conflict_resolution_popup_mode(conflicted_files, &sync_popup_context);
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

        for (session_id, personality_id) in
            std::mem::take(&mut event_batch.session_personality_updates)
        {
            self.sessions
                .apply_session_personality_updated(&session_id, personality_id);
        }

        for (session_id, reasoning_level) in
            std::mem::take(&mut event_batch.session_reasoning_level_updates)
        {
            self.sessions
                .apply_session_reasoning_level_updated(&session_id, reasoning_level);
        }

        for (session_id, diff_stats) in std::mem::take(&mut event_batch.session_diff_stats_updates)
        {
            self.sessions
                .apply_session_diff_stats_updated(&session_id, diff_stats);
        }

        for (session_id, generation) in
            std::mem::take(&mut event_batch.session_title_generation_finished)
        {
            self.sessions
                .clear_title_generation_task_if_matches(&session_id, generation);
        }

        for (session_id, entries) in std::mem::take(&mut event_batch.at_mention_entries_updates) {
            let lookup_root = self.at_mention_lookup_root(&session_id);
            self.sessions
                .set_at_mention_index_for_root(lookup_root, entries.clone());

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
        self.question_progress.remove(session_id);
        let questions = turn_applied_state.questions.clone();
        if questions.is_empty() {
            return;
        }

        if self.is_viewing_session(session_id) {
            self.mode = AppMode::Question {
                at_mention_state: None,
                selected_option_index: default_option_index(&questions, 0),
                session_id: session_id.into(),
                questions,
                responses: Vec::new(),
                current_index: 0,
                focus: ChatFocus::Input,
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
            | AppMode::ReviewComments {
                session_id: view_id,
                ..
            }
            | AppMode::Question {
                session_id: view_id,
                ..
            }
            | AppMode::LaunchConfigurationSelector {
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
            | AppMode::IssueDetail { .. }
            | AppMode::ReviewDetail { .. }
            | AppMode::SessionCreation { .. }
            | AppMode::PreCommitHookWarning { .. }
            | AppMode::ProjectSwitcher { .. }
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Help { .. } => false,
        }
    }

    /// Routes one published-branch auto-push update to the matching in-memory
    /// session snapshot.
    async fn apply_published_branch_sync_update(
        &mut self,
        session_id: &str,
        sync_update: PublishedBranchSyncUpdate,
    ) {
        let PublishedBranchSyncUpdate {
            persistent_notice,
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
                let was_applied = self.sessions.finish_published_branch_sync(
                    session_id,
                    &sync_operation_id,
                    persistent_notice.as_deref(),
                );
                if was_applied && let Some(persistent_notice) = persistent_notice {
                    SessionTaskService::persist_workflow_notice(
                        self.services.db(),
                        session_id,
                        &persistent_notice,
                    )
                    .await;
                }
            }
        }
    }

    /// Returns the lookup root for one session's at-mention entries.
    ///
    /// Materialized sessions use their own worktree. An unmaterialized
    /// stacked draft uses its parent worktree so files introduced by the
    /// parent remain available before the child worktree is created.
    pub(crate) fn at_mention_lookup_root(&self, session_id: &str) -> PathBuf {
        let project_working_dir = self.working_dir().to_path_buf();

        self.sessions.session_for_id(session_id).map_or_else(
            || project_working_dir.clone(),
            |session| {
                let project_working_dir = project_working_dir.clone();
                let session_folder = session.folder.clone();
                let has_session_folder = self.services.fs_client().is_dir(session_folder.clone());
                if has_session_folder {
                    return session_folder;
                }
                let parent_session_folder =
                    session
                        .parent_session_id
                        .as_ref()
                        .and_then(|parent_session_id| {
                            self.sessions
                                .session_for_id(parent_session_id)
                                .map(|parent_session| parent_session.folder.clone())
                        });
                let has_parent_session_folder =
                    parent_session_folder
                        .as_ref()
                        .is_some_and(|parent_session_folder| {
                            self.services
                                .fs_client()
                                .is_dir(parent_session_folder.clone())
                        });

                at_mention_lookup_root(
                    project_working_dir,
                    parent_session_folder,
                    has_parent_session_folder,
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
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_selection,
        )
        .await;
    }

    /// Applies one completed branch-publish action to the session chat.
    pub(super) async fn apply_branch_publish_action_update(
        &mut self,
        branch_publish_action_update: BranchPublishActionUpdate,
    ) {
        let BranchPublishActionUpdate { result, session_id } = branch_publish_action_update;

        match result {
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name,
                review_request_creation,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);

                let result_message = TransientMessageBody::Markdown(format!(
                    "**{}**\n\n{}",
                    Self::branch_publish_success_title(PublishBranchAction::Push),
                    Self::branch_publish_success_message(
                        &branch_name,
                        review_request_creation.as_ref(),
                    )
                ));
                self.sessions
                    .finish_branch_publish(&session_id, result_message);
            }
            Ok(BranchPublishTaskSuccess::PullRequestPublished {
                review_request,
                upstream_reference,
                ..
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);
                self.sessions
                    .apply_review_request(&session_id, review_request.clone());

                let persistent_notice = Self::review_request_created_notice(&review_request);
                if self
                    .sessions
                    .finish_review_request_publish(&session_id, &persistent_notice)
                {
                    SessionTaskService::persist_workflow_notice(
                        self.services.db(),
                        &session_id,
                        &persistent_notice,
                    )
                    .await;
                }
            }
            Err(failure) => {
                let result_message = TransientMessageBody::Markdown(format!(
                    "**{}**\n\n{}",
                    failure.title, failure.message
                ));
                self.sessions
                    .finish_branch_publish(&session_id, result_message);
            }
        }
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
                    .record_externally_merged_session(&session_id, session_head_hash)
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

    /// Records one externally merged session as read-only `Merged` without
    /// starting local cleanup or stacked-child restacking.
    pub(super) async fn record_externally_merged_session(
        &self,
        session_id: &str,
        session_head_hash: Option<String>,
    ) -> Option<String> {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return None;
        };
        let mut warnings = Vec::new();

        if let Some(session_head_hash) = session_head_hash
            && let Err(error) = self
                .services
                .db()
                .sessions()
                .update_session_merged_commit_hash(session_id, Some(session_head_hash))
                .await
        {
            warnings.push(format!("Merged commit hash persistence failed: {error}"));
        }

        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        if !status_transition.apply(Status::Merged).await {
            warnings.push("Could not mark the merged session read-only".to_string());
        }

        (!warnings.is_empty()).then(|| warnings.join("\n"))
    }

    /// Finalizes read-only merged sessions targeting the branch updated by a
    /// successful user-triggered main sync.
    async fn finalize_merged_sessions_after_main_sync(
        &mut self,
        default_branch: &str,
    ) -> Vec<SessionId> {
        let mut deferred_session_ids = Vec::new();
        let session_ids = self
            .sessions
            .sessions()
            .iter()
            .filter(|session| {
                session
                    .review_request
                    .as_ref()
                    .is_some_and(|review_request| {
                        review_request.summary.target_branch == default_branch
                    })
                    && self
                        .sessions
                        .session_handles_or_err(&session.id)
                        .ok()
                        .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                        == Some(Status::Merged)
            })
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();

        for session_id in session_ids {
            let session_head_hash = match self
                .services
                .db()
                .sessions()
                .load_session_merged_commit_hash(&session_id)
                .await
            {
                Ok(session_head_hash) => session_head_hash,
                Err(error) => {
                    self.append_output_for_session(
                        &session_id,
                        &TranscriptNotice::ReviewRequestSyncWarning
                            .format(format!("Merged commit hash load failed: {error}")),
                    )
                    .await;
                    deferred_session_ids.push(session_id);

                    continue;
                }
            };

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
            if self
                .sessions
                .session_handles_or_err(&session_id)
                .ok()
                .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                == Some(Status::Merged)
            {
                deferred_session_ids.push(session_id);
            }
        }

        deferred_session_ids
    }

    /// Marks one externally merged session `Done` after manual target sync,
    /// persists child restack intent, and returns any finalization warning.
    ///
    /// The session is still moved to `Done` when cleanup fails because the
    /// merge already happened upstream, but the caller should surface the
    /// warning to the user.
    pub(super) async fn complete_externally_merged_session(
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

        let folder = session.folder.clone();
        let base_branch = session.base_branch.clone();
        let source_branch = crate::app::session::session_branch(session_id);
        let app_event_tx = self.services.event_sender();

        match crate::app::session::SessionManager::restack_child_sessions_after_parent_merge(
            self.services.db(),
            session_id,
            &base_branch,
            session_head_hash,
        )
        .await
        {
            Ok(child_session_ids) => {
                crate::app::session::SessionManager::emit_stacked_parent_merge_completed(
                    &app_event_tx,
                    child_session_ids,
                );
            }
            Err(error) => {
                return Some(format!("Stacked child restack intent failed: {error}"));
            }
        }

        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let status_applied = status_transition.apply(Status::Done).await;
        if !status_applied {
            warnings.push("Could not archive the merged session".to_string());

            return Some(warnings.join("\n"));
        }
        self.spawn_externally_merged_session_cleanup(session_id, folder, source_branch, handles);

        (!warnings.is_empty()).then(|| warnings.join("\n"))
    }

    /// Removes an externally merged session worktree without delaying terminal
    /// input or redraws, persisting any cleanup warning after the task
    /// finishes.
    fn spawn_externally_merged_session_cleanup(
        &self,
        session_id: &str,
        folder: PathBuf,
        source_branch: String,
        handles: &SessionHandles,
    ) {
        let app_event_tx = self.services.event_sender();
        let db = self.services.db().clone();
        let fs_client = self.services.fs_client();
        let git_client = self.services.git_client();
        let session_id = SessionId::from(session_id);
        let session_update_versions = self.services.session_update_versions();
        let transcript = Arc::clone(&handles.transcript);
        let cleanup_task = tokio::spawn(async move {
            if let Err(error) =
                crate::app::session::SessionManager::cleanup_merged_session_worktree(
                    folder,
                    fs_client,
                    git_client,
                    source_branch,
                    None,
                )
                .await
            {
                let warning = TranscriptNotice::ReviewRequestSyncWarning
                    .format(format!("Worktree cleanup failed: {error}"));
                SessionTaskService::append_workflow_notice(
                    &transcript,
                    &db,
                    &app_event_tx,
                    &session_update_versions,
                    session_id.as_str(),
                    &warning,
                )
                .await;
            }
        });
        self.services.track_cleanup_task(cleanup_task);
    }

    /// Transitions one externally closed review session to `Canceled`.
    async fn cancel_externally_closed_session(&self, session_id: &str) {
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };

        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let _ = status_transition.apply(Status::Canceled).await;
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

    /// Returns the inline loading label for one branch-publish action.
    pub(super) fn branch_publish_loading_label(
        publish_branch_action: PublishBranchAction,
    ) -> String {
        branch_publish_loading_label_text(publish_branch_action)
    }

    /// Returns the inline success title for a completed branch-publish action.
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

    /// Returns the durable transcript notice for one completed review-request
    /// publish.
    pub(super) fn review_request_created_notice(
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        review_request_created_notice_text(review_request)
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

    /// Builds the in-progress sync popup shown while conflicts are being
    /// resolved.
    pub(super) fn sync_main_conflict_resolution_popup_mode(
        conflicted_files: &[String],
        sync_popup_context: &SyncPopupContext,
    ) -> AppMode {
        AppMode::SyncBlockedPopup {
            project_name: Some(sync_popup_context.project_name.clone()),
            default_branch: Some(sync_popup_context.default_branch.clone()),
            is_loading: true,
            message: Self::sync_conflict_resolution_message(conflicted_files),
            title: "Resolving conflicts".to_string(),
        }
    }

    /// Returns loading-state copy for assisted sync conflict resolution.
    fn sync_conflict_resolution_message(conflicted_files: &[String]) -> String {
        let file_list = conflicted_files
            .iter()
            .map(|file| format!("- {file}"))
            .collect::<Vec<String>>()
            .join("\n");

        format!("Resolving conflicts during sync.\n\nConflicted files:\n{file_list}")
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

        let mut message = sync_message::format_sync_success_message(
            &pulled_summary,
            &pulled_titles,
            &pushed_summary,
            &pushed_titles,
            &conflict_summary,
        );
        if !sync_main_outcome.deferred_merged_session_ids.is_empty() {
            let session_ids = sync_main_outcome
                .deferred_merged_session_ids
                .iter()
                .map(|session_id| format!("- `{session_id}`"))
                .collect::<Vec<_>>()
                .join("\n");
            message.push_str(
                "\n\n## Merged sessions still waiting\nThese sessions could not be archived or \
                 restacked. Review their workflow warning, then retry the sync:\n",
            );
            message.push_str(&session_ids);
        }

        message
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
    use ag_forge::{
        ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
    };

    use super::*;

    #[tokio::test]
    async fn test_issue_detail_success_preserves_action_error() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let project_id = app.projects.active_project_id();
        let generation = app.assigned_issue_generation;
        app.mode = AppMode::IssueDetail {
            action_error: Some("Failed to start issue session: unavailable".to_string()),
            detail: None,
            error: None,
            issue: ag_forge::AssignedIssue {
                display_id: "#124".to_string(),
                repository: "agentty-xyz/agentty".to_string(),
                title: "Keep issue details reachable".to_string(),
                updated_at: None,
                web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
            },
            scroll_offset: 0,
        };

        // Act
        app.apply_issue_detail_update(IssueDetailUpdate {
            display_id: "#124".to_string(),
            generation,
            project_id,
            result: Ok(ag_forge::IssueDetail {
                assignees: Vec::new(),
                author: "octocat".to_string(),
                body: Some("Loaded after the action failed.".to_string()),
                created_at: None,
                display_id: "#124".to_string(),
                labels: Vec::new(),
                repository: "agentty-xyz/agentty".to_string(),
                state: "OPEN".to_string(),
                title: "Keep issue details reachable".to_string(),
                updated_at: None,
                web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
            }),
        });

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::IssueDetail {
                action_error: Some(ref action_error),
                detail: Some(_),
                error: None,
                ..
            } if action_error == "Failed to start issue session: unavailable"
        ));
    }

    #[tokio::test]
    async fn test_session_review_comment_result_updates_matching_open_page() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: None,
            diff: String::new(),
            is_loading_comments: true,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            result: Ok(ag_forge::ReviewCommentSnapshot::default()),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ReviewComments {
                comment_error: None,
                comment_snapshot: Some(_),
                is_loading_comments: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_session_review_comment_refresh_retargets_selected_thread() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let previous_snapshot = review_comment_snapshot([
            review_comment_thread("selected", false),
            review_comment_thread("other", false),
        ]);
        let updated_snapshot = review_comment_snapshot([
            review_comment_thread("selected", true),
            review_comment_thread("other", false),
        ]);
        app.mode = AppMode::ReviewComments {
            comment_actions: vec![
                ReviewCommentActionSelection {
                    action: ReviewCommentAction::Address,
                    thread_id: "selected".to_string(),
                },
                ReviewCommentActionSelection {
                    action: ReviewCommentAction::Deny,
                    thread_id: "other".to_string(),
                },
            ],
            comment_error: None,
            comment_snapshot: Some(previous_snapshot),
            diff: String::new(),
            is_loading_comments: true,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            result: Ok(updated_snapshot),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ReviewComments {
                ref comment_actions,
                comment_snapshot: Some(ref snapshot),
                selected_comment_index: 1,
                ..
            } if review_comment_selection::selected_thread_id(snapshot, 1) == Some("selected")
                && comment_actions == &[ReviewCommentActionSelection {
                    action: ReviewCommentAction::Deny,
                    thread_id: "other".to_string(),
                }]
        ));
    }

    #[tokio::test]
    async fn test_session_review_comment_result_ignores_stale_pages_and_surfaces_errors() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

        // Act
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            result: Ok(ag_forge::ReviewCommentSnapshot::default()),
            session_id: "closed-session".into(),
        })
        .await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: None,
            diff: String::new(),
            is_loading_comments: true,
            selected_comment_index: 0,
            session_id: "open-session".into(),
            scroll_offset: 0,
        };
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            result: Ok(ag_forge::ReviewCommentSnapshot::default()),
            session_id: "stale-session".into(),
        })
        .await;
        app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
            result: Err("authentication failed".to_string()),
            session_id: "open-session".into(),
        })
        .await;

        // Assert
        assert!(app.is_viewing_session("open-session"));
        assert!(!app.is_viewing_session("stale-session"));
        assert!(matches!(
            app.mode,
            AppMode::ReviewComments {
                comment_error: Some(ref error),
                comment_snapshot: None,
                is_loading_comments: false,
                ..
            } if error == "Failed to load review comments: authentication failed"
        ));
    }

    /// Builds a comment snapshot from inline thread fixtures.
    fn review_comment_snapshot<const THREAD_COUNT: usize>(
        threads: [ReviewCommentThread; THREAD_COUNT],
    ) -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: Vec::from(threads),
        }
    }

    /// Builds one current or resolved review-comment thread.
    fn review_comment_thread(id: &str, is_resolved: bool) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "reviewer".to_string(),
                body: "Review comment".to_string(),
            }],
            id: id.to_string(),
            is_outdated: Some(false),
            is_resolved,
            line: Some(1),
            path: "src/main.rs".to_string(),
            start_line: None,
        }
    }

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

    #[test]
    fn test_issue_detail_batch_retains_same_generation_results_in_arrival_order() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        let visible_issue_event = AppEvent::IssueDetailLoaded {
            display_id: "#124".to_string(),
            generation: 1,
            project_id: 42,
            result: Err("visible issue result".to_string()),
        };
        let previous_issue_event = AppEvent::IssueDetailLoaded {
            display_id: "#123".to_string(),
            generation: 1,
            project_id: 42,
            result: Err("previous issue result".to_string()),
        };

        // Act
        event_batch.collect_event(visible_issue_event);
        event_batch.collect_event(previous_issue_event);

        // Assert
        assert_eq!(event_batch.issue_details.len(), 2);
        assert_eq!(event_batch.issue_details[0].display_id, "#124");
        assert_eq!(event_batch.issue_details[1].display_id, "#123");
    }

    #[test]
    fn test_assigned_issues_batch_changes_observable_state() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::AssignedIssuesLoaded {
            generation: 1,
            project_id: 42,
            result: Ok(Vec::new()),
        });

        // Assert
        assert!(App::app_event_batch_changes_observable_state(&event_batch));
    }

    #[test]
    fn test_issue_detail_batch_changes_observable_state() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::IssueDetailLoaded {
            display_id: "#124".to_string(),
            generation: 1,
            project_id: 42,
            result: Err("issue detail failure".to_string()),
        });

        // Assert
        assert!(App::app_event_batch_changes_observable_state(&event_batch));
    }

    #[tokio::test]
    async fn test_diff_preview_events_map_all_worktree_results() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let outcomes = [
            Ok(ag_git::WorktreeFileContent::Text("# Preview".to_string())),
            Ok(ag_git::WorktreeFileContent::Missing),
            Ok(ag_git::WorktreeFileContent::Binary),
            Ok(ag_git::WorktreeFileContent::TooLarge),
            Err("read failed".to_string()),
        ];
        let resolve_diff_state = |mode: &AppMode| match mode {
            AppMode::Diff {
                preview,
                scroll_cache,
                ..
            } => Some((preview.clone(), scroll_cache.is_none())),
            _ => None,
        };

        // Act
        let mut resolved_previews = Vec::new();
        for (request_id, result) in (1_u64..).zip(outcomes) {
            app.mode = AppMode::Diff {
                diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
                file_explorer_selected_index: 0,
                preview: DiffPreview::Loading {
                    path: "README.md".to_string(),
                    request_id,
                },
                restore: None,
                scroll_cache: Some(crate::presentation::app_mode::DiffScrollCache {
                    content_area: crate::presentation::app_mode::ViewportRect {
                        height: 24,
                        width: 80,
                        x: 0,
                        y: 0,
                    },
                    file_explorer_selected_index: 0,
                    max_scroll_offset: 4,
                }),
                scroll_offset: 2,
                session_id: "session-id".into(),
            };
            app.apply_app_events(AppEvent::DiffPreviewLoaded {
                path: "README.md".to_string(),
                request_id,
                result,
                session_id: "session-id".into(),
            })
            .await;
            let (preview, scroll_cache_cleared) = resolve_diff_state(&app.mode)
                .expect("diff preview result should preserve diff mode");
            assert!(scroll_cache_cleared);
            resolved_previews.push(preview);
        }

        // Assert
        assert!(resolve_diff_state(&AppMode::List).is_none());
        assert_eq!(resolved_previews.len(), 5);
        assert!(matches!(
            &resolved_previews[0],
            DiffPreview::Ready { content, .. } if content == "# Preview"
        ));
        assert!(matches!(
            &resolved_previews[1],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::Deleted,
                ..
            }
        ));
        assert!(matches!(
            &resolved_previews[2],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::Binary,
                ..
            }
        ));
        assert!(matches!(
            &resolved_previews[3],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::TooLarge,
                ..
            }
        ));
        assert!(matches!(
            &resolved_previews[4],
            DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::LoadFailed(error),
                ..
            } if error == "read failed"
        ));
    }

    #[tokio::test]
    async fn test_diff_preview_event_ignores_stale_mode_session_path_and_request() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let loading = || DiffPreview::Loading {
            path: "README.md".to_string(),
            request_id: 4,
        };
        let event = |path: &str, request_id: u64, session_id: &str| AppEvent::DiffPreviewLoaded {
            path: path.to_string(),
            request_id,
            result: Ok(ag_git::WorktreeFileContent::Text("stale".to_string())),
            session_id: session_id.into(),
        };
        let diff_mode = |preview| AppMode::Diff {
            diff: "diff".to_string(),
            file_explorer_selected_index: 0,
            preview,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            session_id: "session-id".into(),
        };

        // Act
        app.mode = diff_mode(loading());
        app.apply_app_events(event("OTHER.md", 4, "session-id"))
            .await;
        let stale_path_ignored = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Loading { .. },
                ..
            }
        );
        app.mode = diff_mode(loading());
        app.apply_app_events(event("README.md", 5, "session-id"))
            .await;
        let stale_request_ignored = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Loading { .. },
                ..
            }
        );
        app.mode = diff_mode(loading());
        app.apply_app_events(event("README.md", 4, "other-session"))
            .await;
        let stale_session_ignored = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Loading { .. },
                ..
            }
        );
        app.mode = AppMode::List;
        app.apply_app_events(event("README.md", 4, "session-id"))
            .await;

        // Assert
        assert!(stale_path_ignored);
        assert!(stale_request_ignored);
        assert!(stale_session_ignored);
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_diff_preview_event_resolves_while_help_is_open() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
                file_explorer_selected_index: 0,
                preview: DiffPreview::Loading {
                    path: "README.md".to_string(),
                    request_id: 8,
                },
                restore: None,
                scroll_offset: 0,
                session_id: "session-id".into(),
            },
            scroll_offset: 0,
        };

        // Act
        app.apply_app_events(AppEvent::DiffPreviewLoaded {
            path: "README.md".to_string(),
            request_id: 8,
            result: Ok(ag_git::WorktreeFileContent::Text("# Ready".to_string())),
            session_id: "session-id".into(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    preview: DiffPreview::Ready { ref content, .. },
                    ..
                },
                ..
            } if content == "# Ready"
        ));
    }
}
