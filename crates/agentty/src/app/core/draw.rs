//! Draw helpers and render-facing accessors for the app core module.

use ratatui::Frame;

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
            AppMode::List
            | AppMode::ReviewDetail { .. }
            | AppMode::SessionCreation { .. }
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. } => {
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
        let current_tab = self.tabs.current();
        let latest_available_version = self.latest_available_version.as_deref();
        let session_progress_messages = &self.session_progress_messages;
        let update_status = self.update_status.as_ref();
        let wall_clock_unix_seconds =
            session::unix_timestamp_from_system_time(self.sessions.state().now_system_time());
        let status_bar_fyi_rotation_index =
            u64::try_from(wall_clock_unix_seconds.div_euclid(60)).unwrap_or_default();
        let review_comment_cache = self.services.review_comment_cache();
        let requested_review_selected_index = self.requested_review_selected_index();
        let mode = &self.mode;
        let requested_review_table_state = &mut self.requested_review_table_state;
        let project_render_parts = self.projects.render_parts();
        let session_render_parts = self.sessions.render_parts();
        let settings = &mut self.settings;
        let _theme_scope = style::scoped_active_theme(settings.theme);

        ui::render(
            frame,
            ui::RenderContext {
                active_project_id: project_render_parts.active_project_id,
                current_tab,
                git_branch: project_render_parts.git_branch,
                diff_layout_cache: &self.diff_layout_cache,
                git_upstream_ref: project_render_parts.git_upstream_ref,
                git_status: project_render_parts.git_status,
                latest_available_version,
                markdown_render_cache: &self.markdown_render_cache,
                update_status,
                mode,
                output_layout_cache: &self.session_output_layout_cache,
                project_table_state: project_render_parts.table_state,
                projects: project_render_parts.project_items,
                review_comment_cache: &review_comment_cache,
                requested_reviews: &self.requested_reviews,
                requested_review_selected_index,
                requested_review_table_state,
                active_prompt_outputs: session_render_parts.active_prompt_outputs,
                session_branch_names: session_render_parts.session_branch_names,
                session_git_statuses: session_render_parts.session_git_statuses,
                session_index_by_id: session_render_parts.session_index_by_id,
                session_progress_messages,
                session_update_versions: &self.last_seen_session_update_versions,
                session_worktree_availability: session_render_parts.session_worktree_availability,
                settings,
                stats_activity: session_render_parts.stats_activity,
                sessions: session_render_parts.sessions,
                status_bar_fyi_rotation_index,
                table_state: session_render_parts.table_state,
                working_dir: project_render_parts.working_dir,
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
                .sessions()
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
