//! App state definitions and workflow glue for the app core module.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use ag_agent::{AgentAvailabilityProbe, AppServerClient, RealAgentAvailabilityProbe};
#[cfg(test)]
use ag_forge as forge;
use ag_forge::{
    AssignedIssue, RealReviewRequestClient, RequestedReview, RequestedReviewAudience,
    ReviewCommentSnapshot, ReviewRequestClient,
};
use ag_git::{GitClient, GitError, RealGitClient};
#[cfg(test)]
use app::branch_publish::detected_forge_kind_from_git_push_error;
use app::branch_publish::{
    BranchPublishTaskContext, BranchPublishTaskSession, run_branch_publish_action,
};
#[cfg(test)]
use app::branch_publish::{BranchPublishTaskFailure, branch_push_failure, push_session_branch};
use app::merge_queue::{MergeQueue, MergeQueueProgress};
use app::project::ProjectManager;
use app::review::{
    ReviewCacheEntry, mark_session_agent_review, review_failure_message, review_loading_message,
    review_view_text, start_review_assist as spawn_review_assist,
};
use app::service::AppServices;
use app::session::SessionManager;
use app::setting::SettingsManager;
use app::sync::SyncMainRunner;
use app::tab::{Tab, TabManager};
use app::{sync, task};
use session::StatusTransition;
#[cfg(test)]
use session::{SyncMainOutcome, SyncSessionStartError, TurnAppliedState};
use tokio::sync::mpsc;
use tracing::warn;

use super::events::AppEvent;
#[cfg(test)]
use super::events::{AppEventBatch, ReviewRequestStatusUpdate};
use crate::app;
use crate::app::{AppError, AssignedIssueState, RequestedReviewState, session};
#[cfg(test)]
use crate::domain::agent::AgentCliInfo;
#[cfg(test)]
use crate::domain::agent::AgentSelection;
use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, QuestionProgress, default_option_index};
use crate::domain::session::{FollowUpTaskAction, PublishBranchAction, Session, SessionId, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
#[cfg(test)]
use crate::infra::db;
use crate::infra::fs::{FsClient, RealFsClient};
use crate::infra::project_discovery::{ProjectDiscoveryClient, RealProjectDiscoveryClient};
use crate::infra::tmux::{RealTmuxClient, TmuxClient};
use crate::presentation::app_mode::{AppMode, ChatFocus, ConfirmationViewMode};

/// Relative directory name used for session git worktrees within the
/// `agentty` home directory.
pub const AGENTTY_WT_DIR: &str = "wt";

/// Background auto-update progress state for the status bar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    /// Background `npm i -g agentty@latest` is running.
    InProgress {
        /// Version currently being installed.
        version: String,
    },
    /// Update installed successfully; restart to use the new version.
    Complete {
        /// Version that was installed.
        version: String,
    },
    /// Update failed; fall back to manual update hint.
    Failed {
        /// Version whose installation failed.
        version: String,
    },
}

/// Immutable context displayed in sync-main popup content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SyncPopupContext {
    pub(super) default_branch: String,
    pub(super) project_name: String,
}

/// Source-session context needed to create a seeded continuation draft.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TerminalContinuationDraft {
    /// Base branch copied from the terminal source session.
    base_branch: String,
    /// Persisted project identifier copied from the terminal source session.
    project_id: i64,
    /// Initial draft message that gives the new session prior context.
    prompt_seed: String,
}

/// Identity for one background requested-review comment snapshot load.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct RequestedReviewCommentFetchKey {
    /// Provider display id such as GitHub `#123` or GitLab `!123`.
    pub(super) display_id: String,
    /// Requested-review list generation visible when the comment load began.
    pub(super) generation: u64,
    /// Project id that owns the requested-review row.
    pub(super) project_id: i64,
    /// Browser-openable review-request URL used to disambiguate rows.
    pub(super) web_url: String,
}

/// Background sync task result carrying the normalized summary for
/// persistence alongside the UI outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncReviewRequestTaskResult {
    pub(crate) outcome: session::SyncReviewRequestOutcome,
    /// Normalized summary to persist when a review request was found or
    /// refreshed.
    pub(crate) summary: Option<crate::domain::session::ReviewRequestSummary>,
}

/// External clients used to compose [`App`] startup dependencies.
pub(crate) struct AppClients {
    pub(super) agent_availability_probe: Arc<dyn AgentAvailabilityProbe>,
    /// Whether startup should spawn background CLI version detection.
    pub(super) agent_cli_version_task_enabled: bool,
    pub(super) app_server_client_override: Option<Arc<dyn AppServerClient>>,
    pub(super) fs_client: Arc<dyn FsClient>,
    pub(super) git_client: Arc<dyn GitClient>,
    pub(super) project_discovery_client: Arc<dyn ProjectDiscoveryClient>,
    pub(super) review_request_client: Arc<dyn ReviewRequestClient>,
    pub(super) sync_main_runner: Option<Arc<dyn SyncMainRunner>>,
    pub(super) tmux_client: Arc<dyn TmuxClient>,
}

impl AppClients {
    /// Builds one client bundle with real implementations for each external
    /// boundary.
    pub(crate) fn new() -> Self {
        Self {
            agent_availability_probe: Arc::new(RealAgentAvailabilityProbe),
            agent_cli_version_task_enabled: !cfg!(test),
            app_server_client_override: None,
            fs_client: Arc::new(RealFsClient),
            git_client: Arc::new(RealGitClient),
            project_discovery_client: Arc::new(RealProjectDiscoveryClient),
            review_request_client: Arc::new(RealReviewRequestClient::default()),
            sync_main_runner: None,
            tmux_client: Arc::new(RealTmuxClient),
        }
    }

    /// Replaces the startup agent-availability boundary while preserving the
    /// remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_agent_availability_probe(
        mut self,
        agent_availability_probe: Arc<dyn AgentAvailabilityProbe>,
    ) -> Self {
        self.agent_availability_probe = agent_availability_probe;

        self
    }

    /// Replaces the default provider-owned app-server clients with one shared
    /// override.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_app_server_client_override(
        mut self,
        app_server_client_override: Arc<dyn AppServerClient>,
    ) -> Self {
        self.app_server_client_override = Some(app_server_client_override);

        self
    }

    /// Replaces the startup project-discovery boundary while preserving the
    /// remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_project_discovery_client(
        mut self,
        project_discovery_client: Arc<dyn ProjectDiscoveryClient>,
    ) -> Self {
        self.project_discovery_client = project_discovery_client;

        self
    }

    /// Replaces the tmux boundary while preserving the remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_tmux_client(mut self, tmux_client: Arc<dyn TmuxClient>) -> Self {
        self.tmux_client = tmux_client;

        self
    }
}

// SessionState definition moved to session_state.rs

/// Stores application state and coordinates session/project workflows.
pub struct App {
    /// Tracks the currently active UI mode and its transient state.
    pub mode: AppMode,
    /// Tracks whether the foreground runtime should render a fresh frame.
    pub(crate) needs_redraw: bool,
    /// Stores persisted and in-memory application settings for the active
    /// project.
    pub settings: SettingsManager,
    /// Manages the selected top-level list tab.
    pub tabs: TabManager,
    /// Monotonic assigned-issue refresh generation for rejecting stale task
    /// results.
    pub(super) assigned_issue_generation: u64,
    /// Selected assigned-issue item index for the top-level `Issues` tab.
    pub(super) assigned_issue_selected_index: Option<usize>,
    /// Caches open GitHub issues assigned to the authenticated user.
    pub(crate) assigned_issues: AssignedIssueState,
    /// Saves partially answered clarification progress per session so
    /// already-submitted answers survive leaving question mode with `q` and
    /// reopening the session. Entries are consumed on restore and cleared
    /// when a new turn result replaces the session's question list.
    pub(crate) question_progress: HashMap<SessionId, QuestionProgress>,
    /// Records the session for which `reconcile_open_session_question_mode`
    /// already reloaded detail and still found no persisted questions, so the
    /// per-frame reconciliation does not reissue that database load every
    /// render cycle. Cleared once the open view leaves `Status::Question` or
    /// `AppMode::View`, or once the panel opens, so a later legitimate
    /// transition reloads again.
    pub(crate) question_reconcile_reload_attempted: Option<SessionId>,
    /// Caches generated focused review text per session so it survives mode
    /// switches, is hydrated after restart, and is ready when the user presses
    /// `f`.
    pub(crate) review_cache: HashMap<SessionId, ReviewCacheEntry>,
    /// Owns project selection state, project metadata, and git status
    /// snapshots.
    pub(crate) projects: ProjectManager,
    /// Shares application-wide services and external clients across workflows.
    pub(crate) services: AppServices,
    /// Owns session state, runtime handles, and session workflow coordination.
    pub(crate) sessions: SessionManager,
    /// Runs sync-to-main workflows behind an injectable boundary.
    pub(crate) sync_main_runner: Arc<dyn SyncMainRunner>,
    /// Owns the active-project sync orchestrator command and context
    /// channels.
    pub(crate) sync_handle: sync::SyncHandle,
    /// Monotonic requested-review refresh generation for rejecting stale task
    /// results.
    pub(super) requested_review_generation: u64,
    /// Tracks requested-review comment snapshot loads currently running in
    /// background tasks so reopening the same detail page does not duplicate
    /// forge API calls.
    pub(super) requested_review_comment_fetches: HashSet<RequestedReviewCommentFetchKey>,
    /// Selected requested-review item index for the active project's
    /// top-level `Review` tab, excluding non-selectable section headers.
    pub(crate) requested_review_selected_index: Option<usize>,
    /// Caches requested PR/MR reviews for the active project's `Review` tab.
    pub(crate) requested_reviews: RequestedReviewState,
    /// Receives app events emitted by background tasks and workflows.
    pub(super) event_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Stores the latest available stable `agentty` version when one is
    /// detected.
    pub(crate) latest_available_version: Option<String>,
    /// Serializes local merge requests so only one merge workflow runs at a
    /// time.
    pub(super) merge_queue: MergeQueue,
    /// Tracks per-session thinking text rendered while background work is
    /// active.
    pub(crate) session_progress_messages: HashMap<SessionId, String>,
    /// Interacts with tmux panes for session-specific terminal workflows.
    pub(super) tmux_client: Arc<dyn TmuxClient>,
    /// Tracks the last reduced observable-handle version for each session so
    /// stale `SessionUpdated` events do not trigger redundant redraws.
    pub(crate) last_seen_session_update_versions: HashMap<SessionId, u64>,
    /// Stores the current auto-update progress state when an update is running.
    pub(crate) update_status: Option<UpdateStatus>,
}

