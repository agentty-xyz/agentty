use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::{AssignedIssueState, RequestedReviewState, SettingsManager, Tab};
use crate::domain::agent::{AgentCliInfo, ReasoningLevel};
use crate::domain::input::InputState;
use crate::domain::project::{ProjectListItem, ordered_project_items};
use crate::domain::session::{DailyActivity, Session, SessionId};
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::ui::overlay::{
    HelpOverlayRenderContext, SyncBlockedPopupRenderContext, ViewInfoPopupRenderContext,
};
use crate::ui::{
    Component, Page, RenderContext, SessionReviewSnapshot, component, markdown, overlay, page,
};

/// Borrowed list-background view into shared route state.
pub(crate) struct ListBackgroundRenderContext<'a, 'state> {
    /// Shared route data borrowed while a list-backed page or overlay renders.
    shared: &'a mut RouteSharedContext<'state>,
}

impl<'state> ListBackgroundRenderContext<'_, 'state> {
    /// Returns the active project identifier for list-backed overlays.
    pub(crate) fn active_project_id(&self) -> i64 {
        self.shared.active_project_id
    }

    /// Returns the project rows in most-recently-opened order.
    ///
    /// The order itself is cached by the app layer, so this only maps the
    /// cached indices onto the shared rows.
    pub(crate) fn mru_projects(&self) -> Vec<&'state ProjectListItem> {
        ordered_project_items(self.shared.projects, self.shared.mru_project_order)
    }

    /// Returns whether the session-creation overlay can offer a stacked
    /// session option for the currently selected session row.
    pub(crate) fn can_create_stacked_session(&self) -> bool {
        self.shared.current_tab == Tab::Sessions
            && self
                .shared
                .table_state
                .selected()
                .and_then(|selected_index| self.shared.sessions.get(selected_index))
                .is_some_and(Session::allows_stacked_child_creation)
    }

    /// Returns the active project-scoped reasoning level for list-restored
    /// session backgrounds.
    pub(crate) fn default_reasoning_level(&self) -> ReasoningLevel {
        self.shared.settings.reasoning_level
    }

    /// Returns the shared session rows available to list-backed overlays.
    pub(crate) fn sessions(&self) -> &[Session] {
        self.shared.sessions
    }
}

/// Shared mutable routing data reused across app modes in `route_frame`.
pub(crate) struct RouteSharedContext<'a> {
    /// Identifier for the active project shared across list-mode renders.
    active_project_id: i64,
    assigned_issue_selected_index: Option<usize>,
    assigned_issue_table_state: &'a mut TableState,
    assigned_issues: &'a AssignedIssueState,
    /// Locally available agent CLI executables and detected versions.
    available_agent_clis: &'a [AgentCliInfo],
    current_tab: Tab,
    /// Cached most-recently-opened ordering over `projects`.
    mru_project_order: &'a [usize],
    project_table_state: &'a mut TableState,
    projects: &'a [ProjectListItem],
    requested_review_selected_index: Option<usize>,
    requested_review_table_state: &'a mut TableState,
    requested_reviews: &'a RequestedReviewState,
    sessions: &'a [Session],
    settings: &'a SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl<'state> RouteSharedContext<'state> {
    /// Creates a list-background context for overlays/pages that render on top
    /// of the tabbed list content without repacking shared route fields.
    fn list_background(&mut self) -> ListBackgroundRenderContext<'_, 'state> {
        ListBackgroundRenderContext { shared: self }
    }
}

/// Borrowed inputs for rendering a session chat page.
#[derive(Clone, Copy)]
struct SessionChatRenderContext<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    mode: &'a AppMode,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<SessionId, String>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    sessions: &'a [Session],
    scroll_offset: Option<u16>,
    wall_clock_unix_seconds: i64,
}

/// Borrowed inputs for rendering the publish-branch overlay and its
/// background session view.
#[derive(Clone, Copy)]
struct PublishBranchOverlayContext<'a> {
    default_branch_name: &'a str,
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    input: &'a InputState,
    locked_upstream_ref: Option<&'a str>,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    restore_view: &'a ConfirmationViewMode,
    session_progress_messages: &'a HashMap<SessionId, String>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    sessions: &'a [Session],
}

/// Shared immutable routing inputs that are not part of list-background state.
#[derive(Clone, Copy)]
struct RouteAuxContext<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    diff_layout_cache: &'a page::diff::DiffLayoutCache,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    session_progress_messages: &'a HashMap<SessionId, String>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    wall_clock_unix_seconds: i64,
}

