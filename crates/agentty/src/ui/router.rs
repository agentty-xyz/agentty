//! Content-area router that maps the current `AppMode` to a page surface and
//! its overlays.

use std::cell::Cell;
use std::collections::HashMap;

use ag_contracts::TurnUsage;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::Tab;
use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::{AgentCliInfo, AgentKind, AgentModel, ReasoningLevel};
use crate::domain::harness::HarnessAvailability;
use crate::domain::project::{ProjectListItem, ordered_project_items};
use crate::domain::resource::SessionResources;
use crate::domain::session::{DailyActivity, Session, SessionId, activity_day_key_with_offset};
use crate::domain::session_usage::{LiveUsageKey, SessionUsage};
use crate::presentation::api_key::{API_KEY_TITLE, ApiKeyDialog};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget,
    DiffReviewComments, DiffSidebarFocus, HelpContext, PromptModeSnapshot, UsagePanel,
    allows_diff_line_comment_reply,
};
use crate::presentation::chatgpt_sign_in::{
    CHATGPT_SIGN_IN_LOADING_LABEL, CHATGPT_SIGN_IN_TITLE, ChatGptSignInDialog,
};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::presentation::platform::PlatformTabView;
use crate::presentation::remote_control::RemoteControlView;
use crate::presentation::setting::SettingsScreenSnapshot;
use crate::ui::component::confirmation_overlay::ConfirmationOverlay;
use crate::ui::component::diff_comment_submit_overlay::DiffCommentSubmitOverlay;
use crate::ui::component::info_overlay::InfoOverlay;
use crate::ui::component::project_catalog_overlay::ProjectCatalogOverlay;
use crate::ui::usage_readout::UsageReadout;
use crate::ui::{
    Component, Page, RenderContext, SessionReviewSnapshot, component, markdown, overlay, page,
    style, usage_format,
};

/// Shared mutable routing data reused across app modes in `route_frame`.
struct RouteSharedContext<'a> {
    /// Identifier for the active project shared across list-mode renders.
    active_project_id: i64,
    archived_session_count: usize,
    /// Locally available agent CLI executables and detected versions.
    available_agent_clis: &'a [AgentCliInfo],
    /// Startup-validated agent CLIs that can run new sessions.
    available_agent_kinds: &'a [AgentKind],
    current_tab: Tab,
    default_reasoning_level: ReasoningLevel,
    /// Whether the in-process harness can run new sessions, and on which
    /// model.
    harness_availability: HarnessAvailability,
    has_more_archived_sessions: bool,
    is_archive_expanded: bool,
    /// Cached most-recently-opened ordering over `projects`.
    mru_project_order: &'a [usize],
    /// Platform sign-in and remote-control state for the Platform tab,
    /// when this launch offers it.
    platform: Option<PlatformTabView<'a>>,
    /// Latest Agentty process-tree totals shown in the tab header.
    process_resources: Option<SessionResources>,
    project_table_state: &'a mut TableState,
    projects: &'a [ProjectListItem],
    /// Remote-control listener and paired-device state for the Remote tab,
    /// when this launch offers it.
    remote_control: Option<&'a RemoteControlView>,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    /// Cumulative usage of each session's running turn, folded into the
    /// usage panel.
    session_live_usage: &'a HashMap<LiveUsageKey, TurnUsage>,
    sessions: &'a [Session],
    settings_screen: Option<&'a SettingsScreenSnapshot>,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
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
        scroll_offset: Option<usize>,
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
    color_support: style::ColorSupport,
    frame_time: FrameTime,
    graph_chat: Option<&'a GraphChatSnapshot>,
    greeting_animated: &'a Cell<bool>,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    mode: &'a AppMode,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    scroll_offset: Option<usize>,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<SessionId, String>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    sessions: &'a [Session],
    usage: UsageSources<'a>,
}

/// Usage the live readout reads for the visible session.
#[derive(Clone, Copy)]
struct UsageSources<'a> {
    /// Cumulative usage of each running turn.
    live: &'a HashMap<LiveUsageKey, TurnUsage>,
    /// Listed harness models a local server runs.
    local_models: &'a [AgentModel],
    /// Recorded usage of each session the readout has shown.
    totals: &'a HashMap<SessionId, SessionUsage>,
}

