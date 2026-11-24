//! Immutable application view projection consumed by frontends.

use std::collections::HashMap;
use std::path::Path;

use crate::app::session_state::SessionGitStatus;
use crate::app::{App, ProjectSyncStatus, Tab, UpdateStatus, session};
use crate::domain::agent::{AgentCliInfo, AgentKind, AgentModel, ReasoningLevel};
use crate::domain::harness::HarnessAvailability;
use crate::domain::project::ProjectListItem;
use crate::domain::resource::SessionResources;
use crate::domain::session::{DailyActivity, Session, SessionId};
use crate::domain::theme::ColorTheme;
use crate::infra::clock::{self, Clock};
use crate::presentation::app_mode::{AppMode, HelpContext};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::presentation::platform::PlatformTabView;
use crate::presentation::remote_control::RemoteControlView;
use crate::presentation::setting::SettingsScreenSnapshot;

/// Focused-review display state for the visible session.
pub(crate) struct SessionReviewView<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) text: Option<&'a str>,
}

/// Borrowed immutable application data required by one frontend frame.
pub(crate) struct AppViewSnapshot<'a> {
    pub(crate) active_project_id: i64,
    pub(crate) active_prompt_outputs: &'a HashMap<SessionId, String>,
    /// Full archive total for the active project.
    pub(crate) archived_session_count: usize,
    pub(crate) available_agent_clis: Vec<AgentCliInfo>,
    /// Startup-validated agent CLIs that can run new sessions.
    pub(crate) available_agent_kinds: Vec<AgentKind>,
    pub(crate) current_tab: Tab,
    pub(crate) current_version_display_text: &'a str,
    pub(crate) default_reasoning_level: ReasoningLevel,
    pub(crate) frame_time: FrameTime,
    pub(crate) git_branch: Option<&'a str>,
    pub(crate) git_status: Option<(u32, u32)>,
    pub(crate) git_upstream_ref: Option<&'a str>,
    pub(crate) graph_chat: Option<&'a GraphChatSnapshot>,
    /// Whether the in-process harness can run new sessions, and on which
    /// model.
    pub(crate) harness_availability: HarnessAvailability,
    /// Whether the session list has an archive pagination action.
    pub(crate) has_more_archived_sessions: bool,
    /// Whether loaded archived rows extend beyond the first page.
    pub(crate) is_archive_expanded: bool,
    pub(crate) latest_available_version: Option<&'a str>,
    /// Listed harness models a local server runs, whose usage the live
    /// readout shows as `local`.
    pub(crate) local_harness_models: Vec<AgentModel>,
    pub(crate) mode: &'a AppMode,
    pub(crate) mru_project_order: &'a [usize],
    /// Platform sign-in and remote-control state, or `None` unless the
    /// launch offers the Platform tab.
    pub(crate) platform: Option<PlatformTabView<'a>>,
    /// Latest totals for the Agentty process and all of its descendants.
    pub(crate) process_resources: Option<SessionResources>,
    pub(crate) project_selected_index: Option<usize>,
    /// Explicit sync state of the active project only.
    pub(crate) project_sync_status: Option<&'a ProjectSyncStatus>,
    pub(crate) projects: &'a [ProjectListItem],
    /// Remote-control listener and paired-device state, or `None` unless
    /// `--experimental-remote` or `--remote-relay` offers the Remote tab.
    pub(crate) remote_control: Option<&'a RemoteControlView>,
    pub(crate) session_branch_names: &'a HashMap<SessionId, String>,
    pub(crate) session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    pub(crate) session_index_by_id: &'a HashMap<SessionId, usize>,
    pub(crate) session_live_usage:
        &'a HashMap<crate::domain::session_usage::LiveUsageKey, ag_contracts::TurnUsage>,
    pub(crate) session_progress_messages: &'a HashMap<SessionId, String>,
    pub(crate) session_review: Option<SessionReviewView<'a>>,
    pub(crate) session_selected_index: Option<usize>,
    pub(crate) session_update_versions: &'a HashMap<SessionId, u64>,
    /// Recorded usage of each session the live readout has shown.
    pub(crate) session_usage_totals:
        &'a HashMap<SessionId, crate::domain::session_usage::SessionUsage>,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings_screen: Option<SettingsScreenSnapshot>,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) theme: ColorTheme,
    pub(crate) update_status: Option<&'a UpdateStatus>,
    pub(crate) working_dir: &'a Path,
}

