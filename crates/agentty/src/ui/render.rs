use std::collections::HashMap;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use crate::app::session::session_branch;
use crate::app::session_state::SessionGitStatus;
use crate::app::{ProjectSyncStatus, Tab, UpdateStatus};
use crate::domain::agent::{AgentCliInfo, ReasoningLevel};
use crate::domain::project::ProjectListItem;
use crate::domain::resource::SessionResources;
use crate::domain::session::{DailyActivity, Session, SessionId};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, HelpContext};
use crate::presentation::frame_time::FrameTime;
use crate::ui::{RenderCacheStore, component, layout, page, router};

/// Focused-review display state projected from the app cache for one visible
/// session.
pub struct SessionReviewSnapshot<'a> {
    /// Stable identifier of the session owning the cached review.
    pub session_id: &'a str,
    /// Focused-review markdown, when generated.
    pub text: Option<&'a str>,
}

/// A trait for UI pages that enforces a standard rendering interface.
pub trait Page {
    /// Renders a page in the provided frame and area.
    fn render(&mut self, f: &mut Frame, area: Rect);
}

/// A trait for UI components that enforces a standard rendering interface.
pub trait Component {
    /// Renders a component in the provided frame and area.
    fn render(&self, f: &mut Frame, area: Rect);
}

/// Immutable data required to draw a single UI frame.
pub struct RenderContext<'a> {
    /// Exact prompt transcript blocks keyed by session id for active turns.
    pub active_prompt_outputs: &'a HashMap<SessionId, String>,
    /// Identifier of the currently active project.
    pub active_project_id: i64,
    /// Locally available agent CLI executables and detected versions.
    pub available_agent_clis: &'a [AgentCliInfo],
    /// Active top-level tab selection.
    pub current_tab: Tab,
    /// Version label rendered in the status bar.
    pub current_version_display_text: &'a str,
    /// Active project-scoped reasoning level used by session pages.
    pub default_reasoning_level: ReasoningLevel,
    /// One coherent wall-clock snapshot used by this render pass.
    pub(crate) frame_time: FrameTime,
    /// Current local branch name for the active project.
    pub git_branch: Option<&'a str>,
    /// Current upstream reference tracked by the active project branch.
    pub git_upstream_ref: Option<&'a str>,
    /// Latest ahead/behind counts for the active project branch.
    pub git_status: Option<(u32, u32)>,
    /// Whether tmux-only worktree actions can be rendered.
    pub is_tmux_session: bool,
    /// Newer stable version when one is available.
    pub latest_available_version: Option<&'a str>,
    /// Current app mode and its transient state.
    pub mode: &'a AppMode,
    /// Cached most-recently-opened ordering over `projects`, reused by the
    /// project switcher popup instead of re-sorting each frame.
    pub mru_project_order: &'a [usize],
    /// UI-owned cache resources shared by every page in this frame.
    pub render_cache_store: &'a RenderCacheStore,
    /// Table selection state for the projects list.
    pub project_table_state: &'a mut TableState,
    /// Project rows available for rendering.
    pub projects: &'a [ProjectListItem],
    /// Latest explicit project-sync lifecycle state.
    pub(crate) project_sync_status: Option<&'a ProjectSyncStatus>,
    /// Focused-review state for the visible session, projected from the app
    /// cache for this render pass.
    pub session_review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    /// Detected session worktree branch names keyed by session id.
    pub session_branch_names: &'a HashMap<SessionId, String>,
    /// Latest session-branch ahead/behind snapshots keyed by session id,
    /// including both base-branch and tracked-remote comparisons.
    pub session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    pub session_index_by_id: &'a HashMap<SessionId, usize>,
    /// Background thinking messages keyed by session id.
    pub session_progress_messages: &'a HashMap<SessionId, String>,
    /// Internal host-temperature sidecar for the tracked process roots.
    pub(crate) session_cpu_temperatures: &'a HashMap<SessionId, f32>,
    /// Latest tracked process-tree totals.
    pub session_resources: &'a HashMap<SessionId, SessionResources>,
    /// Latest observable update versions keyed by session id.
    pub session_update_versions: &'a HashMap<SessionId, u64>,
    /// Whether each rendered session currently has a materialized worktree on
    /// disk, keyed by session id.
    pub session_worktree_availability: &'a HashMap<SessionId, bool>,
    /// Settings-screen projection when the active tab can render it.
    pub(crate) settings_screen: Option<&'a crate::presentation::settings::SettingsScreenSnapshot>,
    /// Daily session activity series used by dashboard activity summaries.
    pub stats_activity: &'a [DailyActivity],
    /// Session rows available for rendering.
    pub sessions: &'a [Session],
    /// Table selection state for the session list.
    pub table_state: &'a mut TableState,
    /// Background auto-update progress state for the status bar.
    pub update_status: Option<&'a UpdateStatus>,
    /// Absolute one-minute rotation slot used for page-scoped status-bar FYIs.
    pub status_bar_fyi_rotation_index: u64,
    /// Working directory for the active project.
    pub working_dir: &'a Path,
}

