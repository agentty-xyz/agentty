//! Draw helpers and render-facing accessors for the app core module.

use ratatui::Frame;

use super::roadmap::ActiveProjectRoadmap;
use super::state::{App, UpdateStatus};
use crate::app::session;
use crate::app::tab::Tab;
use crate::domain::session::{PublishedBranchSyncStatus, Session, Status};
use crate::ui;
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, HelpContext};
use crate::ui::style;

impl App {
    /// Returns the active project identifier.
    pub fn active_project_id(&self) -> i64 {
        self.projects.active_project_id()
    }

    /// Returns the working directory for the active project.
    pub fn working_dir(&self) -> &std::path::Path {
        self.projects.working_dir()
    }

    /// Returns the git branch of the active project, when available.
    pub fn git_branch(&self) -> Option<&str> {
        self.projects.git_branch()
    }

    /// Returns the upstream reference tracked by the active project branch,
    /// when available.
    pub fn git_upstream_ref(&self) -> Option<&str> {
        self.projects.git_upstream_ref()
    }

    /// Returns the latest ahead/behind snapshot from reducer-applied events.
    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.projects.git_status()
    }

    /// Builds prompt slash-menu state from the cached machine-scoped agent
    /// availability snapshot.
    pub(crate) fn prompt_slash_state(&self) -> crate::ui::state::prompt::PromptSlashState {
        crate::ui::state::prompt::PromptSlashState::with_available_agent_kinds(
            self.services.available_agent_kinds(),
        )
    }

    /// Returns the newer stable `agentty` version when an update is available.
    pub fn latest_available_version(&self) -> Option<&str> {
        self.latest_available_version.as_deref()
    }

    /// Returns the current background auto-update status, if any.
    pub fn update_status(&self) -> Option<&UpdateStatus> {
        self.update_status.as_ref()
    }

    /// Returns whether the visible UI contains spinner or timer state that
    /// should force periodic redraws even when no new events arrive.
    pub(crate) fn has_visible_tick_driven_ui(&self) -> bool {
        match &self.mode {
            AppMode::List | AppMode::Confirmation { .. } | AppMode::SyncBlockedPopup { .. } => {
                self.list_background_has_tick_driven_ui()
                    || matches!(
                        &self.mode,
                        AppMode::SyncBlockedPopup {
                            is_loading: true,
                            ..
                        }
                    )
            }
            AppMode::View { session_id, .. }
            | AppMode::Prompt { session_id, .. }
            | AppMode::Question { session_id, .. }
            | AppMode::OpenCommandSelector {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            }
            | AppMode::PublishBranchInput {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            } => self.session_has_tick_driven_ui(session_id),
            AppMode::ViewInfoPopup {
                is_loading,
                restore_view,
                ..
            } => *is_loading || self.session_has_tick_driven_ui(&restore_view.session_id),
            AppMode::Diff { .. } => false,
            AppMode::Help { context, .. } => self.help_overlay_has_tick_driven_ui(context),
        }
    }

    /// Renders a complete UI frame by applying the active color theme,
    /// assembling a [`ui::RenderContext`] from current app state, and
    /// dispatching to the UI render pipeline.
    pub fn draw(&mut self, frame: &mut Frame) {
        let has_tasks_tab = self.active_project_has_tasks_tab();
        self.tabs.normalize(has_tasks_tab);
        let active_project_id = self.projects.active_project_id();
        let current_tab = self.tabs.current();
        let working_dir = self.projects.working_dir().to_path_buf();
        let git_branch = self.projects.git_branch().map(str::to_string);
        let git_upstream_ref = self.projects.git_upstream_ref().map(str::to_string);
        let git_status = self.projects.git_status();
        let latest_available_version = self.latest_available_version.as_deref().map(str::to_string);
        let task_roadmap = self.active_project_roadmap.as_ref().and_then(|roadmap| {
            if let ActiveProjectRoadmap::Loaded(content) = roadmap {
                return Some(content.clone());
            }

            None
        });
        let task_roadmap_error = self.active_project_roadmap.as_ref().and_then(|roadmap| {
            if let ActiveProjectRoadmap::LoadError(message) = roadmap {
                return Some(message.clone());
            }

            None
        });
        let session_git_statuses = self.sessions.session_git_statuses().clone();
        let session_branch_names = self.sessions.session_branch_names().clone();
        let session_index_by_id = self.sessions.state().session_index_by_id().clone();
        let session_worktree_availability = self.sessions.session_worktree_availability().clone();
        let active_prompt_outputs = self.sessions.active_prompt_outputs().clone();
        let session_progress_messages = self.session_progress_messages.clone();
        let update_status = self.update_status().cloned();
        let wall_clock_unix_seconds =
            session::unix_timestamp_from_system_time(self.sessions.state().clock.now_system_time());
        let status_bar_fyi_rotation_index =
            u64::try_from(wall_clock_unix_seconds.div_euclid(60)).unwrap_or_default();
        let projects = self.projects.project_items().to_vec();
        let review_comment_cache = self.services.review_comment_cache();
        let mode = &self.mode;
        let project_table_state = self.projects.project_table_state_mut();
        let (sessions, stats_activity, table_state) = self.sessions.render_parts();
        let settings = &mut self.settings;
        style::set_active_theme(settings.theme);

        ui::render(
            frame,
            ui::RenderContext {
                active_project_id,
                current_tab,
                has_tasks_tab,
                git_branch: git_branch.as_deref(),
                git_upstream_ref: git_upstream_ref.as_deref(),
                git_status,
                latest_available_version: latest_available_version.as_deref(),
                markdown_render_cache: &self.markdown_render_cache,
                update_status: update_status.as_ref(),
                mode,
                output_layout_cache: &self.session_output_layout_cache,
                project_table_state,
                projects: &projects,
                review_comment_cache: &review_comment_cache,
                task_roadmap: task_roadmap.as_deref(),
                task_roadmap_error: task_roadmap_error.as_deref(),
                task_roadmap_scroll_offset: self.task_roadmap_scroll_offset,
                active_prompt_outputs: &active_prompt_outputs,
                session_branch_names: &session_branch_names,
                session_git_statuses: &session_git_statuses,
                session_index_by_id: &session_index_by_id,
                session_progress_messages: &session_progress_messages,
                session_update_versions: &self.last_seen_session_update_versions,
                session_worktree_availability: &session_worktree_availability,
                settings,
                stats_activity,
                sessions,
                status_bar_fyi_rotation_index,
                table_state,
                working_dir: &working_dir,
                wall_clock_unix_seconds,
            },
        );
    }

    /// Returns whether the currently visible list background contains any
    /// spinner or timer-driven session rows.
    fn list_background_has_tick_driven_ui(&self) -> bool {
        self.tabs.current() == Tab::Sessions
            && self
                .sessions
                .state()
                .sessions
                .iter()
                .any(Self::session_tick_driven_ui_active)
    }

    /// Returns whether the help overlay keeps a dynamic background visible.
    fn help_overlay_has_tick_driven_ui(&self, context: &HelpContext) -> bool {
        match context {
            HelpContext::List { .. } => self.list_background_has_tick_driven_ui(),
            HelpContext::View { session_id, .. } => self.session_has_tick_driven_ui(session_id),
            HelpContext::Diff { .. } => false,
        }
    }

    /// Returns whether the visible session view for `session_id` contains any
    /// spinner or elapsed-timer state.
    fn session_has_tick_driven_ui(&self, session_id: &str) -> bool {
        self.sessions
            .state()
            .session_for_id(session_id)
            .is_some_and(Self::session_tick_driven_ui_active)
    }

    /// Returns whether one session snapshot currently renders any time-driven
    /// indicator.
    fn session_tick_driven_ui_active(session: &Session) -> bool {
        session.in_progress_started_at.is_some()
            || matches!(
                session.status,
                Status::AgentReview | Status::InProgress | Status::Merging | Status::Rebasing
            )
            || session.published_branch_sync_status == PublishedBranchSyncStatus::InProgress
    }
}
