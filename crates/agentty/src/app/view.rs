//! Immutable application view projection consumed by frontends.

use std::collections::HashMap;
use std::path::Path;

use crate::app::session_state::SessionGitStatus;
use crate::app::{App, AssignedIssueState, RequestedReviewState, Tab, UpdateStatus, session};
use crate::domain::agent::{AgentCliInfo, ReasoningLevel};
use crate::domain::project::ProjectListItem;
use crate::domain::session::{DailyActivity, Session, SessionId};
use crate::domain::theme::ColorTheme;
use crate::infra::clock;
use crate::presentation::app_mode::{AppMode, HelpContext};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::settings::SettingsScreenSnapshot;

/// Focused-review display state for the visible session.
pub(crate) struct SessionReviewView<'a> {
    pub(crate) session_id: &'a str,
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
    pub(crate) default_reasoning_level: ReasoningLevel,
    pub(crate) frame_time: FrameTime,
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
    pub(crate) session_branch_names: &'a HashMap<SessionId, String>,
    pub(crate) session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    pub(crate) session_index_by_id: &'a HashMap<SessionId, usize>,
    pub(crate) session_progress_messages: &'a HashMap<SessionId, String>,
    pub(crate) session_review: Option<SessionReviewView<'a>>,
    pub(crate) session_selected_index: Option<usize>,
    pub(crate) session_update_versions: &'a HashMap<SessionId, u64>,
    pub(crate) session_worktree_availability: &'a HashMap<SessionId, bool>,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings_screen: Option<SettingsScreenSnapshot>,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) status_bar_fyi_rotation_index: u64,
    pub(crate) theme: ColorTheme,
    pub(crate) update_status: Option<&'a UpdateStatus>,
    pub(crate) working_dir: &'a Path,
}

impl App {
    /// Projects immutable application state for one frontend frame.
    pub(crate) fn view_snapshot(&self) -> AppViewSnapshot<'_> {
        let clock_client = self.services.clock();
        let system_time = clock_client.now_system_time();
        let wall_clock_unix_seconds = session::unix_timestamp_from_system_time(system_time);
        let local_utc_offset_seconds =
            clock_client.local_utc_offset_seconds(wall_clock_unix_seconds);
        let sessions = self.sessions.render_parts();
        let visible_session_id = visible_session_id(&self.mode);
        let rate_limit_reset_local_utc_offset_seconds = rate_limit_reset_local_utc_offset_seconds(
            |timestamp_seconds| clock_client.local_utc_offset_seconds(timestamp_seconds),
            visible_session_id,
            sessions.sessions,
            local_utc_offset_seconds,
        );
        let frame_time = FrameTime::new(
            wall_clock_unix_seconds,
            clock::unix_timestamp_millis(system_time),
            local_utc_offset_seconds,
        )
        .with_rate_limit_reset_local_utc_offset_seconds(rate_limit_reset_local_utc_offset_seconds);
        let status_bar_fyi_rotation_index =
            u64::try_from(wall_clock_unix_seconds.div_euclid(60)).unwrap_or_default();
        let session_review = visible_session_id.map(|session_id| {
            let (_, text) = self.review_view_state(session_id);

            SessionReviewView { session_id, text }
        });
        let project = self.projects.render_parts();
        let current_tab = self.tabs.current();
        let settings_screen = (current_tab == Tab::Settings)
            .then(|| self.settings_presentation.snapshot(&self.settings.view()));

        AppViewSnapshot {
            active_project_id: project.active_project_id,
            active_prompt_outputs: sessions.active_prompt_outputs,
            assigned_issue_selected_index: self.assigned_issue_selected_index(),
            assigned_issues: &self.assigned_issues,
            available_agent_clis: self.services.available_agent_clis(),
            current_tab,
            default_reasoning_level: self.settings.reasoning_level,
            frame_time,
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
            session_branch_names: sessions.session_branch_names,
            session_git_statuses: sessions.session_git_statuses,
            session_index_by_id: sessions.session_index_by_id,
            session_progress_messages: &self.session_progress_messages,
            session_review,
            session_selected_index: sessions.selected_index,
            session_update_versions: &self.last_seen_session_update_versions,
            session_worktree_availability: sessions.session_worktree_availability,
            sessions: sessions.sessions,
            settings_screen,
            stats_activity: sessions.stats_activity,
            status_bar_fyi_rotation_index,
            theme: self.settings.theme,
            update_status: self.update_status.as_ref(),
            working_dir: project.working_dir,
        }
    }
}

