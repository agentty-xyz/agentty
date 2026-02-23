use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::{SettingsManager, Tab};
use crate::domain::permission::PlanFollowup;
use crate::domain::project::Project;
use crate::domain::session::{AllTimeModelUsage, CodexUsageLimits, DailyActivity, Session};
use crate::ui::state::app_mode::AppMode;
use crate::ui::state::palette::{PaletteCommand, PaletteFocus};
use crate::ui::{Component, Page, RenderContext, components, overlays, pages};

/// Shared borrowed data required to render list-page backgrounds.
pub(crate) struct ListBackgroundRenderContext<'a> {
    pub(crate) all_time_model_usage: &'a [AllTimeModelUsage],
    pub(crate) codex_usage_limits: Option<CodexUsageLimits>,
    pub(crate) current_tab: Tab,
    pub(crate) longest_session_duration_seconds: u64,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
}

/// Context passed while rendering list-family modes and overlays.
struct ListModeRenderContext<'a> {
    active_project_id: i64,
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    mode: &'a AppMode,
    projects: &'a [Project],
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl<'a> ListModeRenderContext<'a> {
    /// Converts list mode context into the shared list-background context.
    fn into_list_background(self) -> ListBackgroundRenderContext<'a> {
        ListBackgroundRenderContext {
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            current_tab: self.current_tab,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }

    /// Converts list mode context into command-palette rendering context.
    fn into_command_palette(
        self,
        focus: PaletteFocus,
        input: &'a str,
        selected_index: usize,
    ) -> CommandPaletteRenderContext<'a> {
        CommandPaletteRenderContext {
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            current_tab: self.current_tab,
            focus,
            input,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            selected_index,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }

    /// Converts list mode context into command-option rendering context.
    fn into_command_option(
        self,
        command: PaletteCommand,
        selected_index: usize,
    ) -> CommandOptionRenderContext<'a> {
        CommandOptionRenderContext {
            active_project_id: self.active_project_id,
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            command,
            current_tab: self.current_tab,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            projects: self.projects,
            selected_index,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }

    /// Converts list mode context into sync-popup rendering context.
    fn into_sync_popup(
        self,
        default_branch: Option<&'a str>,
        is_loading: bool,
        message: &'a str,
        project_name: Option<&'a str>,
        title: &'a str,
    ) -> SyncPopupRenderContext<'a> {
        SyncPopupRenderContext {
            all_time_model_usage: self.all_time_model_usage,
            codex_usage_limits: self.codex_usage_limits,
            current_tab: self.current_tab,
            default_branch,
            is_loading,
            longest_session_duration_seconds: self.longest_session_duration_seconds,
            message,
            project_name,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
            title,
        }
    }
}

/// Shared borrowed data required to render command palette overlays.
pub(crate) struct CommandPaletteRenderContext<'a> {
    pub(crate) all_time_model_usage: &'a [AllTimeModelUsage],
    pub(crate) codex_usage_limits: Option<CodexUsageLimits>,
    pub(crate) current_tab: Tab,
    pub(crate) focus: PaletteFocus,
    pub(crate) input: &'a str,
    pub(crate) longest_session_duration_seconds: u64,
    pub(crate) selected_index: usize,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
}

/// Shared borrowed data required to render command option overlays.
pub(crate) struct CommandOptionRenderContext<'a> {
    pub(crate) active_project_id: i64,
    pub(crate) all_time_model_usage: &'a [AllTimeModelUsage],
    pub(crate) codex_usage_limits: Option<CodexUsageLimits>,
    pub(crate) command: PaletteCommand,
    pub(crate) current_tab: Tab,
    pub(crate) longest_session_duration_seconds: u64,
    pub(crate) projects: &'a [Project],
    pub(crate) selected_index: usize,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
}

