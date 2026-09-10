use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::Tab;
use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::{AgentCliInfo, ReasoningLevel};
use crate::domain::project::{ProjectListItem, ordered_project_items};
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    DailyActivity, Session, SessionId, activity_day_key_with_offset, can_append_session_to_stack,
    can_create_stacked_child,
};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget,
    DiffReviewComments, DiffSidebarFocus, HelpContext, allows_diff_line_comment_reply,
};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::setting::SettingsScreenSnapshot;
use crate::ui::{
    Component, Page, RenderContext, SessionReviewSnapshot, component, markdown, overlay, page,
};

/// Shared mutable routing data reused across app modes in `route_frame`.
struct RouteSharedContext<'a> {
    /// Identifier for the active project shared across list-mode renders.
    active_project_id: i64,
    /// Locally available agent CLI executables and detected versions.
    available_agent_clis: &'a [AgentCliInfo],
    current_tab: Tab,
    default_reasoning_level: ReasoningLevel,
    /// Cached most-recently-opened ordering over `projects`.
    mru_project_order: &'a [usize],
    project_table_state: &'a mut TableState,
    projects: &'a [ProjectListItem],
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    sessions: &'a [Session],
    settings_screen: Option<&'a SettingsScreenSnapshot>,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl RouteSharedContext<'_> {
    /// Returns whether the selected session can be used as a stacked parent.
    fn can_create_stacked_session(&self) -> bool {
        self.current_tab == Tab::Sessions
            && self
                .table_state
                .selected()
                .and_then(|selected_index| self.sessions.get(selected_index))
                .is_some_and(|session| can_create_stacked_child(self.sessions, session.id.as_str()))
    }

    /// Returns whether the selected session has at least one eligible stack
    /// parent.
    fn can_append_selected_session(&self) -> bool {
        let Some(session_id) = self
            .table_state
            .selected()
            .and_then(|selected_index| self.sessions.get(selected_index))
            .map(|session| session.id.as_str())
        else {
            return false;
        };

        self.sessions.iter().any(|candidate| {
            can_append_session_to_stack(self.sessions, session_id, candidate.id.as_str())
        })
    }

    /// Returns eligible parent rows for one source session.
    fn stack_append_parent_sessions(&self, session_id: &str) -> Vec<&Session> {
        self.sessions
            .iter()
            .filter(|candidate| {
                can_append_session_to_stack(self.sessions, session_id, candidate.id.as_str())
            })
            .collect()
    }
}

/// UI-private base page selected for the active mode.
#[derive(Clone, Copy)]
enum Surface<'a> {
    Diff {
        diff: &'a str,
        file_explorer_selected_index: usize,
        focus: DiffFocus,
        line_comments: &'a DiffLineComments,
        preview: &'a DiffPreview,
        review_comments: Option<&'a DiffReviewComments>,
        restore: Option<&'a DiffRestoreTarget>,
        scroll_offset: u16,
        selected_diff_line_index: usize,
        session_id: &'a str,
        sidebar_focus: DiffSidebarFocus,
    },
    DiffLoading {
        session_id: &'a str,
        sidebar_focus: DiffSidebarFocus,
    },
    List,
    Session {
        mode: SessionSurfaceMode<'a>,
        scroll_offset: Option<u16>,
        session_id: &'a str,
    },
}

impl Surface<'_> {
    /// Returns the stable base-page identity used across mode overlays.
    fn kind(self) -> SurfaceKind {
        match self {
            Self::Diff { .. } | Self::DiffLoading { .. } => SurfaceKind::Diff,
            Self::List => SurfaceKind::List,
            Self::Session { .. } => SurfaceKind::Session,
        }
    }
}

/// Stable identity for the base page painted beneath mode-specific overlays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceKind {
    Diff,
    List,
    Session,
}

/// Session mode used while rendering a chat surface.
#[derive(Clone, Copy)]
enum SessionSurfaceMode<'a> {
    Interactive(&'a AppMode),
    View,
}

/// Borrowed inputs for rendering a session chat page.
#[derive(Clone, Copy)]
struct SessionChatRenderContext<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    frame_time: FrameTime,
    is_tmux_session: bool,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    mode: &'a AppMode,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    scroll_offset: Option<u16>,
    session_cpu_temperatures: &'a HashMap<SessionId, f32>,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<SessionId, String>,
    /// Latest tracked process-tree totals.
    session_resources: &'a HashMap<SessionId, SessionResources>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    sessions: &'a [Session],
}