/// Resolves the local UTC offset active at the visible session's weekly quota
/// reset, falling back to the current frame offset when no reset is available.
fn rate_limit_reset_local_utc_offset_seconds(
    local_utc_offset_seconds: impl FnOnce(i64) -> i64,
    visible_session_id: Option<&str>,
    sessions: &[Session],
    fallback_offset_seconds: i64,
) -> i64 {
    visible_session_id
        .and_then(|session_id| {
            sessions
                .iter()
                .find(|session| session.id.as_str() == session_id)
        })
        .and_then(Session::codex_weekly_rate_limit_window)
        .and_then(|window| window.resets_at)
        .map_or_else(|| fallback_offset_seconds, local_utc_offset_seconds)
}

/// Returns the session visible behind the active presentation mode.
fn visible_session_id(mode: &AppMode) -> Option<&str> {
    match mode {
        AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::ReviewComments { session_id, .. }
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
        | AppMode::PreCommitHookWarning { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
    use crate::domain::session::{CodexRateLimits, RateLimitWindow};
    use crate::test_support::SessionFixtureBuilder;

    const DST_BOUNDARY_TIMESTAMP: i64 = 1_772_964_000;
    const DST_CURRENT_TIMESTAMP: i64 = 1_772_960_400;
    const DST_RESET_TIMESTAMP: i64 = 1_772_967_600;

    #[test]
    fn visible_session_id_includes_review_comments() {
        // Arrange
        let mode = AppMode::ReviewComments {
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
        let session_id = visible_session_id(&mode);

        // Assert
        assert_eq!(session_id, Some("session-id"));
    }

    #[test]
    fn rate_limit_reset_offset_uses_the_offset_at_the_reset_timestamp() {
        // Arrange
        let mut session = SessionFixtureBuilder::new().build();
        session.agent = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55);
        session.stats.rate_limits = Some(CodexRateLimits {
            primary: None,
            secondary: Some(RateLimitWindow {
                resets_at: Some(DST_RESET_TIMESTAMP),
                used_percent: 3,
                window_duration_mins: Some(10_080),
            }),
        });
        let session_id = session.id.clone();
        let sessions = [session];
        let offset_at = |timestamp_seconds| {
            if timestamp_seconds >= DST_BOUNDARY_TIMESTAMP {
                -7 * 3_600
            } else {
                -8 * 3_600
            }
        };
        let current_offset_seconds = offset_at(DST_CURRENT_TIMESTAMP);

        // Act
        let offset_seconds = rate_limit_reset_local_utc_offset_seconds(
            offset_at,
            Some(session_id.as_str()),
            &sessions,
            current_offset_seconds,
        );

        // Assert
        assert_eq!(current_offset_seconds, -8 * 3_600);
        assert_eq!(offset_seconds, -7 * 3_600);
    }

    #[tokio::test]
    async fn view_snapshot_projects_the_visible_quota_reset_offset() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let mut session = SessionFixtureBuilder::new().id("session-id").build();
        session.agent = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt55);
        session.stats.rate_limits = Some(CodexRateLimits {
            primary: None,
            secondary: Some(RateLimitWindow {
                resets_at: Some(DST_RESET_TIMESTAMP),
                used_percent: 3,
                window_duration_mins: Some(10_080),
            }),
        });
        let session_id = session.id.clone();
        app.sessions.push_session(session);
        app.mode = AppMode::View {
            scroll_offset: Some(0),
            session_id,
        };
        let expected_offset_seconds = app.local_utc_offset_seconds(DST_RESET_TIMESTAMP);

        // Act
        let frame_time = app.view_snapshot().frame_time;

        // Assert
        assert_eq!(
            frame_time.rate_limit_reset_local_utc_offset_seconds(),
            expected_offset_seconds
        );
    }

    #[tokio::test]
    async fn view_snapshot_builds_settings_screen_only_for_settings_tab() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act
        app.tabs.set(Tab::Sessions);
        let sessions_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();
        app.tabs.set(Tab::Settings);
        let settings_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();

        // Assert
        assert!(!sessions_tab_has_settings_screen);
        assert!(settings_tab_has_settings_screen);
    }
}