impl UsageSources<'_> {
    /// Returns the live readout of `session`, or `None` before it records
    /// any usage.
    fn readout(self, session: &Session) -> Option<UsageReadout> {
        let figures = usage_format::session_readout_figures(
            self.totals.get(&session.id),
            &session.id,
            self.live,
        )?;

        Some(UsageReadout::new(
            figures,
            self.local_models.contains(&session.agent.model()),
        ))
    }
}

/// UI-owned immutable resources shared by every surface in one frame.
///
/// Cache handles and session-derived display snapshots stay bundled until the
/// selected surface projects the narrower page input it needs.
#[derive(Clone, Copy)]
struct FrameResources<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    /// Whether the terminal renders colors.
    color_support: style::ColorSupport,
    diff_layout_cache: &'a page::diff::DiffLayoutCache,
    frame_time: FrameTime,
    graph_chat: Option<&'a GraphChatSnapshot>,
    /// Records whether the drawn session chat painted an animating nerpa
    /// greeting.
    greeting_animated: &'a Cell<bool>,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    session_progress_messages: &'a HashMap<SessionId, String>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    usage: UsageSources<'a>,
}

impl<'a> FrameResources<'a> {
    /// Creates a session chat render context from shared route inputs and the
    /// mode-specific session selection.
    fn session_chat<'b>(
        self,
        sessions: &'b [Session],
        mode: &'b AppMode,
        session_id: &'b str,
        scroll_offset: Option<usize>,
    ) -> SessionChatRenderContext<'b>
    where
        'a: 'b,
    {
        SessionChatRenderContext {
            graph_chat: self.graph_chat,
            greeting_animated: self.greeting_animated,
            active_prompt_outputs: self.active_prompt_outputs,
            color_support: self.color_support,
            markdown_render_cache: self.markdown_render_cache,
            mode,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            session_id,
            session_git_statuses: self.session_git_statuses,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            sessions,
            scroll_offset,
            frame_time: self.frame_time,
            usage: self.usage,
        }
    }
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(frame: &mut Frame, area: Rect, context: RenderContext<'_>) {
    if let AppMode::ExecutionGraph(view) = context.mode {
        page::execution_graph::render(
            frame,
            area,
            view,
            context.render_cache_store.markdown_render_cache(),
        );

        return;
    }
    let RenderContext {
        graph_chat,
        active_project_id,
        archived_session_count,
        has_more_archived_sessions,
        is_archive_expanded,
        active_prompt_outputs,
        available_agent_clis,
        available_agent_kinds,
        color_support,
        current_tab,
        default_reasoning_level,
        harness_availability,
        local_harness_models,
        mode,
        mru_project_order,
        platform,
        process_resources,
        remote_control,
        render_cache_store,
        project_table_state,
        projects,
        session_review_snapshot,
        session_git_statuses,
        session_live_usage,
        session_progress_messages,
        session_update_versions,
        session_usage_totals,
        settings_screen,
        stats_activity,
        sessions,
        table_state,
        frame_time,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        active_project_id,
        archived_session_count,
        available_agent_clis,
        available_agent_kinds,
        current_tab,
        default_reasoning_level,
        harness_availability,
        has_more_archived_sessions,
        is_archive_expanded,
        mru_project_order,
        platform,
        process_resources,
        project_table_state,
        projects,
        remote_control,
        session_git_statuses,
        session_live_usage,
        sessions,
        settings_screen,
        stats_activity,
        table_state,
    };

    let resources = FrameResources {
        graph_chat,
        active_prompt_outputs,
        color_support,
        diff_layout_cache: render_cache_store.diff_layout_cache(),
        greeting_animated: render_cache_store.greeting_animated(),
        markdown_render_cache: render_cache_store.markdown_render_cache(),
        output_layout_cache: render_cache_store.session_output_layout_cache(),
        review_snapshot: session_review_snapshot,
        session_git_statuses,
        session_progress_messages,
        session_update_versions,
        frame_time,
        usage: UsageSources {
            live: session_live_usage,
            local_models: local_harness_models,
            totals: session_usage_totals,
        },
    };

    render_surface(frame, area, surface_for_mode(mode), &mut shared, resources);
    render_mode_overlay(frame, area, mode, &shared, resources);
}