/// UI-owned immutable resources shared by every surface in one frame.
///
/// Cache handles and session-derived display snapshots stay bundled until the
/// selected surface projects the narrower page input it needs.
#[derive(Clone, Copy)]
struct FrameResources<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    diff_layout_cache: &'a page::diff::DiffLayoutCache,
    frame_time: FrameTime,
    is_tmux_session: bool,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    session_cpu_temperatures: &'a HashMap<SessionId, f32>,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    session_progress_messages: &'a HashMap<SessionId, String>,
    /// Latest tracked process-tree totals.
    session_resources: &'a HashMap<SessionId, SessionResources>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
}

impl<'a> FrameResources<'a> {
    /// Creates a session chat render context from shared route inputs and the
    /// mode-specific session selection.
    fn session_chat<'b>(
        self,
        sessions: &'b [Session],
        mode: &'b AppMode,
        session_id: &'b str,
        scroll_offset: Option<u16>,
    ) -> SessionChatRenderContext<'b>
    where
        'a: 'b,
    {
        SessionChatRenderContext {
            active_prompt_outputs: self.active_prompt_outputs,
            default_reasoning_level: self.default_reasoning_level,
            is_tmux_session: self.is_tmux_session,
            markdown_render_cache: self.markdown_render_cache,
            mode,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            session_id,
            session_git_statuses: self.session_git_statuses,
            session_cpu_temperatures: self.session_cpu_temperatures,
            session_resources: self.session_resources,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions,
            scroll_offset,
            frame_time: self.frame_time,
        }
    }
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        active_prompt_outputs,
        available_agent_clis,
        current_tab,
        default_reasoning_level,
        mode,
        mru_project_order,
        render_cache_store,
        project_table_state,
        projects,
        session_review_snapshot,
        session_git_statuses,
        session_cpu_temperatures,
        session_resources,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        settings_screen,
        stats_activity,
        sessions,
        table_state,
        frame_time,
        is_tmux_session,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        active_project_id,
        available_agent_clis,
        current_tab,
        default_reasoning_level,
        mru_project_order,
        project_table_state,
        projects,
        session_git_statuses,
        sessions,
        settings_screen,
        stats_activity,
        table_state,
    };

    let resources = FrameResources {
        active_prompt_outputs,
        default_reasoning_level,
        diff_layout_cache: render_cache_store.diff_layout_cache(),
        is_tmux_session,
        markdown_render_cache: render_cache_store.markdown_render_cache(),
        output_layout_cache: render_cache_store.session_output_layout_cache(),
        review_snapshot: session_review_snapshot,
        session_git_statuses,
        session_cpu_temperatures,
        session_resources,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        frame_time,
    };

    render_surface(f, area, surface_for_mode(mode), &mut shared, resources);
    render_mode_overlay(f, area, mode, &shared, resources);
}

/// Resolves the stable base-page identity for terminal transition handling.
pub(crate) fn surface_kind_for_mode(mode: &AppMode) -> SurfaceKind {
    surface_for_mode(mode).kind()
}

/// Resolves the base page painted for every mode before any overlay.
fn surface_for_mode(mode: &AppMode) -> Surface<'_> {
    match mode {
        AppMode::Confirmation {
            confirmation_intent:
                ConfirmationIntent::ContinueSession
                | ConfirmationIntent::ForkSession
                | ConfirmationIntent::MergeSession
                | ConfirmationIntent::RegenerateReview
                | ConfirmationIntent::DetachManagedSession
                | ConfirmationIntent::OpenManagedWorktree
                | ConfirmationIntent::ChooseIntegrationApproach,
            restore_view: Some(restore_view),
            ..
        }
        | AppMode::ViewInfoPopup { restore_view, .. }
        | AppMode::LaunchConfigurationSelector { restore_view, .. }
        | AppMode::PublishBranchInput { restore_view, .. } => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: restore_view.scroll_offset,
            session_id: &restore_view.session_id,
        },
        AppMode::List
        | AppMode::SessionCreation { .. }
        | AppMode::StackAppendParentSelection { .. }
        | AppMode::PreCommitHookWarning { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Confirmation { .. } => Surface::List,
        AppMode::Help { context, .. } => surface_for_help_context(context),
        AppMode::View {
            session_id,
            scroll_offset,
        } => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: *scroll_offset,
            session_id,
        },
        AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        }
        | AppMode::Question {
            session_id,
            scroll_offset,
            ..
        } => Surface::Session {
            mode: SessionSurfaceMode::Interactive(mode),
            scroll_offset: *scroll_offset,
            session_id,
        },
        AppMode::DiffLoading {
            session_id,
            sidebar_focus,
            ..
        } => Surface::DiffLoading {
            session_id,
            sidebar_focus: *sidebar_focus,
        },
        AppMode::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments,
            restore,
            scroll_offset,
            selected_diff_line_index,
            session_id,
            ..
        } => Surface::Diff {
            diff,
            file_explorer_selected_index: *file_explorer_selected_index,
            focus: *focus,
            line_comments,
            preview,
            review_comments: review_comments.as_ref(),
            restore: restore.as_deref(),
            scroll_offset: *scroll_offset,
            selected_diff_line_index: *selected_diff_line_index,
            session_id,
            sidebar_focus: review_comments
                .as_ref()
                .map_or(DiffSidebarFocus::Files, |review_comments| {
                    review_comments.sidebar_focus
                }),
        },
    }
}