impl<'a> RouteAuxContext<'a> {
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
            markdown_render_cache: self.markdown_render_cache,
            mode,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            session_id,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions,
            scroll_offset,
            wall_clock_unix_seconds: self.wall_clock_unix_seconds,
        }
    }

    /// Creates a session-overlay render context from shared route inputs and
    /// the view restored behind the overlay.
    fn session_overlay<'b>(
        self,
        sessions: &'b [Session],
        restore_view: &'b ConfirmationViewMode,
    ) -> SessionOverlayRenderContext<'b>
    where
        'a: 'b,
    {
        SessionOverlayRenderContext {
            active_prompt_outputs: self.active_prompt_outputs,
            default_reasoning_level: self.default_reasoning_level,
            markdown_render_cache: self.markdown_render_cache,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            restore_view,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions,
            wall_clock_unix_seconds: self.wall_clock_unix_seconds,
        }
    }

    /// Creates the publish-branch overlay context by combining shared route
    /// inputs with publish-mode values.
    fn publish_branch_overlay<'b>(
        self,
        sessions: &'b [Session],
        mode_context: PublishBranchModeContext<'b>,
    ) -> PublishBranchOverlayContext<'b>
    where
        'a: 'b,
    {
        PublishBranchOverlayContext {
            default_branch_name: mode_context.default_branch_name,
            active_prompt_outputs: self.active_prompt_outputs,
            default_reasoning_level: self.default_reasoning_level,
            markdown_render_cache: self.markdown_render_cache,
            output_layout_cache: self.output_layout_cache,
            input: mode_context.input,
            locked_upstream_ref: mode_context.locked_upstream_ref,
            review_snapshot: self.review_snapshot,
            restore_view: mode_context.restore_view,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions,
        }
    }
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        assigned_issue_selected_index,
        assigned_issue_table_state,
        assigned_issues,
        active_prompt_outputs,
        available_agent_clis,
        current_tab,
        diff_layout_cache,
        markdown_render_cache,
        mode,
        mru_project_order,
        output_layout_cache,
        project_table_state,
        projects,
        requested_review_selected_index,
        requested_review_table_state,
        requested_reviews,
        session_review_snapshot,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        settings,
        stats_activity,
        sessions,
        table_state,
        wall_clock_unix_seconds,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        active_project_id,
        assigned_issue_selected_index,
        assigned_issue_table_state,
        assigned_issues,
        available_agent_clis,
        current_tab,
        mru_project_order,
        project_table_state,
        projects,
        requested_review_selected_index,
        requested_review_table_state,
        requested_reviews,
        sessions,
        settings,
        stats_activity,
        table_state,
    };

    let aux = RouteAuxContext {
        active_prompt_outputs,
        default_reasoning_level: shared.settings.reasoning_level,
        diff_layout_cache,
        markdown_render_cache,
        output_layout_cache,
        review_snapshot: session_review_snapshot,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        wall_clock_unix_seconds,
    };

    if render_list_or_overlay_mode(f, area, mode, &mut shared, aux) {
        return;
    }

    render_session_or_diff_mode(f, area, mode, shared.sessions, aux);
}

/// Renders all list/overlay-driven modes and returns whether it handled `mode`.
fn render_list_or_overlay_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &mut RouteSharedContext<'_>,
    aux: RouteAuxContext<'_>,
) -> bool {
    match mode {
        AppMode::List => render_list_background(
            f,
            area,
            shared.list_background(),
            aux.wall_clock_unix_seconds,
        ),
        AppMode::SessionCreation {
            selected_option_index,
        } => overlay::render_session_creation_overlay(
            f,
            area,
            shared.list_background(),
            *selected_option_index,
            aux.wall_clock_unix_seconds,
        ),
        AppMode::PreCommitHookWarning { message } => overlay::render_pre_commit_hook_warning(
            f,
            area,
            shared.list_background(),
            message,
            aux.wall_clock_unix_seconds,
        ),
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => overlay::render_project_switcher_overlay(
            f,
            area,
            shared.list_background(),
            *selected_option_index,
            aux.wall_clock_unix_seconds,
        ),
        AppMode::Confirmation { .. } => render_confirmation_mode(f, area, mode, shared, aux),

        AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } => overlay::render_sync_blocked_popup(
            f,
            area,
            shared.list_background(),
            aux.wall_clock_unix_seconds,
            SyncBlockedPopupRenderContext {
                default_branch: default_branch.as_deref(),
                is_loading: *is_loading,
                message,
                project_name: project_name.as_deref(),
                title,
            },
        ),
        AppMode::ViewInfoPopup { .. } => {
            render_view_info_popup_mode(f, area, mode, shared.sessions, aux);
        }
        AppMode::IssueDetail { .. } | AppMode::ReviewDetail { .. } => {
            render_detail_mode(f, area, mode, aux.markdown_render_cache);
        }
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => overlay::render_help(
            f,
            area,
            HelpOverlayRenderContext {
                diff_layout_cache: aux.diff_layout_cache,
                help_context,
                list_background: shared.list_background(),
                markdown_render_cache: aux.markdown_render_cache,
                output_layout_cache: aux.output_layout_cache,
                review_snapshot: aux.review_snapshot,
                scroll_offset: *scroll_offset,
                session_progress_messages: aux.session_progress_messages,
                session_update_versions: aux.session_update_versions,
                wall_clock_unix_seconds: aux.wall_clock_unix_seconds,
            },
        ),
        AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::PublishBranchInput { .. }
        | AppMode::LaunchConfigurationSelector { .. }
        | AppMode::Diff { .. }
        | AppMode::ReviewComments { .. } => {
            return false;
        }
    }

    true
}