/// Shared borrowed data required to render the sync popup overlay.
pub(crate) struct SyncPopupRenderContext<'a> {
    pub(crate) all_time_model_usage: &'a [AllTimeModelUsage],
    pub(crate) codex_usage_limits: Option<CodexUsageLimits>,
    pub(crate) current_tab: Tab,
    pub(crate) default_branch: Option<&'a str>,
    pub(crate) is_loading: bool,
    pub(crate) longest_session_duration_seconds: u64,
    pub(crate) message: &'a str,
    pub(crate) project_name: Option<&'a str>,
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
    pub(crate) title: &'a str,
}

/// Context passed while rendering view/prompt/diff/help modes.
struct SessionModeRenderContext<'a> {
    all_time_model_usage: &'a [AllTimeModelUsage],
    codex_usage_limits: Option<CodexUsageLimits>,
    current_tab: Tab,
    longest_session_duration_seconds: u64,
    mode: &'a AppMode,
    plan_followups: &'a HashMap<String, PlanFollowup>,
    session_progress_messages: &'a HashMap<String, String>,
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

/// Borrowed data for rendering `SessionChatPage` in view/prompt modes.
struct SessionChatRenderContext<'a> {
    mode: &'a AppMode,
    plan_followups: Option<&'a HashMap<String, PlanFollowup>>,
    scroll_offset: Option<u16>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        mode,
        plan_followups,
        projects,
        session_progress_messages,
        settings,
        stats_activity,
        sessions,
        table_state,
        ..
    } = context;

    match mode {
        AppMode::List
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Confirmation { .. }
        | AppMode::CommandPalette { .. }
        | AppMode::CommandOption { .. } => render_list_mode_content(
            f,
            area,
            ListModeRenderContext {
                active_project_id,
                all_time_model_usage,
                codex_usage_limits,
                current_tab,
                longest_session_duration_seconds,
                mode,
                projects,
                sessions,
                settings,
                stats_activity,
                table_state,
            },
        ),
        AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Diff { .. }
        | AppMode::Help { .. } => render_session_mode_content(
            f,
            area,
            SessionModeRenderContext {
                all_time_model_usage,
                codex_usage_limits,
                current_tab,
                longest_session_duration_seconds,
                mode,
                plan_followups,
                session_progress_messages,
                sessions,
                settings,
                stats_activity,
                table_state,
            },
        ),
    }
}

/// Renders list modes and list-bound overlays.
fn render_list_mode_content(f: &mut Frame, area: Rect, context: ListModeRenderContext<'_>) {
    match context.mode {
        AppMode::List => render_list_background(f, area, context.into_list_background()),
        AppMode::Confirmation { .. } => {
            overlays::render_confirmation_overlay(
                f,
                area,
                context.mode,
                context.into_list_background(),
            );
        }
        AppMode::CommandPalette {
            input,
            selected_index,
            focus,
        } => overlays::render_command_palette(
            f,
            area,
            context.into_command_palette(*focus, input, *selected_index),
        ),
        AppMode::CommandOption {
            command,
            selected_index,
        } => overlays::render_command_options(
            f,
            area,
            context.into_command_option(*command, *selected_index),
        ),
        AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } => overlays::render_sync_blocked_popup(
            f,
            area,
            context.into_sync_popup(
                default_branch.as_deref(),
                *is_loading,
                message,
                project_name.as_deref(),
                title,
            ),
        ),
        _ => {}
    }
}