impl App {
    /// Returns an advisory message when the active project declares
    /// pre-commit validation without an executable Git hook.
    pub(crate) async fn pre_commit_hook_warning(&self) -> Option<String> {
        let git_client = self.services.git_client();
        let working_dir = self.projects.working_dir().to_path_buf();
        let repo_root = git_client.find_git_repo_root(working_dir).await?;

        match git_client.check_pre_commit_hook_ready(repo_root).await {
            Ok(()) => None,
            Err(error @ GitError::PreCommitHookMissing { .. }) => Some(error.to_string()),
            Err(error) => {
                warn!(
                    error = %error,
                    "failed to inspect pre-commit hook readiness before session creation"
                );

                None
            }
        }
    }

    /// Marks the app as needing one fresh terminal frame.
    pub(crate) fn mark_dirty(&mut self) {
        self.needs_redraw = true;
    }

    /// Returns whether the runtime should render a fresh frame immediately.
    pub(crate) fn needs_redraw(&self) -> bool {
        self.needs_redraw
    }

    /// Clears the pending redraw request after one frame is rendered.
    pub(crate) fn clear_redraw(&mut self) {
        self.needs_redraw = false;
    }

    /// Cycles the active list tab forward.
    pub fn next_tab(&mut self) {
        self.tabs.next();
        self.refresh_requested_reviews_if_inbox_tab(false);
        self.refresh_assigned_issues_if_issues_tab(false);
    }

    /// Cycles the active list tab backward.
    pub fn previous_tab(&mut self) {
        self.tabs.previous();
        self.refresh_requested_reviews_if_inbox_tab(false);
        self.refresh_assigned_issues_if_issues_tab(false);
    }

    /// Persists the active list tab for startup restoration.
    pub(crate) async fn persist_current_tab(&self) {
        let _ = self
            .services
            .db()
            .settings()
            .upsert_setting(SettingName::ActiveTab, self.tabs.current().as_str())
            .await;
    }

    /// Refreshes requested reviews when the `Inbox` tab is visible.
    pub fn refresh_requested_reviews_for_current_project(&mut self) {
        self.refresh_requested_reviews_if_inbox_tab(true);
    }

    /// Refreshes assigned GitHub issues when the `Issues` tab is visible.
    pub fn refresh_assigned_issues(&mut self) {
        self.refresh_assigned_issues_if_issues_tab(true);
    }

    /// Replaces the requested-review list for `project_id`, normalizes rows to
    /// the render order, and selects the first review when the list is
    /// non-empty.
    pub(crate) fn replace_requested_reviews(
        &mut self,
        project_id: i64,
        mut items: Vec<RequestedReview>,
    ) {
        items.sort_by_key(|item| Self::requested_review_audience_order(item.audience));
        self.requested_review_selected_index = (!items.is_empty()).then_some(0);
        self.requested_reviews = RequestedReviewState::Loaded { items, project_id };
    }

    /// Returns the currently selected requested-review item index, excluding
    /// section headers.
    pub(crate) fn requested_review_selected_index(&self) -> Option<usize> {
        self.requested_review_selected_index
    }

    /// Replaces the assigned-issue list and selects the first issue when
    /// available.
    pub(crate) fn replace_assigned_issues(&mut self, project_id: i64, items: Vec<AssignedIssue>) {
        self.assigned_issue_selected_index = (!items.is_empty()).then_some(0);
        self.assigned_issues = AssignedIssueState::Loaded { items, project_id };
    }

    /// Returns the currently selected assigned-issue index.
    pub(crate) fn assigned_issue_selected_index(&self) -> Option<usize> {
        self.assigned_issue_selected_index
    }

    /// Moves selection to the next assigned issue.
    pub(crate) fn next_assigned_issue(&mut self) {
        let Some(item_count) = self.assigned_issue_item_count() else {
            self.assigned_issue_selected_index = None;

            return;
        };

        self.assigned_issue_selected_index = Some(match self.assigned_issue_selected_index {
            Some(index) => (index + 1) % item_count,
            None => 0,
        });
    }

    /// Moves selection to the previous assigned issue.
    pub(crate) fn previous_assigned_issue(&mut self) {
        let Some(item_count) = self.assigned_issue_item_count() else {
            self.assigned_issue_selected_index = None;

            return;
        };

        self.assigned_issue_selected_index = Some(match self.assigned_issue_selected_index {
            Some(0) | None => item_count - 1,
            Some(index) => index - 1,
        });
    }

    /// Opens the selected assigned issue and loads its base details in the
    /// background without requesting comments.
    pub(crate) fn open_selected_assigned_issue(&mut self) {
        let Some(issue) = self.selected_assigned_issue().cloned() else {
            return;
        };

        let project_id = self.projects.active_project_id();
        task::TaskService::spawn_issue_detail_task(
            issue.display_id.clone(),
            self.assigned_issue_generation,
            project_id,
            self.projects.working_dir().to_path_buf(),
            self.services.event_sender(),
            self.services.git_client(),
            self.services.review_request_client(),
        );
        self.mode = AppMode::IssueDetail {
            detail: None,
            error: None,
            issue,
            scroll_offset: 0,
        };
    }

    /// Moves selection to the next requested review in the `Inbox` tab.
    pub(crate) fn next_requested_review(&mut self) {
        let Some(item_count) = self.requested_review_item_count() else {
            self.requested_review_selected_index = None;

            return;
        };

        self.requested_review_selected_index = Some(match self.requested_review_selected_index {
            Some(index) => (index + 1) % item_count,
            None => 0,
        });
    }

    /// Moves selection to the previous requested review in the `Inbox` tab.
    pub(crate) fn previous_requested_review(&mut self) {
        let Some(item_count) = self.requested_review_item_count() else {
            self.requested_review_selected_index = None;

            return;
        };

        self.requested_review_selected_index = Some(match self.requested_review_selected_index {
            Some(0) | None => item_count - 1,
            Some(index) => index - 1,
        });
    }

