//! Immutable application view projection consumed by frontends.

use std::collections::HashMap;
use std::path::Path;

use crate::app::session_state::SessionGitStatus;
use crate::app::{
    App, AssignedIssueState, RequestedReviewState, SettingsManager, Tab, UpdateStatus,
    review_view_state,
};
use crate::domain::agent::AgentCliInfo;
use crate::domain::project::ProjectListItem;
use crate::domain::session::{DailyActivity, Session, SessionId};
use crate::infra::review_comment_cache::ReviewCommentCache;
use crate::presentation::app_mode::{AppMode, HelpContext};

/// Focused-review display state for the visible session.
pub(crate) struct SessionReviewView<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) status_message: Option<String>,
    pub(crate) text: Option<&'a str>,
}

/// Borrowed immutable application data required by one frontend frame.
pub(crate) struct AppViewSnapshot<'a> {
    pub(crate) active_project_id: i64,
    pub(crate) active_prompt_outputs: &'a HashMap<SessionId, String>,
    pub(crate) assigned_issue_selected_index: Option<usize>,
    pub(crate) assigned_issues: &'a AssignedIssueState,
    pub(crate) available_agent_clis: Vec<AgentCliInfo>,
    pub(crate) current_tab: Tab,
    pub(crate) git_branch: Option<&'a str>,
    pub(crate) git_status: Option<(u32, u32)>,
    pub(crate) git_upstream_ref: Option<&'a str>,
    pub(crate) latest_available_version: Option<&'a str>,
    pub(crate) mode: &'a AppMode,
    pub(crate) mru_project_order: &'a [usize],
    pub(crate) project_selected_index: Option<usize>,
    pub(crate) projects: &'a [ProjectListItem],
    pub(crate) requested_review_selected_index: Option<usize>,
    pub(crate) requested_reviews: &'a RequestedReviewState,
    pub(crate) review_comment_cache: ReviewCommentCache,
    pub(crate) session_branch_names: &'a HashMap<SessionId, String>,
    pub(crate) session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    pub(crate) session_index_by_id: &'a HashMap<SessionId, usize>,
    pub(crate) session_progress_messages: &'a HashMap<SessionId, String>,
    pub(crate) session_review: Option<SessionReviewView<'a>>,
    pub(crate) session_selected_index: Option<usize>,
    pub(crate) session_update_versions: &'a HashMap<SessionId, u64>,
    pub(crate) session_worktree_availability: &'a HashMap<SessionId, bool>,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) status_bar_fyi_rotation_index: u64,
    pub(crate) update_status: Option<&'a UpdateStatus>,
    pub(crate) wall_clock_unix_seconds: i64,
    pub(crate) working_dir: &'a Path,
}

impl App {
    /// Projects immutable application state for one frontend frame.
    pub(crate) fn view_snapshot(&self) -> AppViewSnapshot<'_> {
        let wall_clock_unix_seconds = self.wall_clock_unix_seconds();
        let status_bar_fyi_rotation_index =
            u64::try_from(wall_clock_unix_seconds.div_euclid(60)).unwrap_or_default();
        let visible_session_id = visible_review_session_id(&self.mode);
        let session_review = visible_session_id.map(|session_id| {
            let (status_message, text) = review_view_state(
                &self.review_cache,
                session_id,
                self.settings.default_review_selection.model(),
            );

            SessionReviewView {
                session_id,
                status_message,
                text,
            }
        });
        let project = self.projects.render_parts();
        let sessions = self.sessions.render_parts();

        AppViewSnapshot {
            active_project_id: project.active_project_id,
            active_prompt_outputs: sessions.active_prompt_outputs,
            assigned_issue_selected_index: self.assigned_issue_selected_index(),
            assigned_issues: &self.assigned_issues,
            available_agent_clis: self.services.available_agent_clis(),
            current_tab: self.tabs.current(),
            git_branch: project.git_branch,
            git_status: project.git_status,
            git_upstream_ref: project.git_upstream_ref,
            latest_available_version: self.latest_available_version.as_deref(),
            mode: &self.mode,
            mru_project_order: project.mru_project_order,
            project_selected_index: project.selected_index,
            projects: project.project_items,
            requested_review_selected_index: self.requested_review_selected_index(),
            requested_reviews: &self.requested_reviews,
            review_comment_cache: self.services.review_comment_cache(),
            session_branch_names: sessions.session_branch_names,
            session_git_statuses: sessions.session_git_statuses,
            session_index_by_id: sessions.session_index_by_id,
            session_progress_messages: &self.session_progress_messages,
            session_review,
            session_selected_index: sessions.selected_index,
            session_update_versions: &self.last_seen_session_update_versions,
            session_worktree_availability: sessions.session_worktree_availability,
            sessions: sessions.sessions,
            settings: &self.settings,
            stats_activity: sessions.stats_activity,
            status_bar_fyi_rotation_index,
            update_status: self.update_status.as_ref(),
            wall_clock_unix_seconds,
            working_dir: project.working_dir,
        }
    }
}

/// Returns the session visible behind the active presentation mode.
fn visible_review_session_id(mode: &AppMode) -> Option<&str> {
    match mode {
        AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::Help {
            context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
            ..
        } => Some(session_id),
        AppMode::Confirmation {
            restore_view: Some(restore_view),
            ..
        }
        | AppMode::LaunchConfigurationSelector { restore_view, .. }
        | AppMode::PublishBranchInput { restore_view, .. }
        | AppMode::ViewInfoPopup { restore_view, .. } => Some(&restore_view.session_id),
        AppMode::List
        | AppMode::IssueDetail { .. }
        | AppMode::ReviewDetail { .. }
        | AppMode::SessionCreation { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => None,
    }
}