/// Routes issue and requested-review snapshots to their detail page renderers.
fn render_detail_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    markdown_render_cache: &markdown::MarkdownRenderCache,
) {
    match mode {
        AppMode::IssueDetail {
            detail,
            error,
            issue,
            scroll_offset,
        } => page::issue_detail::IssueDetailPage::new(
            issue,
            detail.as_ref(),
            error.as_deref(),
            markdown_render_cache,
            *scroll_offset,
        )
        .render(f, area),
        AppMode::ReviewDetail {
            comment_error,
            is_loading_comments,
            review,
            scroll_offset,
        } => page::review_detail::ReviewDetailPage::new(
            review,
            markdown_render_cache,
            *scroll_offset,
        )
        .with_comment_status(comment_error.as_deref(), *is_loading_comments)
        .render(f, area),
        _ => {}
    }
}

/// Renders confirmation modes, including session-scoped confirmations that
/// preserve the originating chat background.
fn render_confirmation_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &mut RouteSharedContext<'_>,
    aux: RouteAuxContext<'_>,
) {
    let AppMode::Confirmation {
        confirmation_intent,
        confirmation_message,
        confirmation_title,
        restore_view,
        selected_confirmation_index,
        ..
    } = mode
    else {
        return;
    };

    if matches!(
        confirmation_intent,
        ConfirmationIntent::ContinueSession
            | ConfirmationIntent::ForkSession
            | ConfirmationIntent::MergeSession
            | ConfirmationIntent::RegenerateReview
    ) && let Some(view_mode) = restore_view
    {
        render_session_confirmation_overlay(
            f,
            area,
            aux.session_overlay(shared.sessions, view_mode),
            &SessionConfirmationContext {
                confirmation_message,
                confirmation_title,
                selected_confirmation_index: *selected_confirmation_index,
            },
        );

        return;
    }

    overlay::render_confirmation_overlay(
        f,
        area,
        mode,
        shared.list_background(),
        aux.wall_clock_unix_seconds,
    );
}

/// Renders an informational popup above a restored session-view background.
fn render_view_info_popup_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    sessions: &[Session],
    aux: RouteAuxContext<'_>,
) {
    let AppMode::ViewInfoPopup {
        is_loading,
        loading_label,
        message,
        restore_view,
        title,
    } = mode
    else {
        return;
    };

    overlay::render_view_info_popup(
        f,
        area,
        ViewInfoPopupRenderContext {
            can_open_worktree: *aux
                .session_worktree_availability
                .get(&restore_view.session_id)
                .unwrap_or(&false),
            default_reasoning_level: aux.default_reasoning_level,
            markdown_render_cache: aux.markdown_render_cache,
            output_layout_cache: aux.output_layout_cache,
            is_loading: *is_loading,
            loading_label,
            message,
            review_snapshot: aux.review_snapshot,
            restore_view,
            session_progress_messages: aux.session_progress_messages,
            session_update_versions: aux.session_update_versions,
            sessions,
            title,
            wall_clock_unix_seconds: aux.wall_clock_unix_seconds,
        },
    );
}

/// Borrowed context for the confirmation overlay portion of a session-scoped
/// confirmation render (merge, regenerate focused review).
struct SessionConfirmationContext<'a> {
    /// The body text displayed inside the confirmation dialog.
    confirmation_message: &'a str,
    /// The header title of the confirmation dialog.
    confirmation_title: &'a str,
    /// Index of the currently highlighted confirmation option.
    selected_confirmation_index: usize,
}