    /// Opens the selected requested-review detail page immediately and starts
    /// one background comment snapshot load when the review has no cached
    /// snapshot and no matching load is already in flight.
    pub(crate) fn open_selected_requested_review(&mut self) {
        let Some(review) = self.selected_requested_review().cloned() else {
            return;
        };

        let is_loading_comments = review.comment_snapshot.is_none();
        if is_loading_comments {
            let comment_fetch_key = RequestedReviewCommentFetchKey {
                display_id: review.display_id.clone(),
                generation: self.requested_review_generation,
                project_id: self.projects.active_project_id(),
                web_url: review.web_url.clone(),
            };
            if self
                .requested_review_comment_fetches
                .insert(comment_fetch_key.clone())
            {
                task::TaskService::spawn_requested_review_comment_snapshot_task(
                    task::RequestedReviewCommentSnapshotTask {
                        display_id: comment_fetch_key.display_id,
                        generation: comment_fetch_key.generation,
                        project_id: comment_fetch_key.project_id,
                        web_url: comment_fetch_key.web_url,
                        working_dir: self.projects.working_dir().to_path_buf(),
                    },
                    self.services.event_sender(),
                    self.services.git_client(),
                    self.services.review_request_client(),
                );
            }
        }

        self.mode = AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments,
            review,
            scroll_offset: 0,
        };
    }

    /// Opens the read-only comment page for a session's linked review request
    /// and starts a background forge comment load.
    pub(crate) fn open_session_review_comments(
        &mut self,
        session_id: &SessionId,
        diff: String,
    ) -> bool {
        let Some(session) = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)
        else {
            return false;
        };
        let Some(review_request) = session.review_request.as_ref() else {
            return false;
        };
        let display_id = review_request.summary.display_id.clone();
        let fallback_repo_url = SessionManager::review_request_repo_url(review_request);
        let working_dir = session.folder.clone();

        self.mode = AppMode::ReviewComments {
            comment_error: None,
            comment_snapshot: None,
            diff,
            is_loading_comments: true,
            selected_comment_index: 0,
            session_id: session_id.clone(),
            scroll_offset: 0,
        };
        task::TaskService::spawn_session_review_comment_snapshot_task(
            task::SessionReviewCommentSnapshotTask {
                display_id,
                fallback_repo_url,
                session_id: session_id.clone(),
                working_dir,
            },
            self.services.event_sender(),
            self.services.git_client(),
            self.services.review_request_client(),
        );

        true
    }

    /// Caches a lazily loaded requested-review comment snapshot back onto the
    /// loaded Inbox tab row so reopening the same detail page avoids another
    /// comment API call.
    pub(super) fn cache_requested_review_comment_snapshot(
        &mut self,
        display_id: &str,
        web_url: &str,
        comment_snapshot: &ReviewCommentSnapshot,
    ) {
        let RequestedReviewState::Loaded { items, .. } = &mut self.requested_reviews else {
            return;
        };

        let Some(item) = items
            .iter_mut()
            .find(|item| item.web_url == web_url && item.display_id == display_id)
        else {
            return;
        };

        item.comment_snapshot = Some(comment_snapshot.clone());
    }

    /// Returns the currently selected requested review, when the review list
    /// is loaded and the selection points at a row.
    pub(crate) fn selected_requested_review(&self) -> Option<&RequestedReview> {
        let RequestedReviewState::Loaded { items, .. } = &self.requested_reviews else {
            return None;
        };

        items.get(self.requested_review_selected_index?)
    }

    /// Moves selection to the next session in the list.
    pub fn next(&mut self) {
        self.sessions.next();
    }

    /// Moves selection to the previous session in the list.
    pub fn previous(&mut self) {
        self.sessions.previous();
    }

    /// Moves selection to the next project in the projects list.
    pub fn next_project(&mut self) {
        self.projects.next_project();
    }

    /// Moves selection to the previous project in the projects list.
    pub fn previous_project(&mut self) {
        self.projects.previous_project();
    }

    /// Selects the currently selected project in the projects list.
    ///
    /// # Errors
    /// Returns an error if there is no selected project or project switching
    /// fails.
    pub async fn switch_selected_project(&mut self) -> Result<(), AppError> {
        let selected_project_id = self
            .projects
            .selected_project_id()
            .ok_or_else(|| AppError::Workflow("No project selected".to_string()))?;

        self.switch_project(selected_project_id).await
    }

    /// Switches app context to one persisted project id.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or session refresh fails.
    pub async fn switch_project(&mut self, project_id: i64) -> Result<(), AppError> {
        let project = self
            .services
            .db()
            .projects()
            .get_project(project_id)
            .await?
            .map(Self::project_from_row)
            .ok_or_else(|| {
                AppError::Workflow(format!("Project with id `{project_id}` was not found"))
            })?;
        let git_branch = self
            .services
            .git_client()
            .detect_git_info(project.path.clone())
            .await;
        let git_upstream_ref = Self::load_git_upstream_ref(
            self.services.git_client().as_ref(),
            project.path.as_path(),
            git_branch.as_deref(),
        )
        .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .projects()
            .upsert_project(&project.path.to_string_lossy(), git_branch.clone())
            .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .settings()
            .set_active_project_id(project.id)
            .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .projects()
            .touch_project_last_opened(project.id)
            .await;

        self.projects.update_active_project_context(
            project.id,
            project.display_label(),
            git_branch,
            git_upstream_ref,
            project.path,
        );
        self.refresh_requested_reviews_if_inbox_tab(true);
        self.settings = SettingsManager::new(&self.services, project.id).await;
        let default_session_model = SessionManager::load_default_session_model(
            &self.services,
            Some(project.id),
            AgentKind::Antigravity.default_model(),
        )
        .await;
        self.sessions
            .set_default_session_model(default_session_model);
        for (session_id, persisted_review) in
            Self::load_focused_review_cache(self.services.db(), project.id).await
        {
            self.review_cache
                .entry(session_id)
                .or_insert(persisted_review);
        }
        self.reload_projects().await;
        self.refresh_sessions_now().await;

        Ok(())
    }

    /// Starts a requested-review list fetch when the visible tab needs one.
    fn refresh_requested_reviews_if_inbox_tab(&mut self, force: bool) {
        if self.tabs.current() != Tab::Review {
            return;
        }

        let project_id = self.projects.active_project_id();
        if !force && self.requested_reviews.is_current_for_project(project_id) {
            return;
        }

        self.requested_review_generation = self.requested_review_generation.saturating_add(1);
        let generation = self.requested_review_generation;
        self.requested_review_selected_index = None;
        self.clear_requested_review_comment_fetches_for_project(project_id);
        self.requested_reviews = RequestedReviewState::Loading {
            generation,
            project_id,
        };
        task::TaskService::spawn_requested_reviews_task(
            generation,
            project_id,
            self.projects.working_dir().to_path_buf(),
            self.services.event_sender(),
            self.services.git_client(),
            self.services.review_request_client(),
        );
        self.mark_dirty();
    }

    /// Starts an assigned-issue fetch when the visible tab needs one.
    pub(super) fn refresh_assigned_issues_if_issues_tab(&mut self, force: bool) {
        if self.tabs.current() != Tab::Issues {
            return;
        }

        let project_id = self.projects.active_project_id();
        if !force && self.assigned_issues.is_current_for_project(project_id) {
            return;
        }

        self.assigned_issue_generation = self.assigned_issue_generation.saturating_add(1);
        let generation = self.assigned_issue_generation;
        self.assigned_issue_selected_index = None;
        self.assigned_issues = AssignedIssueState::Loading {
            generation,
            project_id,
        };
        task::TaskService::spawn_assigned_issues_task(
            generation,
            project_id,
            self.projects.working_dir().to_path_buf(),
            self.services.event_sender(),
            self.services.git_client(),
            self.services.review_request_client(),
        );
        self.mark_dirty();
    }

    /// Returns the loaded assigned-issue row count when a row can be selected.
    fn assigned_issue_item_count(&self) -> Option<usize> {
        let AssignedIssueState::Loaded { items, .. } = &self.assigned_issues else {
            return None;
        };

        (!items.is_empty()).then_some(items.len())
    }

    /// Returns the selected assigned-issue row when the list is loaded.
    fn selected_assigned_issue(&self) -> Option<&AssignedIssue> {
        let AssignedIssueState::Loaded { items, .. } = &self.assigned_issues else {
            return None;
        };

        self.assigned_issue_selected_index
            .and_then(|index| items.get(index))
    }

    /// Invalidates pending requested-review comment loads for `project_id`
    /// because a list refresh supersedes snapshots fetched from the old list
    /// generation.
    fn clear_requested_review_comment_fetches_for_project(&mut self, project_id: i64) {
        self.requested_review_comment_fetches
            .retain(|fetch_key| fetch_key.project_id != project_id);
    }

    /// Returns the loaded requested-review row count when at least one review
    /// can be selected.
    fn requested_review_item_count(&self) -> Option<usize> {
        let RequestedReviewState::Loaded { items, .. } = &self.requested_reviews else {
            return None;
        };

        (!items.is_empty()).then_some(items.len())
    }

    /// Returns the display-order rank for one requested-review audience.
    fn requested_review_audience_order(audience: RequestedReviewAudience) -> u8 {
        match audience {
            RequestedReviewAudience::Personal => 0,
            RequestedReviewAudience::Group => 1,
        }
    }

    /// Creates a blank session and schedules list refresh through events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_session(&mut self) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_session(&self.projects, &self.services)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Creates a blank draft session and schedules list refresh through
    /// events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_draft_session(&mut self) -> Result<String, AppError> {
        let base_branch = self
            .projects
            .git_branch()
            .ok_or_else(|| {
                AppError::Workflow("Git branch is required to create a session".to_string())
            })?
            .to_string();

        self.create_finalized_draft_session_for_project(
            self.projects.active_project_id(),
            &base_branch,
        )
        .await
    }

    /// Creates a draft session stacked on the selected parent session.
    ///
    /// # Errors
    /// Returns an error if the parent is not eligible for stacking or the
    /// stacked draft row cannot be persisted.
    pub async fn create_stacked_draft_session(
        &mut self,
        parent_session_id: &str,
    ) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_stacked_draft_session(&self.services, parent_session_id)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Forks one root review-ready session and opens the forked session
    /// view.
    ///
    /// The new session starts on a fresh worktree branch that points at the
    /// source session branch tip, with persisted transcript history copied at
    /// fork time and provider-native conversation identifiers reset.
    ///
    /// # Errors
    /// Returns an error when the source session is missing, not root
    /// review-ready, lacks project metadata, or the forked session cannot be
    /// created.
    pub async fn fork_session(&mut self, source_session_id: &str) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .fork_session(&self.services, source_session_id)
            .await?;
        self.finish_session_creation(&session_id).await;
        self.sessions
            .load_session_detail_into_state(self.services.db(), &session_id)
            .await;
        self.open_session(&session_id);

        Ok(session_id)
    }

    /// Creates one fresh draft session, stages the continuation context as
    /// its first draft message, and opens an empty composer for follow-up
    /// notes.
    ///
    /// # Errors
    /// Returns an error if the source session is missing, is not terminal, has
    /// neither a merged commit hash nor persisted continuation context, or if
    /// the new draft session cannot be created.
    pub async fn continue_terminal_session(
        &mut self,
        source_session_id: &str,
    ) -> Result<String, AppError> {
        let continuation_draft = self
            .terminal_session_continuation_draft(source_session_id)
            .await?;
        let session_id = self
            .create_finalized_draft_session_for_project(
                continuation_draft.project_id,
                &continuation_draft.base_branch,
            )
            .await?;
        self.stage_draft_message(&session_id, continuation_draft.prompt_seed)
            .await?;

        self.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: crate::presentation::prompt::PromptAttachmentState::default(),
            focus: ChatFocus::Input,
            history_state: crate::presentation::prompt::PromptHistoryState::new(Vec::new()),
            slash_state: self.prompt_slash_state(),
            session_id: SessionId::from(session_id.as_str()),
            input: InputState::default(),
            scroll_offset: None,
        };

        Ok(session_id)
    }

    /// Creates one draft session for a project and runs the shared app-level
    /// post-create refresh/selection flow before returning.
    ///
    /// # Errors
    /// Returns an error if draft persistence or app refresh fails.
    async fn create_finalized_draft_session_for_project(
        &mut self,
        project_id: i64,
        base_branch: &str,
    ) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_draft_session_for_project(&self.services, project_id, base_branch)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Applies the shared post-create refresh and selection flow for a new
    /// session.
    async fn finish_session_creation(&mut self, session_id: &str) {
        self.process_pending_app_events().await;
        self.reload_projects().await;

        let index = self
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.sessions.select_session_index(Some(index));
    }

    /// Returns the persisted continuation draft context for one terminal
    /// session.
    ///
    /// # Errors
    /// Returns an error if the source session is missing, not terminal, or has
    /// neither a stored merged commit hash nor usable persisted continuation
    /// context.
    async fn terminal_session_continuation_draft(
        &self,
        source_session_id: &str,
    ) -> Result<TerminalContinuationDraft, AppError> {
        let source_session = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == source_session_id)
            .ok_or_else(|| AppError::Workflow("Session not found".to_string()))?;
        if !source_session.allows_terminal_continuation() {
            return Err(AppError::Workflow(
                "Only `Done` sessions can be continued".to_string(),
            ));
        }

        let project_id = self
            .services
            .db()
            .sessions()
            .load_session_project_id(source_session_id)
            .await?
            .ok_or_else(|| {
                AppError::Workflow(
                    "Source session has no project association. Restart Agentty from this project \
                     to backfill legacy sessions, then continue the session again."
                        .to_string(),
                )
            })?;
        let prompt_seed = if let Some(merged_commit_hash) = self
            .services
            .db()
            .sessions()
            .load_session_merged_commit_hash(source_session_id)
            .await?
        {
            Self::merged_commit_continuation_prompt(source_session, &merged_commit_hash)
        } else {
            source_session.continuation_prompt_seed().ok_or_else(|| {
                AppError::Workflow(
                    "Terminal continuation requires a merged commit hash or persisted context"
                        .to_string(),
                )
            })?
        };

        Ok(TerminalContinuationDraft {
            base_branch: source_session.base_branch.clone(),
            project_id,
            prompt_seed,
        })
    }

    /// Builds the initial continuation draft message that asks the agent to use
    /// one merged session commit as context.
    fn merged_commit_continuation_prompt(
        _source_session: &Session,
        merged_commit_hash: &str,
    ) -> String {
        format!("Use {merged_commit_hash} commit as an initial context for this session")
    }

    /// Submits the initial prompt for a newly created session.
    ///
    /// Starting a new turn clears cached and persisted focused-review output
    /// for that session so review text does not bleed into the next prompt
    /// cycle.
    ///
    /// # Errors
    /// Returns an error if the session is missing or task enqueue fails.
    pub async fn start_session(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), AppError> {
        self.clear_review_output(session_id);
        self.services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None)
            .await?;

        Ok(self
            .sessions
            .start_session(&self.services, session_id, prompt)
            .await?)
    }

    /// Persists one staged draft message for a `Draft` session without
    /// launching the agent.
    ///
    /// # Errors
    /// Returns an error if the session cannot accept more staged drafts.
    pub async fn stage_draft_message(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), AppError> {
        Ok(self
            .sessions
            .stage_draft_message(&self.services, session_id, prompt)
            .await?)
    }

    /// Starts a `Draft` session from its persisted staged draft bundle.
    ///
    /// Stacked drafts only launch when their parent branch is review-ready and
    /// the stack has no other active branch work.
    ///
    /// # Errors
    /// Returns an error if the session is missing, has no staged drafts, or
    /// stack consistency or launch enqueueing fails.
    pub async fn start_staged_session(&mut self, session_id: &str) -> Result<(), AppError> {
        self.clear_review_output(session_id);
        self.services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None)
            .await?;

        Ok(self
            .sessions
            .start_staged_session(&self.services, session_id)
            .await?)
    }

    /// Submits a follow-up prompt for an existing session.
    ///
    /// Starting a new turn clears cached and persisted focused-review output
    /// for that session so review text does not persist past prompt
    /// submission. Returns `true` when the reply command was enqueued on the
    /// session worker.
    pub async fn reply(&mut self, session_id: &str, prompt: impl Into<TurnPrompt>) -> bool {
        self.clear_review_output(session_id);
        let _ = self
            .services
            .db()
            .sessions()
            .update_session_focused_review(session_id, None, None)
            .await;

        self.sessions
            .reply(&self.services, session_id, prompt)
            .await
    }

    /// Queues one chat prompt for an existing `InProgress` or `Rebasing`
    /// session so the session worker dispatches it as the next turn once the
    /// active operation finishes.
    ///
    /// # Errors
    /// Returns the underlying [`crate::app::session::SessionError`] when
    /// the session does not exist or the payload is empty.
    pub fn enqueue_message(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), crate::app::session::SessionError> {
        self.sessions
            .enqueue_message(&self.services, session_id, prompt)
    }

    /// Returns the current wall-clock time used for render-time timers.
    pub(crate) fn wall_clock_unix_seconds(&self) -> i64 {
        session::unix_timestamp_from_system_time(self.sessions.state().now_system_time())
    }

    /// Returns the focused-review output state that should be shown when one
    /// session view is reopened.
    pub(crate) fn review_view_state(&self, session_id: &str) -> (Option<String>, Option<&str>) {
        let status_message = match self.review_cache.get(session_id) {
            Some(ReviewCacheEntry::Loading { .. }) => Some(review_loading_message(
                self.settings.default_review_selection.model(),
            )),
            Some(ReviewCacheEntry::Failed { error, .. }) => Some(review_failure_message(error)),
            Some(ReviewCacheEntry::Ready { .. } | ReviewCacheEntry::Suppressed) | None => None,
        };

        (
            status_message,
            review_view_text(&self.review_cache, session_id),
        )
    }

    /// Returns whether focused-review generation is already running for one
    /// session without inspecting user-visible status copy.
    pub(crate) fn review_is_loading(&self, session_id: &str) -> bool {
        matches!(
            self.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { .. })
        )
    }

    /// Restores one session's cache-backed focused review into its visible
    /// output slot when the session is reopened.
    pub(crate) fn restore_review_output(&mut self, session_id: &str) {
        app::review::hydrate_review_transient(
            &self.review_cache,
            self.sessions.state_mut(),
            session_id,
            self.settings.default_review_selection.model(),
        );
    }

    /// Clears cached focused-review state and retracts its display slot.
    pub(crate) fn clear_review_output(&mut self, session_id: &str) {
        self.review_cache.remove(session_id);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session
                .transient_messages
                .retract(TransientMessageSlot::Review);
        }
    }

    /// Stores completed focused-review text and posts it to the stable review
    /// display slot.
    pub(crate) fn set_review_ready_output(
        &mut self,
        session_id: &str,
        diff_hash: u64,
        text: String,
    ) {
        self.review_cache.insert(
            SessionId::from(session_id),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: text.clone(),
            },
        );
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::AfterCompletedTurn,
                body: TransientMessageBody::Markdown(text),
                lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                slot: TransientMessageSlot::Review,
                turn_position: session.latest_user_prompt_position(),
            });
        }
    }

    /// Suppresses automatic focused review and removes any previous review
    /// display slot for the stopped turn.
    pub(crate) fn suppress_review_output(&mut self, session_id: &str) {
        self.review_cache
            .insert(SessionId::from(session_id), ReviewCacheEntry::Suppressed);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session
                .transient_messages
                .retract(TransientMessageSlot::Review);
        }
    }

    /// Persists and applies an agent/model selection for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_model(
        &mut self,
        session_id: &str,
        session_agent: crate::domain::agent::AgentSelection,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_model(&self.services, session_id, session_agent)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a reasoning level for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_reasoning_level(
        &mut self,
        session_id: &str,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_reasoning_level(&self.services, session_id, reasoning_level)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.selected_session()
    }

    /// Returns the session snapshot for one list index, if it still exists.
    pub fn session_at(&self, session_index: usize) -> Option<&Session> {
        self.sessions.session_at(session_index)
    }

    /// Returns session id by list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<SessionId> {
        self.sessions.session_id_for_index(session_index)
    }

    /// Resolves a session id to current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.sessions.session_index_for_id(session_id)
    }

    /// Returns compact live thinking text for a session, if available.
    pub fn session_progress_message(&self, session_id: &str) -> Option<&str> {
        self.session_progress_messages
            .get(session_id)
            .map(std::string::String::as_str)
    }

    /// Returns the latest reduced observable update version for a session.
    pub fn session_update_version(&self, session_id: &str) -> u64 {
        self.last_seen_session_update_versions
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    /// Returns the selected follow-up task action for one session, if that
    /// session currently exposes follow-up tasks.
    pub(crate) fn selected_follow_up_task_action(
        &self,
        session_id: &str,
    ) -> Option<FollowUpTaskAction> {
        self.sessions.selected_follow_up_task_action(session_id)
    }

    /// Returns whether one session has multiple follow-up tasks to cycle
    /// through in session view.
    pub(crate) fn has_multiple_follow_up_tasks(&self, session_id: &str) -> bool {
        self.sessions.has_multiple_follow_up_tasks(session_id)
    }

    /// Moves the selected follow-up task forward within one session.
    pub(crate) fn select_next_follow_up_task(&mut self, session_id: &str) {
        self.sessions.select_next_follow_up_task(session_id);
    }

    /// Moves the selected follow-up task backward within one session.
    pub(crate) fn select_previous_follow_up_task(&mut self, session_id: &str) {
        self.sessions.select_previous_follow_up_task(session_id);
    }

    /// Launches the selected follow-up task into a sibling session or opens
    /// the already launched sibling when one is linked.
    ///
    /// # Errors
    /// Returns an error if creating or starting the sibling session fails, or
    /// if persisting the launched-task link cannot be completed.
    pub(crate) async fn launch_or_open_selected_follow_up_task(
        &mut self,
        session_id: &str,
    ) -> Result<(), AppError> {
        let Some((position, task_text, launched_session_id)) =
            self.selected_follow_up_task_snapshot(session_id)
        else {
            return Ok(());
        };

        if let Some(launched_session_id) = launched_session_id {
            if self.open_session_if_present(&launched_session_id) {
                return Ok(());
            }

            self.set_follow_up_task_launched_session_id(session_id, position, None)
                .await?;
        }

        let sibling_session_id = self.create_session().await?;
        self.start_session(&sibling_session_id, TurnPrompt::from_text(task_text))
            .await?;
        self.set_follow_up_task_launched_session_id(
            session_id,
            position,
            Some(sibling_session_id.clone().into()),
        )
        .await?;
        self.open_session(&sibling_session_id);

        Ok(())
    }

    /// Deletes the selected session, clears transient review and `@`-mention
    /// state for that session, and schedules list refresh.
    pub async fn delete_selected_session(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            app::at_mention_task::clear_pending_load(&session_id);
            self.review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Deletes the selected session while deferring worktree filesystem cleanup
    /// to a background task, and clears transient review and `@`-mention
    /// state for that session.
    pub async fn delete_selected_session_deferred_cleanup(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session_deferred_cleanup(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            app::at_mention_task::clear_pending_load(&session_id);
            self.review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Cancels a session that is running, in review, or an unstarted draft.
    ///
    /// # Errors
    /// Returns an error if the session is not found or not cancelable.
    pub async fn cancel_session(&self, session_id: &str) -> Result<(), AppError> {
        Ok(self
            .sessions
            .cancel_session(&self.services, session_id)
            .await?)
    }

    /// Forces one merged-waiting session to `Done` when automatic ancestry
    /// detection cannot complete.
    ///
    /// # Errors
    /// Returns an error when the session is missing or is not waiting in the
    /// merged state.
    pub(crate) async fn force_complete_merged_session(
        &self,
        session_id: &str,
    ) -> Result<(), String> {
        let session = self
            .sessions
            .session_or_err(session_id)
            .map_err(|error| error.to_string())?;
        if session.status != Status::Merged {
            return Err("Session must be in Merged status to force completion".to_string());
        }

        if let Some(warning) = self.promote_externally_merged_session(session_id).await {
            self.append_output_for_session(
                session_id,
                &TranscriptNotice::ReviewRequestSyncWarning.format(warning),
            )
            .await;
        }
        let is_done = self
            .sessions
            .session_handles_or_err(session_id)
            .ok()
            .and_then(|handles| {
                handles
                    .status
                    .lock()
                    .ok()
                    .map(|status| *status == Status::Done)
            })
            .unwrap_or(false);
        if !is_done {
            return Err("Merged session could not be forced to Done".to_string());
        }

        Ok(())
    }

    /// Waits for tracked background cleanup tasks before process shutdown.
    pub(crate) async fn wait_for_background_cleanup_tasks(&self) {
        self.services.wait_for_cleanup_tasks().await;
    }

    /// Opens the selected session worktree in tmux and optionally runs the
    /// first configured launch configuration.
    pub async fn open_session_worktree_in_tmux(&self) {
        let selected_launch_configuration =
            self.configured_launch_configurations().into_iter().next();

        self.open_session_worktree_in_tmux_with_command(selected_launch_configuration.as_deref())
            .await;
    }

    /// Opens the selected session worktree in tmux and optionally runs one
    /// provided launch configuration.
    ///
    /// Sessions without a materialized worktree are treated as a no-op.
    pub(crate) async fn open_session_worktree_in_tmux_with_command(
        &self,
        launch_configuration: Option<&str>,
    ) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if !self.services.fs_client().is_dir(session.folder.clone()) {
            return;
        }

        let Some(window_id) = self
            .tmux_client
            .open_window_for_folder(session.folder.clone())
            .await
        else {
            return;
        };

        let Some(launch_configuration) = launch_configuration
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return;
        };

        self.tmux_client
            .run_command_in_window(window_id, launch_configuration.to_string())
            .await;
    }

    /// Starts the session-view branch-publish action flow for one session.
    pub(crate) fn start_publish_branch_action(
        &mut self,
        restore_view: ConfirmationViewMode,
        session_id: &str,
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<String>,
    ) {
        let Some(branch_publish_context) = self.branch_publish_task_context(session_id) else {
            self.mode = Self::view_info_popup_mode(
                "Branch push failed".to_string(),
                "Session is no longer available.".to_string(),
                false,
                String::new(),
                restore_view,
            );

            return;
        };

        let loading_label = Self::branch_publish_loading_label(publish_branch_action);
        let clock = self.services.clock();
        let db = self.services.db().clone();
        let event_sender = self.services.event_sender();
        let git_client = self.services.git_client();
        let review_request_client = self.services.review_request_client();
        let event_session_id = branch_publish_context.session.id.clone();

        self.sessions
            .start_branch_publish(session_id, loading_label);
        self.mode = restore_view.into_view_mode();

        tokio::spawn(async move {
            let result = run_branch_publish_action(
                publish_branch_action,
                branch_publish_context,
                db,
                clock,
                git_client,
                review_request_client,
                remote_branch_name,
            )
            .await;
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = event_sender.send(AppEvent::BranchPublishActionCompleted {
                result: Box::new(result),
                session_id: event_session_id,
            });
        });
    }

    /// Returns all configured launch configurations in user-defined order.
    #[must_use]
    pub(crate) fn configured_launch_configurations(&self) -> Vec<String> {
        self.settings.launch_configurations()
    }

    /// Appends output text to a session stream and persists it.
    pub(crate) async fn append_output_for_session(&self, session_id: &str, output: &str) {
        self.sessions
            .append_output_for_session(&self.services, session_id, output)
            .await;
    }

    /// Removes prompt attachment files that still belong to the active
    /// composer state.
    pub(crate) async fn cleanup_prompt_attachment_files(&self, prompt: &TurnPrompt) {
        self.sessions
            .cleanup_prompt_attachment_files(&self.services, prompt)
            .await;
    }

    /// Starts squash-merge workflow for a review-ready session.
    ///
    /// # Errors
    /// Returns an error if session is not mergeable, queueing fails, or
    /// immediate merge start fails while the queue is idle.
    pub async fn merge_session(&mut self, session_id: &str) -> Result<(), AppError> {
        if self.merge_queue.is_queued_or_active(session_id) {
            return Ok(());
        }

        self.validate_merge_request(session_id)?;
        if self.merge_queue.has_active() {
            self.mark_session_as_queued_for_merge(session_id).await?;
            self.merge_queue.enqueue(SessionId::from(session_id));

            return Ok(());
        }

        self.merge_queue.enqueue(SessionId::from(session_id));

        self.start_next_merge_from_queue(true).await
    }

    /// Starts or queues a session branch rebase onto its base branch.
    ///
    /// If the session is currently generating focused review output, starting
    /// sync cancels the pending review cache and persisted review entries so
    /// late review-assist completions cannot overwrite the rebased view state
    /// and startup cannot hydrate stale review text.
    ///
    /// # Errors
    /// Returns an error if focused-review persistence cannot be cleared before
    /// sync starts, or if session sync cannot start.
    pub async fn rebase_session(&mut self, session_id: &str) -> Result<(), AppError> {
        let should_clear_pending_review = matches!(
            self.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { .. })
        );
        if should_clear_pending_review {
            self.services
                .db()
                .sessions()
                .update_session_focused_review(session_id, None, None)
                .await?;
            self.clear_review_output(session_id);
        }

        self.sessions
            .rebase_session(&self.services, session_id)
            .await?;

        Ok(())
    }

    /// Starts selected-project branch sync in the background and immediately
    /// opens a loading popup with project and branch context.
    pub(crate) fn start_sync_main(&mut self) {
        let sync_popup_context = self.sync_popup_context();
        self.mode = AppMode::SyncBlockedPopup {
            project_name: Some(sync_popup_context.project_name.clone()),
            default_branch: Some(sync_popup_context.default_branch),
            is_loading: true,
            message: Self::sync_loading_message(),
            title: "Sync in progress".to_string(),
        };

        let app_event_tx = self.services.event_sender();
        let default_branch = self.projects.git_branch().map(str::to_string);
        let working_dir = self.projects.working_dir().to_path_buf();
        let git_client = self.services.git_client();
        let session_model = self.sessions.default_session_model();

        self.sync_main_runner.start_sync_main(
            app_event_tx,
            default_branch,
            git_client,
            session_model,
            working_dir,
        );
    }

    /// Starts review assist generation for one session using the
    /// current diff text and the configured default review model.
    ///
    /// The review assist prompt enforces inspection-only review constraints
    /// and recommends verification commands instead of running them.
    pub(crate) fn start_review_assist(
        &mut self,
        session_id: &str,
        session_folder: &Path,
        diff_hash: u64,
        review_diff: &str,
    ) {
        self.review_cache.insert(
            SessionId::from(session_id),
            ReviewCacheEntry::Loading { diff_hash },
        );
        let session_chat_history = self
            .sessions
            .session_handles()
            .get(session_id)
            .and_then(|handles| {
                handles
                    .transcript
                    .lock()
                    .ok()
                    .as_deref()
                    .and_then(SessionTranscript::conversation_replay_text)
            })
            .or_else(|| {
                self.sessions
                    .session_for_id(session_id)
                    .and_then(|session| session.transcript.as_ref())
                    .and_then(SessionTranscript::conversation_replay_text)
            });

        mark_session_agent_review(self.sessions.state_mut(), session_id);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Loading(review_loading_message(
                    self.settings.default_review_selection.model(),
                )),
                lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
                slot: TransientMessageSlot::Review,
                turn_position: session.latest_user_prompt_position(),
            });
        }

        spawn_review_assist(
            self.services.event_sender(),
            self.settings.default_review_selection,
            session_id,
            session_folder,
            diff_hash,
            review_diff,
            session_chat_history.as_deref(),
        );
    }

    /// Reloads sessions when metadata cache indicates changes.
    ///
    /// Returns `true` when the fallback poll refreshed render-visible session
    /// state.
    pub async fn refresh_sessions_if_needed(&mut self) -> bool {
        let refreshed = self
            .sessions
            .refresh_sessions_if_needed(&mut self.mode, &self.projects, &self.services)
            .await;
        if refreshed {
            app::review::prune_review_cache(&mut self.review_cache, self.sessions.state());
            app::review::hydrate_review_transients(
                &self.review_cache,
                self.sessions.state_mut(),
                self.settings.default_review_selection.model(),
            );
        }

        refreshed
    }

    /// Forces immediate session list reload.
    pub(crate) async fn refresh_sessions_now(&mut self) {
        self.sessions
            .refresh_sessions_now(&mut self.mode, &self.projects, &self.services)
            .await;
        app::review::prune_review_cache(&mut self.review_cache, self.sessions.state());
        app::review::hydrate_review_transients(
            &self.review_cache,
            self.sessions.state_mut(),
            self.settings.default_review_selection.model(),
        );
        self.restart_git_status_task();
    }

    /// Reloads project list snapshots from persistence.
    pub(super) async fn reload_projects(&mut self) {
        let project_items =
            Self::load_project_items(self.services.db(), self.services.fs_client().as_ref()).await;
        self.projects.replace_project_items(project_items);
    }

    /// Publishes the current project/session sync context and requests an
    /// immediate orchestrator refresh when the active project has a git
    /// branch.
    pub(super) fn restart_git_status_task(&mut self) {
        self.publish_sync_context_for_refresh();
        if self.projects.has_git_branch() {
            self.sync_handle.request_refresh();
        }
    }

    /// Publishes a fresh sync context after reducer-applied session changes
    /// that may affect polling targets.
    pub(super) fn publish_sync_context(&self) {
        self.sync_handle.publish_context(Self::sync_context_for(
            &self.projects,
            &self.services,
            &self.sessions,
        ));
    }

    /// Publishes a fresh sync context and forces a new generation so in-flight
    /// status completions computed before the requested refresh are ignored.
    pub(super) fn publish_sync_context_for_refresh(&self) {
        self.sync_handle
            .publish_refresh_context(Self::sync_context_for(
                &self.projects,
                &self.services,
                &self.sessions,
            ));
    }

    /// Builds the versioned sync context for the active project and session
    /// snapshot.
    pub(crate) fn sync_context_for(
        projects: &ProjectManager,
        services: &AppServices,
        sessions: &SessionManager,
    ) -> sync::SyncContext {
        sync::SyncContext {
            generation: 0,
            git_client: services.git_client(),
            merged_session_promotion_targets: Self::merged_session_promotion_targets(sessions),
            project_branch_name: projects.git_branch().map(str::to_string),
            review_request_client: services.review_request_client(),
            review_request_sync_targets: Self::review_request_sync_targets(sessions),
            session_git_status_targets: Self::session_git_status_targets(sessions),
            working_dir: projects.working_dir().to_path_buf(),
        }
    }

    /// Builds ancestry-check targets for sessions that merged upstream but
    /// still wait for their local review-request target branch to sync.
    pub(crate) fn merged_session_promotion_targets(
        sessions: &SessionManager,
    ) -> Vec<sync::MergedSessionPromotionTarget> {
        sessions
            .state()
            .sessions()
            .iter()
            .filter(|session| session.status == Status::Merged)
            .filter_map(|session| {
                let review_request = session.review_request.as_ref()?;
                let merge_commit_sha = review_request.summary.merge_commit_sha.clone()?;

                Some(sync::MergedSessionPromotionTarget {
                    merge_commit_sha,
                    session_id: session.id.clone(),
                    target_branch: review_request.summary.target_branch.clone(),
                })
            })
            .collect()
    }

    /// Builds git-status polling targets for active session branches in the
    /// current project.
    pub(crate) fn session_git_status_targets(
        sessions: &SessionManager,
    ) -> Vec<sync::SessionGitStatusTarget> {
        sessions
            .state()
            .sessions()
            .iter()
            .filter(|session| !matches!(session.status, Status::Canceled | Status::Done))
            .filter(|session| Self::session_has_git_status_target(sessions, session))
            .map(|session| sync::SessionGitStatusTarget {
                base_branch: session.base_branch.clone(),
                branch_name: sessions
                    .session_branch_name(&session.id)
                    .map_or_else(|| session::session_branch(&session.id), str::to_string),
                session_id: session.id.clone(),
            })
            .collect()
    }

    /// Returns whether a session has a materialized branch that can be polled
    /// for git-status comparisons.
    fn session_has_git_status_target(
        sessions: &SessionManager,
        session: &crate::domain::session::Session,
    ) -> bool {
        !session.is_draft_session()
            || sessions
                .session_worktree_availability()
                .get(&session.id)
                .copied()
                .unwrap_or(false)
    }

    /// Builds review-request polling targets for active session branches in
    /// the current project.
    pub(crate) fn review_request_sync_targets(
        sessions: &SessionManager,
    ) -> Vec<sync::ReviewRequestSyncTarget> {
        sessions
            .state()
            .sessions()
            .iter()
            .filter(|session| session.can_sync_review_request())
            .map(|session| sync::ReviewRequestSyncTarget {
                folder: session.folder.clone(),
                linked_review_request: session.review_request.clone(),
                published_upstream_ref: session.published_upstream_ref.clone(),
                session_id: session.id.clone(),
            })
            .collect()
    }

    /// Returns the currently selected follow-up task payload for one session.
    fn selected_follow_up_task_snapshot(
        &self,
        session_id: &str,
    ) -> Option<(usize, String, Option<SessionId>)> {
        let position = self.sessions.selected_follow_up_task_position(session_id)?;
        let session = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)?;
        let follow_up_task = session.follow_up_task(position)?;

        Some((
            follow_up_task.position,
            follow_up_task.text.clone(),
            follow_up_task.launched_session_id.clone(),
        ))
    }

    /// Persists one launched sibling-session link and mirrors it into the
    /// in-memory session snapshot.
    ///
    /// # Errors
    /// Returns an error if the launched-session link cannot be persisted.
    async fn set_follow_up_task_launched_session_id(
        &mut self,
        session_id: &str,
        position: usize,
        launched_session_id: Option<SessionId>,
    ) -> Result<(), AppError> {
        self.services
            .db()
            .sessions()
            .update_session_follow_up_task_launched_session_id(
                session_id,
                position,
                launched_session_id.as_ref().map(ToString::to_string),
            )
            .await?;
        self.sessions.set_follow_up_task_launched_session_id(
            session_id,
            position,
            launched_session_id,
        );

        Ok(())
    }

    /// Opens one linked sibling session when it still exists in memory.
    ///
    /// Returns `true` when the target session was found and opened.
    fn open_session_if_present(&mut self, target_session_id: &str) -> bool {
        let Some(session_index) = self.session_index_for_id(target_session_id) else {
            return false;
        };
        self.open_session_by_index(target_session_id, session_index);

        true
    }

    /// Opens one session by id and preserves question mode for clarification
    /// sessions.
    fn open_session(&mut self, target_session_id: &str) {
        let Some(session_index) = self.session_index_for_id(target_session_id) else {
            return;
        };
        self.open_session_by_index(target_session_id, session_index);
    }

    /// Opens one session by list index and preserves question mode for
    /// clarification sessions.
    fn open_session_by_index(&mut self, target_session_id: &str, session_index: usize) {
        self.sessions.select_session_index(Some(session_index));
        self.restore_review_output(target_session_id);

        let Some(session) = self.sessions.session_at(session_index) else {
            return;
        };
        if session.status == Status::Question {
            let questions = session.questions.clone();
            self.enter_question_mode(target_session_id, questions);

            return;
        }

        self.mode = AppMode::View {
            session_id: SessionId::from(target_session_id),
            scroll_offset: None,
        };
    }

    /// Enters the interactive clarification panel when the actively viewed
    /// session has reached [`Status::Question`] but the UI is still on the
    /// plain session view.
    ///
    /// The live transition into `AppMode::Question` is a one-shot side effect
    /// of the `AgentResponseReceived` projection, gated on the session being
    /// viewed at the instant the turn completes. When that projection is
    /// missed — an overlay was open, the projection coalesced to empty, or the
    /// worker fell back to a reload-only recovery — the durable `Question`
    /// status still reaches the snapshot, stranding the question behind the
    /// session view until a manual reopen. This reconciliation mirrors the
    /// reopen path so the panel appears without one.
    ///
    /// A session view showing `Status::Question` is always an anomaly: every
    /// path that leaves the panel (answering, `Ctrl+C`/`Esc`, or `q`) moves the
    /// session off `Question` or out of `AppMode::View`, so entering the panel
    /// here cannot fight a legitimate view state.
    pub(crate) async fn reconcile_open_session_question_mode(&mut self) {
        let AppMode::View { session_id, .. } = &self.mode else {
            self.question_reconcile_reload_attempted = None;

            return;
        };

        let session_id = session_id.clone();
        let is_pending_question = self
            .sessions
            .session_for_id(&session_id)
            .is_some_and(|session| session.status == Status::Question);
        if !is_pending_question {
            self.question_reconcile_reload_attempted = None;

            return;
        }

        let mut questions = self
            .sessions
            .session_for_id(&session_id)
            .map(|session| session.questions.clone())
            .unwrap_or_default();
        if questions.is_empty() {
            // The list snapshot only carries persisted questions for the
            // active session, so reload detail before giving up, mirroring the
            // reopen path in `open_session_by_index`. A `Question` status with
            // no persisted questions is malformed, so reload at most once per
            // stuck session: without this guard `run_cycle` would reissue the
            // async load on every render frame while the view stays stuck.
            if self.question_reconcile_reload_attempted.as_deref() == Some(session_id.as_str()) {
                return;
            }

            self.question_reconcile_reload_attempted = Some(session_id.clone());
            self.sessions
                .load_session_detail_into_state(self.services.db(), &session_id)
                .await;
            questions = self
                .sessions
                .session_for_id(&session_id)
                .map(|session| session.questions.clone())
                .unwrap_or_default();
        }
        if questions.is_empty() {
            return;
        }

        self.question_reconcile_reload_attempted = None;
        self.enter_question_mode(&session_id, questions);
    }

    /// Enters question mode for a clarification session.
    ///
    /// Consumes saved partial answers from a previous visit when they still
    /// match the session's question list, so leaving question mode with `q`
    /// does not lose already-submitted answers.
    pub(crate) fn enter_question_mode(&mut self, session_id: &str, questions: Vec<QuestionItem>) {
        let progress = self
            .question_progress
            .remove(session_id)
            .filter(|progress| progress.applies_to(&questions));
        let (current_index, input, responses, selected_option_index) = match progress {
            Some(progress) => (
                progress.current_index,
                progress.input,
                progress.responses,
                progress.selected_option_index,
            ),
            None => (
                0,
                InputState::default(),
                Vec::new(),
                default_option_index(&questions, 0),
            ),
        };

        self.mode = AppMode::Question {
            at_mention_state: None,
            current_index,
            focus: ChatFocus::Input,
            input,
            questions,
            responses,
            scroll_offset: None,
            selected_option_index,
            session_id: SessionId::from(session_id),
        };
    }

    /// Validates whether a session is currently eligible for merge queueing.
    ///
    /// Sessions are eligible while actively under review or already marked as
    /// `Queued` (for example, after app restart), provided no open review
    /// request already owns merge authority on the forge. A parent with idle
    /// materialized children can enter merge queueing because merge completion
    /// retargets those children; active stack work still blocks the request.
    ///
    /// # Errors
    /// Returns an error when the session does not exist or has an ineligible
    /// status, or when stack consistency blocks branch mutation.
    fn validate_merge_request(&self, session_id: &str) -> Result<(), AppError> {
        let session = self.sessions.session_or_err(session_id)?;
        if !(session.status.allows_review_actions() || session.status == Status::Queued) {
            return Err(AppError::Workflow(
                "Session must be in review or queued status".to_string(),
            ));
        }
        if session.has_open_review_request() {
            return Err(AppError::Workflow(
                session::OPEN_REVIEW_REQUEST_LOCAL_MERGE_ERROR.to_string(),
            ));
        }
        if !self.sessions.can_merge_session_branch_in_stack(session_id) {
            return Err(AppError::Workflow(
                "Merge can only run when no other stack session is active".to_string(),
            ));
        }

        Ok(())
    }

    /// Marks one session as waiting in the merge queue.
    ///
    /// # Errors
    /// Returns an error when status transition to `Queued` is invalid.
    async fn mark_session_as_queued_for_merge(&self, session_id: &str) -> Result<(), AppError> {
        let handles = self.sessions.session_handles_or_err(session_id)?;
        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let status_updated = status_transition.apply(Status::Queued).await;

        if !status_updated {
            return Err(AppError::Workflow(
                "Invalid status transition to Queued".to_string(),
            ));
        }

        Ok(())
    }

    /// Restores a queued session to `Review` if merge start fails.
    async fn restore_queued_session_to_review(&self, session_id: &str) {
        let session_status = self
            .sessions
            .session_or_err(session_id)
            .map(|session| session.status);
        if !matches!(session_status, Ok(Status::Queued)) {
            return;
        }

        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        // Best-effort: status transition failure is non-critical.
        let status_transition =
            StatusTransition::from_services(&self.services, handles, session_id);
        let _ = status_transition.apply(Status::Review).await;
    }

    /// Starts the next pending merge request when no merge is currently active.
    ///
    /// When `stop_on_failure` is `true`, returns the first start error.
    /// Otherwise, failed entries are skipped and the queue continues.
    ///
    /// # Errors
    /// Returns an error when starting a queued merge fails and
    /// `stop_on_failure` is enabled.
    pub(super) async fn start_next_merge_from_queue(
        &mut self,
        stop_on_failure: bool,
    ) -> Result<(), AppError> {
        if self.merge_queue.has_active() {
            return Ok(());
        }

        while let Some(next_session_id) = self.merge_queue.pop_next() {
            match self
                .sessions
                .merge_session(&next_session_id, &self.projects, &self.services)
                .await
            {
                Ok(()) => {
                    self.merge_queue.set_active(next_session_id);

                    return Ok(());
                }
                Err(error) => {
                    self.restore_queued_session_to_review(&next_session_id)
                        .await;

                    let merge_error = TranscriptNotice::MergeError.format(&error);
                    self.append_output_for_session(&next_session_id, &merge_error)
                        .await;

                    if stop_on_failure {
                        return Err(error.into());
                    }
                }
            }
        }

        Ok(())
    }

    /// Advances queue state after reducer-applied status changes.
    ///
    /// The queue advances when the active merge session transitions away from
    /// `Merging` or disappears from the refreshed session list.
    pub(super) async fn handle_merge_queue_progress(
        &mut self,
        session_ids: &HashSet<SessionId>,
        previous_session_states: &HashMap<SessionId, Status>,
    ) {
        let current_status = self
            .merge_queue
            .active_session_id()
            .and_then(|active_session_id| {
                self.sessions
                    .sessions()
                    .iter()
                    .find(|session| session.id == active_session_id)
                    .map(|session| session.status)
            });
        let progress = self.merge_queue.progress_from_status_updates(
            current_status,
            session_ids,
            previous_session_states,
        );
        if progress == MergeQueueProgress::StartNext {
            // Best-effort: merge queue progression failure is handled by status events.
            let _ = self.start_next_merge_from_queue(false).await;
        }
    }

    /// Drops thinking text for sessions that are no longer actively running.
    pub(super) fn retain_valid_session_progress_messages(&mut self) {
        self.session_progress_messages.retain(|session_id, _| {
            self.sessions
                .sessions()
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| matches!(session.status, Status::InProgress))
        });
    }

    /// Builds one branch-publish task snapshot with its shared operation lock.
    fn branch_publish_task_context(&self, session_id: &str) -> Option<BranchPublishTaskContext> {
        let (session, handles) = self.sessions.session_and_handles_or_err(session_id).ok()?;
        let mut branch_publish_session = BranchPublishTaskSession::from_session(session);
        branch_publish_session.base_branch = self.review_target_branch_for_session(session);

        Some(BranchPublishTaskContext {
            branch_operation_lock: Arc::clone(&handles.branch_operation_lock),
            session: branch_publish_session,
        })
    }

    /// Resolves the forge review-request target branch for one session.
    ///
    /// Root sessions target their stored base branch. Stacked children target
    /// their parent session branch while the parent link exists, preferring
    /// the parent's linked review-request source branch, then the parent's
    /// pushed upstream branch, then the child row's stored local parent branch.
    fn review_target_branch_for_session(&self, session: &Session) -> String {
        self.stacked_parent_review_target_branch(session)
            .unwrap_or_else(|| session.base_branch.clone())
    }

    /// Returns the best review target branch for one stacked child's parent.
    fn stacked_parent_review_target_branch(&self, session: &Session) -> Option<String> {
        let parent_session_id = session.parent_session_id.as_ref()?;
        let parent_session = self
            .sessions
            .sessions()
            .iter()
            .find(|candidate| candidate.id.as_str() == parent_session_id.as_str())?;

        parent_session
            .review_request
            .as_ref()
            .map(|review_request| review_request.summary.source_branch.clone())
            .or_else(|| {
                parent_session
                    .published_upstream_ref
                    .as_deref()
                    .map(session::remote_branch_name_from_upstream_ref)
            })
    }

    /// Returns popup context for the currently active project sync target.
    pub(super) fn sync_popup_context(&self) -> SyncPopupContext {
        let default_branch = self
            .projects
            .git_branch()
            .map_or_else(|| "not detected".to_string(), str::to_string);
        let project_name = self.projects.project_name().to_string();

        SyncPopupContext {
            default_branch,
            project_name,
        }
    }

    /// Returns loading-state popup copy for sync-main operation.
    pub(super) fn sync_loading_message() -> String {
        "Synchronizing with its upstream.".to_string()
    }
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;