/// Resolves the page restored behind a context-aware help overlay.
fn surface_for_help_context(context: &HelpContext) -> Surface<'_> {
    match context {
        HelpContext::List { .. } => Surface::List,
        HelpContext::View {
            session_id,
            scroll_offset,
            ..
        } => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: *scroll_offset,
            session_id,
        },
        HelpContext::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments,
            restore,
            scroll_offset,
            selected_diff_line_index,
            session_id,
            ..
        } => Surface::Diff {
            diff,
            file_explorer_selected_index: *file_explorer_selected_index,
            focus: *focus,
            line_comments,
            preview,
            review_comments: review_comments.as_deref(),
            restore: restore.as_deref(),
            scroll_offset: *scroll_offset,
            selected_diff_line_index: *selected_diff_line_index,
            session_id,
            sidebar_focus: review_comments
                .as_deref()
                .map_or(DiffSidebarFocus::Files, |review_comments| {
                    review_comments.sidebar_focus
                }),
        },
    }
}

/// Paints one base surface through the only page-construction boundary.
fn render_surface(
    f: &mut Frame,
    area: Rect,
    surface: Surface<'_>,
    shared: &mut RouteSharedContext<'_>,
    resources: FrameResources<'_>,
) {
    match surface {
        Surface::List => render_list_background(f, area, shared, resources.frame_time),
        Surface::Session {
            mode,
            scroll_offset,
            session_id,
        } => render_session_surface(
            f,
            area,
            mode,
            scroll_offset,
            session_id,
            shared.sessions,
            resources,
        ),
        Surface::DiffLoading {
            session_id,
            sidebar_focus,
        } => {
            let preview = DiffPreview::default();
            render_diff_surface(
                f,
                area,
                DiffSurfaceInput {
                    diff: "Loading diff...",
                    file_explorer_selected_index: 0,
                    focus: DiffFocus::Files,
                    is_loading: true,
                    line_comments: &DiffLineComments::default(),
                    preview: &preview,
                    review_comments: None,
                    restore: None,
                    scroll_offset: 0,
                    selected_diff_line_index: 0,
                    session_id,
                    sidebar_focus,
                },
                shared.sessions,
                resources,
            );
        }
        Surface::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments,
            restore,
            scroll_offset,
            selected_diff_line_index,
            session_id,
            sidebar_focus,
        } => render_diff_surface(
            f,
            area,
            DiffSurfaceInput {
                diff,
                file_explorer_selected_index,
                focus,
                is_loading: false,
                line_comments,
                preview,
                review_comments,
                restore,
                scroll_offset,
                selected_diff_line_index,
                session_id,
                sidebar_focus,
            },
            shared.sessions,
            resources,
        ),
    }
}

