//! Draw helpers and render-facing accessors for the app core module.

use super::state::{App, UpdateStatus};
use crate::app::tab::Tab;
use crate::domain::session::{PublishedBranchSyncStatus, Session, Status};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, HelpContext};

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
    pub(crate) fn prompt_slash_state(&self) -> crate::presentation::prompt::PromptSlashState {
        crate::presentation::prompt::PromptSlashState::with_available_agent_kinds(
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
            | AppMode::LaunchConfigurationSelector {
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