/// Borrowed data shared by session-scoped overlays that render above the
/// session chat page.
#[derive(Clone, Copy)]
struct SessionOverlayRenderContext<'a> {
    /// Exact prompt transcript blocks keyed by session id for active turns.
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    /// Active project-scoped default reasoning level.
    default_reasoning_level: ReasoningLevel,
    /// Shared render cache for session transcript markdown.
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Shared output-layout cache for the restored session transcript.
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    /// Focused-review state for the restored session background.
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    /// Session view restored after the overlay closes.
    restore_view: &'a ConfirmationViewMode,
    /// Active progress messages keyed by session id.
    session_progress_messages: &'a HashMap<SessionId, String>,
    /// Latest observable update versions keyed by session id.
    session_update_versions: &'a HashMap<SessionId, u64>,
    /// Whether each background session currently has a materialized
    /// worktree, keyed by session id.
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    /// Session rows available for background rendering.
    sessions: &'a [Session],
    /// Render-time clock used for deterministic timers.
    wall_clock_unix_seconds: i64,
}

impl SessionOverlayRenderContext<'_> {
    /// Creates the session-chat context used to render the restored background
    /// page behind a session-scoped overlay.
    fn session_chat<'a>(&'a self, background_mode: &'a AppMode) -> SessionChatRenderContext<'a> {
        SessionChatRenderContext {
            active_prompt_outputs: self.active_prompt_outputs,
            default_reasoning_level: self.default_reasoning_level,
            markdown_render_cache: self.markdown_render_cache,
            mode: background_mode,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            session_id: &self.restore_view.session_id,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions: self.sessions,
            scroll_offset: self.restore_view.scroll_offset,
            wall_clock_unix_seconds: self.wall_clock_unix_seconds,
        }
    }
}

impl<'context> PublishBranchOverlayContext<'context> {
    /// Creates the session-overlay context that restores the chat page behind
    /// the publish-branch prompt.
    fn session_overlay(
        self,
        wall_clock_unix_seconds: i64,
    ) -> SessionOverlayRenderContext<'context> {
        SessionOverlayRenderContext {
            active_prompt_outputs: self.active_prompt_outputs,
            default_reasoning_level: self.default_reasoning_level,
            markdown_render_cache: self.markdown_render_cache,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            restore_view: self.restore_view,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions: self.sessions,
            wall_clock_unix_seconds,
        }
    }
}

/// Renders the shared session-chat background for session-scoped overlays and
/// dims it with the generic overlay backdrop.
fn render_session_overlay_background(
    f: &mut Frame,
    area: Rect,
    context: SessionOverlayRenderContext<'_>,
) {
    let background_mode = context.restore_view.clone().into_view_mode();

    render_session_chat(f, area, context.session_chat(&background_mode));
}

/// Renders a session-scoped confirmation above the originating session chat
/// page.
fn render_session_confirmation_overlay(
    f: &mut Frame,
    area: Rect,
    overlay_context: SessionOverlayRenderContext<'_>,
    confirmation_context: &SessionConfirmationContext<'_>,
) {
    render_session_overlay_background(f, area, overlay_context);

    component::confirmation_overlay::ConfirmationOverlay::new(
        confirmation_context.confirmation_title,
        confirmation_context.confirmation_message,
    )
    .selected_yes(confirmation_context.selected_confirmation_index == 0)
    .render(f, area);
}