/// Project-scoped footer inputs used when no session-specific footer override
/// is active.
#[derive(Clone, Copy)]
struct ProjectFooterContext<'a> {
    /// Current local branch name for the active project.
    git_branch: Option<&'a str>,
    /// Latest ahead/behind counts for the active project branch.
    git_status: Option<(u32, u32)>,
    /// Current upstream reference tracked by the active project branch.
    git_upstream_ref: Option<&'a str>,
    /// Working directory displayed in the footer.
    working_dir: &'a Path,
}

/// Borrowed data required to render the footer bar for one frame.
#[derive(Clone, Copy)]
struct FooterBarRenderContext<'a> {
    /// Active top-level tab used to suppress workspace context on dashboard
    /// pages.
    current_tab: Tab,
    /// Active app mode used to resolve session-scoped footer overrides.
    mode: &'a AppMode,
    /// Project footer values used when the active mode is not session-scoped.
    project: ProjectFooterContext<'a>,
    /// Detected session worktree branch names keyed by session id.
    session_branch_names: &'a HashMap<SessionId, String>,
    /// Latest session-branch ahead/behind snapshots keyed by session id.
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    session_index_by_id: &'a HashMap<SessionId, usize>,
    /// Session rows available for resolving the active footer session.
    sessions: &'a [Session],
}

/// Renders a complete frame including status bar, content area, and footer.
pub fn render(f: &mut Frame, context: RenderContext<'_>) {
    let layout::AppFrameAreas {
        content_area,
        footer_bar_area,
        status_bar_area,
    } = layout::app_frame_areas(f.area());

    component::status_bar::StatusBar::new(context.current_version_display_text.to_string())
        .latest_available_version(
            context
                .latest_available_version
                .map(std::string::ToString::to_string),
        )
        .page_fyis(page::fyi::current_page_messages(
            context.current_tab,
            context.mode,
        ))
        .fyi_rotation_index(context.status_bar_fyi_rotation_index)
        .project_sync_status(context.project_sync_status.cloned())
        .update_status(context.update_status.cloned())
        .render(f, status_bar_area);
    render_footer_bar(
        f,
        footer_bar_area,
        FooterBarRenderContext {
            current_tab: context.current_tab,
            mode: context.mode,
            project: ProjectFooterContext {
                git_branch: context.git_branch,
                git_status: context.git_status,
                git_upstream_ref: context.git_upstream_ref,
                working_dir: context.working_dir,
            },
            session_branch_names: context.session_branch_names,
            session_git_statuses: context.session_git_statuses,
            session_index_by_id: context.session_index_by_id,
            sessions: context.sessions,
        },
    );

    router::route_frame(f, content_area, context);
}

/// Renders the footer bar with directory, branch, and project- or
/// session-scoped git status info.
///
/// Project branches show upstream-tracking counts. Session branches reuse the
/// same footer widget but inject counts relative to each session's base
/// branch and, when available, its tracked remote branch.
fn render_footer_bar(f: &mut Frame, footer_bar_area: Rect, context: FooterBarRenderContext<'_>) {
    let FooterBarRenderContext {
        current_tab,
        mode,
        project,
        session_branch_names,
        session_git_statuses,
        session_index_by_id,
        sessions,
    } = context;
    let session_id = match mode {
        AppMode::Confirmation {
            session_id: Some(session_id),
            ..
        }
        | AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::DiffLoading { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::ViewInfoPopup {
            restore_view: ConfirmationViewMode { session_id, .. },
            ..
        }
        | AppMode::LaunchConfigurationSelector {
            restore_view: ConfirmationViewMode { session_id, .. },
            ..
        }
        | AppMode::PublishBranchInput {
            restore_view: ConfirmationViewMode { session_id, .. },
            ..
        }
        | AppMode::Help {
            context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
            ..
        } => Some(session_id.as_str()),
        _ => None,
    };
    let session_for_footer = session_id
        .and_then(|session_identifier| session_index_by_id.get(session_identifier).copied())
        .and_then(|session_index| sessions.get(session_index));

    let (
        footer_dir,
        footer_branch,
        footer_base_ref,
        footer_upstream_ref,
        footer_base_status,
        footer_status,
    ) = match session_for_footer {
        Some(session) => {
            let session_status = session_git_statuses
                .get(&session.id)
                .copied()
                .unwrap_or_default();

            (
                session.folder.to_string_lossy().to_string(),
                Some(
                    session_branch_names
                        .get(&session.id)
                        .cloned()
                        .unwrap_or_else(|| session_branch(&session.id)),
                ),
                Some(session.base_branch.clone()),
                session.published_upstream_ref.clone(),
                session_status.base_status,
                session_status.remote_status,
            )
        }
        None => (
            project.working_dir.to_string_lossy().to_string(),
            project.git_branch.map(std::string::ToString::to_string),
            None,
            project
                .git_upstream_ref
                .map(std::string::ToString::to_string),
            None,
            project.git_status,
        ),
    };

    let workspace_context_visible = session_for_footer.is_some() || current_tab != Tab::Projects;

    component::footer_bar::FooterBar::new(footer_dir)
        .git_branch(footer_branch)
        .git_base_ref(footer_base_ref)
        .git_base_status(footer_base_status)
        .git_upstream_ref(footer_upstream_ref)
        .git_status(footer_status)
        .workspace_context_visible(workspace_context_visible)
        .render(f, footer_bar_area);
}

#[cfg(test)]
#[path = "render_test.rs"]
mod tests;