/// Renders session modes and help overlays.
fn render_session_mode_content(f: &mut Frame, area: Rect, context: SessionModeRenderContext<'_>) {
    let SessionModeRenderContext {
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        mode,
        plan_followups,
        session_progress_messages,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;

    match mode {
        AppMode::View {
            session_id,
            scroll_offset,
        } => render_session_chat_mode(
            f,
            area,
            sessions,
            &view_session_chat_context(
                mode,
                plan_followups,
                *scroll_offset,
                session_id,
                session_progress_messages,
            ),
        ),
        AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } => render_session_chat_mode(
            f,
            area,
            sessions,
            &prompt_session_chat_context(
                mode,
                *scroll_offset,
                session_id,
                session_progress_messages,
            ),
        ),
        AppMode::Diff {
            diff,
            file_explorer_selected_index,
            scroll_offset,
            session_id,
        } => render_diff_mode(
            f,
            area,
            sessions,
            session_id,
            diff,
            *scroll_offset,
            *file_explorer_selected_index,
        ),
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => overlays::render_help(
            f,
            area,
            help_context,
            *scroll_offset,
            overlays::HelpBackgroundRenderContext {
                all_time_model_usage,
                codex_usage_limits,
                context: help_context,
                current_tab,
                longest_session_duration_seconds,
                plan_followups,
                session_progress_messages,
                sessions,
                settings,
                stats_activity,
                table_state,
            },
        ),
        _ => {}
    }
}

/// Builds chat render context for `AppMode::View`.
fn view_session_chat_context<'a>(
    mode: &'a AppMode,
    plan_followups: &'a HashMap<String, PlanFollowup>,
    scroll_offset: Option<u16>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
) -> SessionChatRenderContext<'a> {
    SessionChatRenderContext {
        mode,
        plan_followups: Some(plan_followups),
        scroll_offset,
        session_id,
        session_progress_messages,
    }
}

/// Builds chat render context for `AppMode::Prompt`.
fn prompt_session_chat_context<'a>(
    mode: &'a AppMode,
    scroll_offset: Option<u16>,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
) -> SessionChatRenderContext<'a> {
    SessionChatRenderContext {
        mode,
        plan_followups: None,
        scroll_offset,
        session_id,
        session_progress_messages,
    }
}

/// Renders the diff page for a specific session id when present.
fn render_diff_mode(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    session_id: &str,
    diff: &str,
    scroll_offset: u16,
    file_explorer_selected_index: usize,
) {
    if let Some(session) = sessions.iter().find(|session| session.id == session_id) {
        pages::diff::DiffPage::new(
            session,
            diff.to_string(),
            scroll_offset,
            file_explorer_selected_index,
        )
        .render(f, area);
    }
}

/// Renders the chat page for prompt/view modes when session exists.
fn render_session_chat_mode(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    context: &SessionChatRenderContext<'_>,
) {
    let mode = context.mode;
    let plan_followups = context.plan_followups;
    let scroll_offset = context.scroll_offset;
    let session_id = context.session_id;
    let session_progress_messages = context.session_progress_messages;

    if let Some(session_index) = sessions.iter().position(|session| session.id == session_id) {
        let plan_followup = plan_followups.and_then(|followups| followups.get(session_id));
        let active_progress = session_progress_messages
            .get(session_id)
            .map(std::string::String::as_str);
        pages::session_chat::SessionChatPage::new(
            sessions,
            session_index,
            scroll_offset,
            mode,
            plan_followup,
            active_progress,
        )
        .render(f, area);
    }
}

/// Renders base list tabs and the currently selected list tab content.
pub(crate) fn render_list_background(
    f: &mut Frame,
    content_area: Rect,
    context: ListBackgroundRenderContext<'_>,
) {
    let ListBackgroundRenderContext {
        all_time_model_usage,
        codex_usage_limits,
        current_tab,
        longest_session_duration_seconds,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;

    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    components::tab::Tabs::new(current_tab).render(f, chunks[0]);

    match current_tab {
        Tab::Sessions => {
            pages::session_list::SessionListPage::new(sessions, table_state).render(f, chunks[1]);
        }
        Tab::Stats => {
            pages::stats::StatsPage::new(
                sessions,
                stats_activity,
                all_time_model_usage,
                longest_session_duration_seconds,
                codex_usage_limits,
            )
            .render(f, chunks[1]);
        }
        Tab::Settings => {
            pages::settings::SettingsPage::new(settings).render(f, chunks[1]);
        }
    }
}