/// Paints the overlay portion of modes after their base surface.
fn render_mode_overlay(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &RouteSharedContext<'_>,
    resources: FrameResources<'_>,
) {
    match mode {
        AppMode::List
        | AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::DiffLoading { .. }
        | AppMode::Diff { .. } => {}
        AppMode::SessionCreation {
            selected_option_index,
        } => component::session_creation_overlay::SessionCreationOverlay::new(
            *selected_option_index,
            shared.can_create_stacked_session(),
            shared.can_append_selected_session(),
        )
        .render(f, area),
        AppMode::StackAppendParentSelection {
            selected_parent_index,
            session_id,
        } => {
            render_stack_append_parent_overlay(f, area, shared, session_id, *selected_parent_index);
        }
        AppMode::PreCommitHookWarning { message } => {
            component::info_overlay::InfoOverlay::new("Pre-commit hook warning", message)
                .render(f, area);
        }
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => {
            let mru_project_items =
                ordered_project_items(shared.projects, shared.mru_project_order);
            component::project_switcher_overlay::ProjectSwitcherOverlay::new(
                &mru_project_items,
                shared.active_project_id,
                *selected_option_index,
            )
            .render(f, area);
        }
        AppMode::Confirmation { .. } => render_confirmation_overlay(f, area, mode),
        AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } => {
            let popup_message = overlay::sync_popup_message(
                default_branch.as_deref(),
                message,
                project_name.as_deref(),
            );
            component::info_overlay::InfoOverlay::new(title, &popup_message)
                .is_loading(*is_loading)
                .loading_label("Sync in progress...")
                .spinner_frame(crate::ui::icon::Icon::spinner_frame_from_millis(
                    resources.frame_time.unix_millis(),
                ))
                .render(f, area);
        }
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            title,
            ..
        } => {
            component::info_overlay::InfoOverlay::new(title, message)
                .is_loading(*is_loading)
                .loading_label(loading_label)
                .spinner_frame(crate::ui::icon::Icon::spinner_frame_from_millis(
                    resources.frame_time.unix_millis(),
                ))
                .render(f, area);
        }
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => component::help_overlay::HelpOverlay::new(help_context)
            .scroll_offset(*scroll_offset)
            .render(f, area),
        AppMode::LaunchConfigurationSelector {
            commands,
            selected_command_index,
            ..
        } => component::launch_configuration_overlay::LaunchConfigurationOverlay::new(commands)
            .selected_command_index(*selected_command_index)
            .render(f, area),
        AppMode::PublishBranchInput {
            default_branch_name,
            input,
            locked_upstream_ref,
            ..
        } => component::publish_branch_overlay::PublishBranchOverlay::new(
            input,
            default_branch_name,
            locked_upstream_ref.as_deref(),
        )
        .render(f, area),
    }
}

/// Renders the eligible parent choices for the session being appended.
fn render_stack_append_parent_overlay(
    f: &mut Frame,
    area: Rect,
    shared: &RouteSharedContext<'_>,
    session_id: &SessionId,
    selected_parent_index: usize,
) {
    let parent_sessions = shared.stack_append_parent_sessions(session_id);
    component::stack_append_parent_overlay::StackAppendParentOverlay::new(
        &parent_sessions,
        selected_parent_index,
    )
    .render(f, area);
}

/// Renders the confirmation overlay after its classified base surface.
fn render_confirmation_overlay(f: &mut Frame, area: Rect, mode: &AppMode) {
    let AppMode::Confirmation {
        confirmation_intent,
        confirmation_message,
        confirmation_title,
        selected_confirmation_index,
        ..
    } = mode
    else {
        return;
    };

    let overlay = component::confirmation_overlay::ConfirmationOverlay::new(
        confirmation_title,
        confirmation_message,
    )
    .selected_first(*selected_confirmation_index == 0);
    if *confirmation_intent == ConfirmationIntent::ChooseIntegrationApproach {
        overlay
            .option_labels("Local merges", "Review requests")
            .render(f, area);
    } else {
        overlay.render(f, area);
    }
}

/// Renders a session surface in either interactive or restored-view mode.
fn render_session_surface(
    f: &mut Frame,
    area: Rect,
    mode: SessionSurfaceMode<'_>,
    scroll_offset: Option<u16>,
    session_id: &str,
    sessions: &[Session],
    resources: FrameResources<'_>,
) {
    match mode {
        SessionSurfaceMode::Interactive(mode) => render_session_chat(
            f,
            area,
            resources.session_chat(sessions, mode, session_id, scroll_offset),
        ),
        SessionSurfaceMode::View => {
            let view_mode = AppMode::View {
                scroll_offset,
                session_id: session_id.into(),
            };
            render_session_chat(
                f,
                area,
                resources.session_chat(sessions, &view_mode, session_id, scroll_offset),
            );
        }
    }
}

/// Borrowed inputs for one diff surface.
#[derive(Clone, Copy)]
struct DiffSurfaceInput<'a> {
    diff: &'a str,
    file_explorer_selected_index: usize,
    focus: DiffFocus,
    is_loading: bool,
    line_comments: &'a DiffLineComments,
    preview: &'a DiffPreview,
    restore: Option<&'a DiffRestoreTarget>,
    review_comments: Option<&'a DiffReviewComments>,
    scroll_offset: u16,
    selected_diff_line_index: usize,
    session_id: &'a str,
    sidebar_focus: DiffSidebarFocus,
}