/// Renders session-scoped modes tied to one selected session.
fn render_session_or_diff_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    sessions: &[Session],
    aux: RouteAuxContext<'_>,
) {
    match mode {
        AppMode::View {
            session_id,
            scroll_offset,
            ..
        }
        | AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        }
        | AppMode::Question {
            session_id,
            scroll_offset,
            ..
        } => render_session_chat(
            f,
            area,
            aux.session_chat(sessions, mode, session_id, *scroll_offset),
        ),
        AppMode::LaunchConfigurationSelector {
            commands,
            restore_view,
            selected_command_index,
        } => render_launch_configuration_selector_overlay(
            f,
            area,
            aux.session_overlay(sessions, restore_view),
            commands,
            *selected_command_index,
        ),
        AppMode::PublishBranchInput {
            default_branch_name,
            input,
            locked_upstream_ref,
            restore_view,
            ..
        } => render_publish_branch_input_mode(
            f,
            area,
            PublishBranchModeContext {
                default_branch_name,
                input,
                locked_upstream_ref: locked_upstream_ref.as_deref(),
                restore_view,
            },
            sessions,
            aux,
        ),
        AppMode::Diff {
            diff,
            file_explorer_selected_index,
            restore_question: _,
            scroll_cache: _,
            scroll_offset,
            session_id,
        } => {
            if let Some(session) = sessions.iter().find(|session| &session.id == session_id) {
                page::diff::DiffPage::new(page::diff::DiffPageInput {
                    diff,
                    diff_layout_cache: aux.diff_layout_cache,
                    file_explorer_selected_index: *file_explorer_selected_index,
                    scroll_offset: *scroll_offset,
                    session,
                })
                .render(f, area);
            }
        }
        AppMode::ReviewComments {
            comment_error,
            comment_snapshot,
            diff,
            is_loading_comments,
            selected_comment_index,
            session_id,
            scroll_offset,
        } => {
            if let Some(session) = sessions.iter().find(|session| &session.id == session_id) {
                page::review_comment::ReviewCommentPage::new(
                    page::review_comment::ReviewCommentPageInput {
                        comment_error: comment_error.as_deref(),
                        comment_snapshot: comment_snapshot.as_ref(),
                        diff,
                        is_loading_comments: *is_loading_comments,
                        markdown_render_cache: aux.markdown_render_cache,
                        scroll_offset: *scroll_offset,
                        selected_comment_index: *selected_comment_index,
                        session,
                    },
                )
                .render(f, area);
            }
        }
        _ => {}
    }
}

/// Renders publish-branch input mode by combining mode-specific values with
/// shared session-render routing data.
fn render_publish_branch_input_mode(
    f: &mut Frame,
    area: Rect,
    mode_context: PublishBranchModeContext<'_>,
    sessions: &[Session],
    aux: RouteAuxContext<'_>,
) {
    render_publish_branch_overlay(
        f,
        area,
        aux.publish_branch_overlay(sessions, mode_context),
        aux.wall_clock_unix_seconds,
    );
}

/// Mode-specific publish-branch values extracted from `AppMode`.
#[derive(Clone, Copy)]
struct PublishBranchModeContext<'a> {
    /// Default branch name shown in the publish prompt.
    default_branch_name: &'a str,
    /// Publish target input state.
    input: &'a InputState,
    /// Existing upstream ref that constrains the publish target, when present.
    locked_upstream_ref: Option<&'a str>,
    /// Session view restored behind the publish prompt.
    restore_view: &'a ConfirmationViewMode,
}

/// Renders launch-configuration selection overlay above the originating session
/// chat.
fn render_launch_configuration_selector_overlay(
    f: &mut Frame,
    area: Rect,
    overlay_context: SessionOverlayRenderContext<'_>,
    commands: &[String],
    selected_command_index: usize,
) {
    render_session_overlay_background(f, area, overlay_context);

    component::launch_configuration_overlay::LaunchConfigurationOverlay::new(commands)
        .selected_command_index(selected_command_index)
        .render(f, area);
}

/// Renders the publish-branch input overlay above the originating session
/// chat.
fn render_publish_branch_overlay(
    f: &mut Frame,
    area: Rect,
    context: PublishBranchOverlayContext<'_>,
    wall_clock_unix_seconds: i64,
) {
    render_session_overlay_background(f, area, context.session_overlay(wall_clock_unix_seconds));

    component::publish_branch_overlay::PublishBranchOverlay::new(
        context.input,
        context.default_branch_name,
        context.locked_upstream_ref,
    )
    .render(f, area);
}

/// Renders the session chat page for all session-chat modes.
fn render_session_chat(f: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        active_prompt_outputs,
        default_reasoning_level,
        markdown_render_cache,
        mode,
        output_layout_cache,
        review_snapshot,
        session_id,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        sessions,
        scroll_offset,
        wall_clock_unix_seconds,
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

    page::session_chat::SessionChatPage::new(page::session_chat::SessionChatPageInput {
        active_prompt_output,
        active_progress,
        default_reasoning_level,
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
        wall_clock_unix_seconds,
    })
    .can_open_worktree(
        *session_worktree_availability
            .get(session_id)
            .unwrap_or(&false),
    )
    .render(f, area);
}