/// Resolves the stable base-page identity for terminal transition handling.
pub(crate) fn surface_kind_for_mode(mode: &AppMode) -> SurfaceKind {
    surface_for_mode(mode).kind()
}

/// Returns the active project's display label for the top status bar and
/// scoped page titles.
pub(crate) fn active_project_name(
    active_project_id: i64,
    projects: &[ProjectListItem],
) -> Option<String> {
    projects
        .iter()
        .find(|project_item| project_item.project.id == active_project_id)
        .map(|project_item| project_item.project.display_label())
}

/// Resolves the base page painted for every mode before any overlay.
fn surface_for_mode(mode: &AppMode) -> Surface<'_> {
    match mode {
        AppMode::ExecutionGraph(view) => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: view.restore.scroll_offset,
            session_id: &view.restore.session_id,
        },
        AppMode::Confirmation {
            confirmation_intent:
                ConfirmationIntent::ContinueSession
                | ConfirmationIntent::ForkSession
                | ConfirmationIntent::MergeSession
                | ConfirmationIntent::RegenerateReview
                | ConfirmationIntent::WatchPublishedReviewRequest
                | ConfirmationIntent::WatchReviewRequest,
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
        AppMode::ChatGptSignIn { dialog } => composer_popup_surface(&dialog.restore),
        AppMode::ApiKeyInput { dialog } => composer_popup_surface(&dialog.restore),
        AppMode::List
        | AppMode::SessionCreation { .. }
        | AppMode::PreCommitHookWarning { .. }
        | AppMode::ProjectCatalog { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Confirmation { .. } => Surface::List,
        AppMode::Help { context, .. } => surface_for_help_context(context),
        AppMode::Usage(panel) => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: panel.restore.scroll_offset(),
            session_id: panel.restore.session_id(),
        },
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

/// Resolves the session page painted behind a popup opened from the
/// composer captured in `restore`.
fn composer_popup_surface(restore: &PromptModeSnapshot) -> Surface<'_> {
    Surface::Session {
        mode: SessionSurfaceMode::View,
        scroll_offset: restore.scroll_offset,
        session_id: &restore.session_id,
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
    frame: &mut Frame,
    area: Rect,
    surface: Surface<'_>,
    shared: &mut RouteSharedContext<'_>,
    resources: FrameResources<'_>,
) {
    match surface {
        Surface::List => render_list_background(frame, area, shared, resources.frame_time),
        Surface::Session {
            mode,
            scroll_offset,
            session_id,
        } => render_session_surface(
            frame,
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
                frame,
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
            frame,
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
    frame: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &RouteSharedContext<'_>,
    resources: FrameResources<'_>,
) {
    match mode {
        AppMode::List
        | AppMode::ExecutionGraph(_)
        | AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::DiffLoading { .. } => {}
        AppMode::Diff { line_comments, .. } => {
            render_diff_comment_submit_overlay(frame, area, line_comments);
        }
        AppMode::ProjectCatalog { dialog } => ProjectCatalogOverlay { dialog }.render(frame, area),
        AppMode::SessionCreation {
            selected_option_index,
        } => component::session_creation_overlay::SessionCreationOverlay::new(
            *selected_option_index,
            !shared.available_agent_kinds.is_empty() || shared.harness_availability.is_available(),
        )
        .render(frame, area),
        AppMode::PreCommitHookWarning { message } => {
            InfoOverlay::new("Pre-commit hook warning", message).render(frame, area);
        }
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => render_project_switcher_overlay(frame, area, shared, *selected_option_index),
        AppMode::Confirmation {
            confirmation_message,
            confirmation_title,
            selected_confirmation_index,
            ..
        } => ConfirmationOverlay::new(confirmation_title, confirmation_message)
            .selected_first(*selected_confirmation_index == 0)
            .render(frame, area),
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
            let popup = InfoOverlay::new(title, &popup_message)
                .is_loading(*is_loading)
                .loading_label("Sync in progress...");
            render_information_overlay(frame, area, popup, resources.frame_time);
        }
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            title,
            ..
        } => {
            let popup = InfoOverlay::new(title, message)
                .is_loading(*is_loading)
                .loading_label(loading_label);
            render_information_overlay(frame, area, popup, resources.frame_time);
        }
        AppMode::ChatGptSignIn { dialog } => {
            render_chatgpt_sign_in_overlay(frame, area, dialog, resources.frame_time);
        }
        AppMode::ApiKeyInput { dialog } => {
            render_api_key_overlay(frame, area, dialog, resources.frame_time);
        }
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => component::help_overlay::HelpOverlay::new(help_context)
            .scroll_offset(*scroll_offset)
            .render(frame, area),
        AppMode::Usage(panel) => render_usage_overlay(frame, area, shared, panel),
        AppMode::LaunchConfigurationSelector {
            commands,
            selected_command_index,
            ..
        } => component::launch_configuration_overlay::LaunchConfigurationOverlay::new(commands)
            .selected_command_index(*selected_command_index)
            .render(frame, area),
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
        .render(frame, area),
    }
}

/// Renders the usage panel: the recorded usage with the session's running
/// turns folded in, titled with the session's display title.
fn render_usage_overlay(
    frame: &mut Frame,
    area: Rect,
    shared: &RouteSharedContext<'_>,
    panel: &UsagePanel,
) {
    let session_id = panel.restore.session_id();
    let content =
        usage_format::session_panel_content(&panel.usage, session_id, shared.session_live_usage);
    let session_title = shared
        .sessions
        .iter()
        .find(|session| session.id == *session_id)
        .map_or("", Session::display_title);

    component::usage_overlay::UsageOverlay::new(&content, session_title)
        .scroll_offset(panel.scroll_offset)
        .render(frame, area);
}

/// Previews pending diff comments while the leave-diff confirmation is open.
fn render_diff_comment_submit_overlay(
    frame: &mut Frame,
    area: Rect,
    line_comments: &DiffLineComments,
) {
    let Some(selected_index) = line_comments.submit_confirmation_index else {
        return;
    };
    let preview = line_comments.prompt_text();

    DiffCommentSubmitOverlay::new(&preview, selected_index == 0).render(frame, area);
}

/// Shares the frame's spinner phase between informational overlays.
fn render_information_overlay(
    frame: &mut Frame,
    area: Rect,
    popup: InfoOverlay<'_>,
    frame_time: FrameTime,
) {
    popup
        .spinner_frame(crate::ui::icon::Icon::spinner_frame_from_millis(
            frame_time.unix_millis(),
        ))
        .render(frame, area);
}

/// Renders the `ChatGPT` sign-in popup for its current step.
fn render_chatgpt_sign_in_overlay(
    frame: &mut Frame,
    area: Rect,
    dialog: &ChatGptSignInDialog,
    frame_time: FrameTime,
) {
    let message = dialog.message();
    let popup = InfoOverlay::new(CHATGPT_SIGN_IN_TITLE, &message)
        .is_loading(dialog.is_loading())
        .loading_label(CHATGPT_SIGN_IN_LOADING_LABEL)
        .action_label(dialog.action_label());
    render_information_overlay(frame, area, popup, frame_time);
}

/// Renders the API key popup: the masked key field while editing, then the
/// saved outcome.
fn render_api_key_overlay(
    frame: &mut Frame,
    area: Rect,
    dialog: &ApiKeyDialog,
    frame_time: FrameTime,
) {
    let message = dialog.message();
    if dialog.is_editing() {
        let masked_input = dialog.masked_input();
        component::api_key_overlay::ApiKeyOverlay::new(
            &message,
            &masked_input,
            dialog.input.cursor,
        )
        .render(frame, area);

        return;
    }
    let popup = InfoOverlay::new(API_KEY_TITLE, &message).action_label("OK");
    render_information_overlay(frame, area, popup, frame_time);
}

/// Renders the project switcher with projects in most-recently-used order.
fn render_project_switcher_overlay(
    frame: &mut Frame,
    area: Rect,
    shared: &RouteSharedContext<'_>,
    selected_option_index: usize,
) {
    let mru_project_items = ordered_project_items(shared.projects, shared.mru_project_order);
    component::project_switcher_overlay::ProjectSwitcherOverlay::new(
        &mru_project_items,
        shared.active_project_id,
        selected_option_index,
    )
    .render(frame, area);
}

/// Renders a session surface in either interactive or restored-view mode.
fn render_session_surface(
    frame: &mut Frame,
    area: Rect,
    mode: SessionSurfaceMode<'_>,
    scroll_offset: Option<usize>,
    session_id: &str,
    sessions: &[Session],
    resources: FrameResources<'_>,
) {
    match mode {
        SessionSurfaceMode::Interactive(mode) => render_session_chat(
            frame,
            area,
            resources.session_chat(sessions, mode, session_id, scroll_offset),
        ),
        SessionSurfaceMode::View => {
            let view_mode = AppMode::View {
                scroll_offset,
                session_id: session_id.into(),
            };
            render_session_chat(
                frame,
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
    frame: &mut Frame,
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
        can_comment: allows_diff_line_comment_reply(session, input.restore),
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
        page.loading().render(frame, area);
    } else {
        page.render(frame, area);
    }
}

/// Renders the session chat page for all session-chat modes.
fn render_session_chat(frame: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        graph_chat,
        greeting_animated,
        active_prompt_outputs,
        color_support,
        markdown_render_cache,
        mode,
        output_layout_cache,
        review_snapshot,
        session_id,
        session_progress_messages,
        session_git_statuses,
        session_update_versions,
        sessions,
        scroll_offset,
        frame_time,
        usage,
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
        color_support,
        greeting_animated,
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
    let usage_readout = usage.readout(&sessions[session_index]);

    page::session_chat::SessionChatPage::new(page_input)
        .graph_chat(graph_chat.filter(|snapshot| snapshot.proposal.input.session_id == session_id))
        .usage_readout(usage_readout.as_ref())
        .render(frame, area);
}

/// Renders base list tabs and the currently selected list tab content.
fn render_list_background(
    frame: &mut Frame,
    content_area: Rect,
    shared: &mut RouteSharedContext<'_>,
    frame_time: FrameTime,
) {
    let chunks = Layout::default()
        .constraints([
            Constraint::Length(component::tab::Tabs::HEIGHT),
            Constraint::Min(0),
        ])
        .split(content_area);

    component::tab::Tabs::new(shared.current_tab)
        .process_stats(shared.process_resources)
        .remote_label(shared.remote_control.map(RemoteControlView::tab_label))
        .platform(shared.platform.is_some())
        .render(frame, chunks[0]);

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
            .render(frame, chunks[1]);
        }
        Tab::Sessions => {
            page::session_list::SessionListPage::new(
                shared.sessions,
                &mut *shared.table_state,
                shared.default_reasoning_level,
                frame_time.unix_seconds(),
            )
            .archived_session_count(shared.archived_session_count)
            .has_more_archived_sessions(shared.has_more_archived_sessions)
            .is_archive_expanded(shared.is_archive_expanded)
            .session_git_statuses(shared.session_git_statuses)
            .render(frame, chunks[1]);
        }
        Tab::Settings => {
            let active_project_name =
                active_project_name(shared.active_project_id, shared.projects);
            if let Some(settings_screen) = shared.settings_screen {
                page::setting::SettingsPage::new(settings_screen, active_project_name)
                    .render(frame, chunks[1]);
            }
        }
        Tab::Remote => {
            if let Some(remote_control) = shared.remote_control {
                page::remote_control::RemoteControlPage::new(remote_control)
                    .render(frame, chunks[1]);
            }
        }
        Tab::Platform => {
            if let Some(platform) = shared.platform {
                page::platform::PlatformPage::new(platform).render(frame, chunks[1]);
            }
        }
    }
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
