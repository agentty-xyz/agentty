//! Draw helpers and render-facing accessors for the app core module.

use std::collections::HashMap;

use super::state::{App, UpdateStatus};
use crate::app::project::ProjectRenderParts;
use crate::app::session::SessionRenderParts;
use crate::app::tab::Tab;
use crate::app::{ReviewCacheEntry, review, session};
use crate::domain::agent::{AgentCliInfo, AgentModel};
use crate::domain::session::{PublishedBranchSyncStatus, Session, SessionId, Status};
use crate::domain::system_log::SystemLogBuffer;
use crate::infra::review_comment_cache::ReviewCommentCache;
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, HelpContext};
use crate::presentation::table_state::TableViewState;

/// Borrowed application projection required to render one frame.
pub(crate) struct AppRenderParts<'a> {
    pub(crate) available_agent_clis: Vec<AgentCliInfo>,
    pub(crate) current_tab: Tab,
    pub(crate) last_seen_session_update_versions: &'a HashMap<SessionId, u64>,
    pub(crate) latest_available_version: Option<&'a str>,
    pub(crate) mode: &'a AppMode,
    pub(crate) project: ProjectRenderParts<'a>,
    pub(crate) requested_review_selected_index: Option<usize>,
    pub(crate) requested_review_table_state: &'a mut TableViewState,
    pub(crate) requested_reviews: &'a crate::app::RequestedReviewState,
    pub(crate) review_comment_cache: ReviewCommentCache,
    pub(crate) session: SessionRenderParts<'a>,
    pub(crate) session_progress_messages: &'a HashMap<SessionId, String>,
    pub(crate) session_review_snapshot: Option<VisibleSessionReview<'a>>,
    pub(crate) settings: &'a mut crate::app::SettingsManager,
    pub(crate) status_bar_fyi_rotation_index: u64,
    pub(crate) system_log_tail_offset: u16,
    pub(crate) system_logs: &'a SystemLogBuffer,
    pub(crate) update_status: Option<&'a UpdateStatus>,
    pub(crate) wall_clock_unix_seconds: i64,
}

/// Focused-review state projected for the session visible in one frame.
pub struct VisibleSessionReview<'a> {
    /// Stable identifier of the visible session.
    pub session_id: &'a str,
    /// Loading or failure message shown in place of completed review text.
    pub status_message: Option<String>,
    /// Completed focused-review text, when available.
    pub text: Option<&'a str>,
}

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

    /// Borrows the disjoint application fields needed to render one frame.
    pub(crate) fn render_parts(&mut self) -> AppRenderParts<'_> {
        let current_tab = self.tabs.current();
        let available_agent_clis = self.services.available_agent_clis();
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
        let session_review_snapshot = visible_session_review_snapshot(
            mode,
            &self.review_cache,
            self.settings.default_review_selection.model(),
        );
        let requested_review_table_state = &mut self.requested_review_table_state;
        let project_render_parts = self.projects.render_parts();
        let session_render_parts = self.sessions.render_parts();
        let settings = &mut self.settings;

        AppRenderParts {
            available_agent_clis,
            current_tab,
            last_seen_session_update_versions: &self.last_seen_session_update_versions,
            latest_available_version,
            mode,
            project: project_render_parts,
            requested_review_selected_index,
            requested_review_table_state,
            requested_reviews: &self.requested_reviews,
            review_comment_cache,
            session: session_render_parts,
            session_progress_messages,
            session_review_snapshot,
            settings,
            status_bar_fyi_rotation_index,
            system_log_tail_offset: self.system_log_tail_offset,
            system_logs: &self.system_logs,
            update_status,
            wall_clock_unix_seconds,
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

/// Projects focused-review cache state for the session currently visible behind
/// the active mode without cloning the cached review body during rendering.
fn visible_session_review_snapshot<'a>(
    mode: &'a AppMode,
    review_cache: &'a HashMap<SessionId, ReviewCacheEntry>,
    review_model: AgentModel,
) -> Option<VisibleSessionReview<'a>> {
    let session_id = match mode {
        AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::Help {
            context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
            ..
        } => session_id,
        AppMode::Confirmation {
            restore_view: Some(restore_view),
            ..
        }
        | AppMode::LaunchConfigurationSelector { restore_view, .. }
        | AppMode::PublishBranchInput { restore_view, .. }
        | AppMode::ViewInfoPopup { restore_view, .. } => &restore_view.session_id,
        AppMode::List
        | AppMode::ReviewDetail { .. }
        | AppMode::SessionCreation { .. }
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Help { .. } => return None,
    };
    let (status_message, text) = review::review_view_state(review_cache, session_id, review_model);

    Some(VisibleSessionReview {
        session_id: session_id.as_str(),
        status_message,
        text,
    })
}