/// Renders a diff page for a resolved session.
fn render_diff_surface(
    f: &mut Frame,
    area: Rect,
    input: DiffSurfaceInput<'_>,
    sessions: &[Session],
    resources: FrameResources<'_>,
) {
    let Some(session) = sessions
        .iter()
        .find(|session| session.id == input.session_id)
    else {
        return;
    };

    let mut page = page::diff::DiffPage::new(page::diff::DiffPageInput {
        can_comment: allows_diff_line_comment_reply(session, sessions, input.restore),
        diff: input.diff,
        diff_layout_cache: resources.diff_layout_cache,
        file_explorer_selected_index: input.file_explorer_selected_index,
        focus: input.focus,
        line_comments: input.line_comments,
        markdown_render_cache: resources.markdown_render_cache,
        preview: input.preview,
        review_comments: input.review_comments,
        scroll_offset: input.scroll_offset,
        selected_diff_line_index: input.selected_diff_line_index,
        session,
        sidebar_focus: input.sidebar_focus,
    });
    if input.is_loading {
        page.loading().render(f, area);
    } else {
        page.render(f, area);
    }
}

/// Renders the session chat page for all session-chat modes.
fn render_session_chat(f: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        active_prompt_outputs,
        default_reasoning_level,
        is_tmux_session,
        markdown_render_cache,
        mode,
        output_layout_cache,
        review_snapshot,
        session_id,
        session_cpu_temperatures,
        session_resources,
        session_progress_messages,
        session_git_statuses,
        session_update_versions,
        session_worktree_availability,
        sessions,
        scroll_offset,
        frame_time,
    } = context;

    let Some(session_index) = sessions.iter().position(|session| session.id == session_id) else {
        return;
    };

    let active_progress = session_progress_messages
        .get(session_id)
        .map(std::string::String::as_str);
    let active_prompt_output = active_prompt_outputs
        .get(session_id)
        .map(std::string::String::as_str);
    let session_update_version = session_update_versions
        .get(session_id)
        .copied()
        .unwrap_or_default();
    let has_merge_conflict = session_git_statuses
        .get(session_id)
        .and_then(|status| status.has_merge_conflict)
        .unwrap_or(false);

    let page_input = page::session_chat::SessionChatPageInput {
        active_prompt_output,
        active_progress,
        resources: session_resources.get(session_id).copied(),
        host_cpu_temperature_celsius: session_cpu_temperatures.get(session_id).copied(),
        default_reasoning_level,
        has_merge_conflict,
        markdown_render_cache,
        mode,
        output_layout_cache,
        review_text: review_snapshot
            .filter(|snapshot| snapshot.session_id == session_id)
            .and_then(|snapshot| snapshot.text),
        scroll_offset,
        session_index,
        session_update_version,
        sessions,
        frame_time,
    };
    let can_open_worktree = is_tmux_session
        && *session_worktree_availability
            .get(session_id)
            .unwrap_or(&false);
    if sessions[session_index].role == crate::domain::session::SessionRole::Orchestrator {
        page::orchestration::OrchestrationPage::new(page_input)
            .can_open_worktree(can_open_worktree)
            .render(f, area);
    } else {
        page::session_chat::SessionChatPage::new(page_input)
            .can_open_worktree(can_open_worktree)
            .render(f, area);
    }
}

/// Renders base list tabs and the currently selected list tab content.
fn render_list_background(
    f: &mut Frame,
    content_area: Rect,
    shared: &mut RouteSharedContext<'_>,
    frame_time: FrameTime,
) {
    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    component::tab::Tabs::new(
        shared.current_tab,
        shared.active_project_id,
        shared.projects,
    )
    .render(f, chunks[0]);

    match shared.current_tab {
        Tab::Projects => {
            page::project_list::ProjectListPage::new(
                shared.projects,
                shared.available_agent_clis,
                shared.stats_activity,
                &mut *shared.project_table_state,
                shared.active_project_id,
                activity_day_key_with_offset(
                    frame_time.unix_seconds(),
                    frame_time.local_utc_offset_seconds(),
                ),
            )
            .render(f, chunks[1]);
        }
        Tab::Sessions => {
            page::session_list::SessionListPage::new(
                shared.sessions,
                &mut *shared.table_state,
                shared.default_reasoning_level,
                frame_time.unix_seconds(),
            )
            .session_git_statuses(shared.session_git_statuses)
            .render(f, chunks[1]);
        }
        Tab::Settings => {
            let active_project_name =
                active_project_name(shared.active_project_id, shared.projects);
            if let Some(settings_screen) = shared.settings_screen {
                page::setting::SettingsPage::new(settings_screen, active_project_name)
                    .render(f, chunks[1]);
            }
        }
    }
}

/// Returns the active project's display label for scoped page titles.
fn active_project_name(active_project_id: i64, projects: &[ProjectListItem]) -> Option<String> {
    projects
        .iter()
        .find(|project_item| project_item.project.id == active_project_id)
        .map(|project_item| project_item.project.display_label())
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