#[cfg(test)]
mod fork_tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::session::SessionStats;
    use crate::domain::session_message::SessionMessageKind;
    use crate::infra::tmux::MockTmuxClient;

    /// Prompt text copied through fork snapshot tests.
    const FORK_SOURCE_PROMPT: &str = "Build the fork workflow";
    /// Assistant text copied through fork snapshot tests.
    const FORK_SOURCE_ANSWER: &str = "Fork workflow complete";

    /// Creates a real git-backed source session and marks it review-ready for
    /// fork tests.
    async fn create_review_source_session_for_fork_test(app: &mut App) -> String {
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create source session");
        let source_status = Status::Review.to_string();
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at(&source_session_id, &source_status, 0)
            .await
            .expect("failed to mark source as review");

        persist_fork_source_runtime_linkage(app, &source_session_id).await;
        persist_fork_source_transcript(app, &source_session_id).await;
        crate::test_support::set_session_status_for_test(app, &source_session_id, Status::Review);

        source_session_id
    }

    /// Persists source-only linkage that a fork must intentionally clear.
    async fn persist_fork_source_runtime_linkage(app: &App, source_session_id: &str) {
        app.services
            .db()
            .sessions()
            .update_session_provider_conversation_id(
                source_session_id,
                Some("provider-thread".to_string()),
            )
            .await
            .expect("failed to persist provider conversation id");
        app.services
            .db()
            .sessions()
            .update_session_instruction_conversation_id(
                source_session_id,
                Some("instruction-thread".to_string()),
            )
            .await
            .expect("failed to persist instruction conversation id");
        app.services
            .db()
            .sessions()
            .update_session_published_upstream_ref(
                source_session_id,
                Some("origin/wt/source".to_string()),
            )
            .await
            .expect("failed to persist upstream ref");
        app.services
            .db()
            .sessions()
            .update_session_stats(
                source_session_id,
                &SessionStats {
                    input_tokens: 13,
                    output_tokens: 21,
                    ..SessionStats::default()
                },
            )
            .await
            .expect("failed to persist source usage stats");
    }

    /// Persists source transcript rows that a fork must copy.
    async fn persist_fork_source_transcript(app: &App, source_session_id: &str) {
        app.services
            .db()
            .sessions()
            .append_session_message(
                source_session_id,
                SessionMessageKind::UserPrompt,
                FORK_SOURCE_PROMPT,
            )
            .await
            .expect("failed to append source user prompt");
        app.services
            .db()
            .sessions()
            .append_session_message(
                source_session_id,
                SessionMessageKind::AssistantAnswer,
                FORK_SOURCE_ANSWER,
            )
            .await
            .expect("failed to append source assistant answer");
    }

    /// Asserts that a fork is open and contains the copied transcript without
    /// source runtime linkage.
    async fn assert_forked_session_snapshot(app: &App, forked_session_id: &str) {
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                ..
            } if session_id.as_str() == forked_session_id
        ));
        assert!(matches!(
            app.selected_session(),
            Some(session) if session.id == forked_session_id
                && session.status == Status::Review
                && session.parent_session_id.is_none()
                && session.published_upstream_ref.is_none()
                && session.stats.input_tokens == 0
                && session.stats.output_tokens == 0
        ));

        let forked_messages = app
            .services
            .db()
            .sessions()
            .load_session_messages(forked_session_id)
            .await
            .expect("failed to load forked messages");
        assert_eq!(forked_messages.len(), 2);
        assert_eq!(forked_messages[0].kind, "user_prompt");
        assert_eq!(forked_messages[0].content, FORK_SOURCE_PROMPT);
        assert_eq!(forked_messages[1].kind, "assistant_answer");
        assert_eq!(forked_messages[1].content, FORK_SOURCE_ANSWER);
        assert_eq!(
            app.services
                .db()
                .sessions()
                .get_session_provider_conversation_id(forked_session_id)
                .await
                .expect("failed to load fork provider conversation id"),
            None
        );
        assert_eq!(
            app.services
                .db()
                .sessions()
                .get_session_instruction_conversation_id(forked_session_id)
                .await
                .expect("failed to load fork instruction conversation id"),
            None
        );
    }

    #[tokio::test]
    async fn test_fork_session_opens_review_session_with_copied_transcript_snapshot() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let source_session_id = create_review_source_session_for_fork_test(&mut app).await;

        // Act
        let forked_session_id = app
            .fork_session(&source_session_id)
            .await
            .expect("expected session fork to succeed");

        // Assert
        assert_ne!(forked_session_id, source_session_id);
        assert_forked_session_snapshot(&app, &forked_session_id).await;
    }

    #[tokio::test]
    async fn test_fork_session_rejects_non_review_source_session() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let source_session_id = app
            .create_session()
            .await
            .expect("failed to create source session");

        // Act
        let result = app.fork_session(&source_session_id).await;

        // Assert
        assert!(matches!(
            result,
            Err(AppError::Session(crate::app::SessionError::Workflow(message)))
                if message == "Only root review-ready sessions can be forked"
        ));
    }

    #[tokio::test]
    async fn test_fork_session_rejects_stacked_child_source_session() {
        // Arrange
        let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
            Arc::new(MockTmuxClient::new()),
        )
        .await;
        let child_session = crate::test_support::SessionFixtureBuilder::new()
            .id("child-source")
            .parent_session_id(Some(SessionId::from("parent-session")))
            .status(Status::Review)
            .build();
        app.sessions.push_session(child_session);

        // Act
        let result = app.fork_session("child-source").await;

        // Assert
        assert!(matches!(
            result,
            Err(AppError::Session(crate::app::SessionError::Workflow(message)))
                if message == "Only root review-ready sessions can be forked"
        ));
    }
}