impl App {
    /// Projects immutable application state for one frontend frame.
    pub(crate) fn view_snapshot(&self) -> AppViewSnapshot<'_> {
        let visible_session_id = visible_review_session_id(&self.mode);
        let frame_time = read_frame_time(self.services.clock().as_ref());
        let session_review = visible_session_id.map(|session_id| {
            let (_, text) = self.review_view_state(session_id);

            SessionReviewView { session_id, text }
        });
        let project = self.projects.render_parts();
        let sessions = self.sessions.render_parts();
        let current_tab = self.tabs.current();
        let settings_screen = (current_tab == Tab::Settings)
            .then(|| self.settings_presentation.snapshot(&self.settings.view()));

        AppViewSnapshot {
            active_project_id: project.active_project_id,
            active_prompt_outputs: sessions.active_prompt_outputs,
            available_agent_clis: self.services.available_agent_clis(),
            available_agent_kinds: self.services.available_agent_kinds(),
            harness_availability: self.services.harness_availability(),
            current_tab,
            current_version_display_text: &self.current_version_display_text,
            default_reasoning_level: self.settings.default_smart_reasoning_level,
            frame_time,
            git_branch: project.git_branch,
            git_status: project.git_status,
            git_upstream_ref: project.git_upstream_ref,
            archived_session_count: self.sessions.archived_session_count(),
            has_more_archived_sessions: self.sessions.has_more_archived_sessions(),
            is_archive_expanded: self.sessions.is_archive_expanded(),
            latest_available_version: self.latest_available_version.as_deref(),
            local_harness_models: self.services.harness_models().local_models(),
            mode: &self.mode,
            mru_project_order: project.mru_project_order,
            platform: self.platform.is_enabled().then(|| PlatformTabView {
                platform: self.platform.view(),
                remote_control: self.remote_control.view(),
            }),
            process_resources: sessions.process_resources,
            project_selected_index: project.selected_index,
            project_sync_status: self.active_project_sync_status(),
            projects: project.project_items,
            remote_control: self
                .remote_control
                .is_enabled()
                .then(|| self.remote_control.view()),
            session_branch_names: sessions.session_branch_names,
            session_git_statuses: sessions.session_git_statuses,
            session_index_by_id: sessions.session_index_by_id,
            session_live_usage: &self.session_live_usage,
            session_progress_messages: &self.session_progress_messages,
            session_review,
            session_selected_index: sessions.selected_index,
            session_update_versions: &self.last_seen_session_update_versions,
            session_usage_totals: &self.session_usage_totals,
            sessions: sessions.sessions,
            settings_screen,
            stats_activity: sessions.stats_activity,
            graph_chat: visible_session_id.and_then(|id| self.graph_chat.snapshot(id)),
            theme: self.settings.theme,
            update_status: self.update_status.as_ref(),
            working_dir: project.working_dir,
        }
    }
}

/// Returns the session visible behind the active presentation mode.
pub(super) fn visible_review_session_id(mode: &AppMode) -> Option<&str> {
    match mode {
        AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::DiffLoading { session_id, .. }
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
        AppMode::ChatGptSignIn { dialog } => Some(dialog.session_id()),
        AppMode::ApiKeyInput { dialog } => Some(dialog.session_id()),
        AppMode::Usage(panel) => Some(panel.restore.session_id()),
        AppMode::ExecutionGraph(_)
        | AppMode::List
        | AppMode::SessionCreation { .. }
        | AppMode::PreCommitHookWarning { .. }
        | AppMode::ProjectCatalog { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => None,
    }
}

/// Reads one coherent frame clock snapshot.
fn read_frame_time(clock_client: &dyn Clock) -> FrameTime {
    let system_time = clock_client.now_system_time();
    let wall_clock_unix_seconds = session::unix_timestamp_from_system_time(system_time);

    FrameTime::new(
        wall_clock_unix_seconds,
        clock::unix_timestamp_millis(system_time),
        clock_client.local_utc_offset_seconds(wall_clock_unix_seconds),
    )
}

#[cfg(test)]
#[path = "view_test.rs"]
mod tests;