/// Renders base list tabs and the currently selected list tab content.
pub(crate) fn render_list_background(
    f: &mut Frame,
    content_area: Rect,
    context: ListBackgroundRenderContext<'_, '_>,
    wall_clock_unix_seconds: i64,
) {
    let shared = context.shared;

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
            )
            .render(f, chunks[1]);
        }
        Tab::Sessions => {
            page::session_list::SessionListPage::new(
                shared.sessions,
                &mut *shared.table_state,
                shared.settings.reasoning_level,
                wall_clock_unix_seconds,
            )
            .render(f, chunks[1]);
        }
        Tab::Review => {
            page::inbox::InboxPage::new(
                shared.requested_reviews,
                shared.requested_review_selected_index,
                &mut *shared.requested_review_table_state,
            )
            .render(f, chunks[1]);
        }
        Tab::Issues => {
            page::issue_list::IssueListPage::new(
                shared.assigned_issues,
                shared.assigned_issue_selected_index,
                &mut *shared.assigned_issue_table_state,
            )
            .render(f, chunks[1]);
        }
        Tab::Settings => {
            let active_project_name =
                active_project_name(shared.active_project_id, shared.projects);
            page::setting::SettingsPage::new(shared.settings, active_project_name)
                .render(f, chunks[1]);
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
mod tests {
    use std::path::PathBuf;

    use ratatui::widgets::Paragraph;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::session::Status;
    use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
    use crate::test_support::SessionFixtureBuilder;

    /// Builds one deterministic session fixture for router render tests.
    fn session_fixture(session_id: &str) -> Session {
        let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "Captured output",
        )]);
        let mut session = SessionFixtureBuilder::new()
            .id(session_id)
            .folder(PathBuf::from(format!("/tmp/{session_id}")))
            .prompt("Prompt")
            .status(Status::Review)
            .summary(Some("Summary line for router test".to_string()))
            .title(Some("Router Session".to_string()))
            .build();
        session.transcript = Some(transcript);

        session
    }

    /// Flattens a rendered test buffer into a plain string for text assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn render_session_or_diff_mode_renders_view_session_content() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-1234";
        let sessions = vec![session_fixture(session_id)];
        let mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: None,
        };
        let progress_messages = HashMap::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_or_diff_mode(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        wall_clock_unix_seconds: 0,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Router Session"));
        assert!(text.contains("Captured output"));
    }

    #[test]
    fn render_session_or_diff_mode_keeps_background_when_session_is_missing() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mode = AppMode::View {
            session_id: "missing-session".into(),
            scroll_offset: None,
        };
        let progress_messages = HashMap::new();
        let sessions = Vec::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(Paragraph::new("sentinel"), area);
                render_session_or_diff_mode(
                    frame,
                    area,
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        wall_clock_unix_seconds: 0,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("sentinel"));
    }

    #[test]
    fn render_session_or_diff_mode_renders_diff_page_for_matching_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-diff";
        let mut session = session_fixture(session_id);
        session.title = Some("Diff Session".to_string());
        let sessions = vec![session];
        let mode = AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
            session_id: session_id.into(),
            scroll_offset: 0,
        };
        let progress_messages = HashMap::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_or_diff_mode(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        wall_clock_unix_seconds: 0,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Diff Session"));
        assert!(text.contains("No changes found."));
    }

    #[test]
    fn render_session_or_diff_mode_renders_review_comments_for_matching_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-comments";
        let sessions = vec![session_fixture(session_id)];
        let mode = AppMode::ReviewComments {
            comment_error: None,
            comment_snapshot: None,
            diff: String::new(),
            is_loading_comments: true,
            selected_comment_index: 0,
            session_id: session_id.into(),
            scroll_offset: 0,
        };
        let progress_messages = HashMap::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_or_diff_mode(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        wall_clock_unix_seconds: 0,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Comment — Router Session"));
        assert!(text.contains("Loading review comments..."));
    }

    #[test]
    fn render_session_confirmation_overlay_renders_confirmation_text() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-merge";
        let sessions = vec![session_fixture(session_id)];
        let progress_messages = HashMap::new();
        let confirmation_context = SessionConfirmationContext {
            confirmation_message: "Queue merge now?",
            confirmation_title: "Confirm Merge",
            selected_confirmation_index: 0,
        };
        let view_mode = ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.into(),
        };
        let cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_confirmation_overlay(
                    frame,
                    frame.area(),
                    SessionOverlayRenderContext {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::High,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        restore_view: &view_mode,
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        sessions: &sessions,
                        wall_clock_unix_seconds: 0,
                    },
                    &confirmation_context,
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Confirm Merge"));
        assert!(text.contains("Queue merge now?"));
    }
}
