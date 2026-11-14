use std::ops::{Deref, DerefMut};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::domain::question::QuestionItem;
use crate::domain::session::{
    Session, Status, can_merge_session_branch_in_stack, can_mutate_session_branch_in_stack,
    can_rebase_session_branch_in_stack, can_reply_to_session_in_stack,
    can_start_staged_session_in_stack,
};
use crate::domain::{input, review};
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::presentation::help_action::{self, ViewActionAvailability, ViewHelpState};
use crate::presentation::prompt::{PromptAtMentionState, PromptSlashCommandSet};
use crate::ui::component::chat_input::{ChatInput, SuggestionList};
use crate::ui::component::session_output::{
    SessionOutput, SessionOutputLayoutCache, SessionOutputLineContext,
};
use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::icon::Icon;
use crate::ui::input_layout::{
    calculate_input_height, overlay_area_above, panel_inner_height, suggestion_dropdown_height,
};
use crate::ui::page::graph_chat;
use crate::ui::{
    Component, Page, layout, markdown, prompt_format, question_format, session_format, style,
};

/// Archived sessions reserve one timing row and one navigation row.
const ARCHIVED_FOOTER_HEIGHT: u16 = 2;

/// Placeholder of an ordinary session composer.
const DEFAULT_COMPOSER_PLACEHOLDER: &str = "Type your message";

/// Maximum rendered height of the prompt input panel, including borders.
const CHAT_INPUT_MAX_PANEL_HEIGHT: u16 = 10;

/// Height of the inactive composer, keeping only its two border rows.
const INACTIVE_COMPOSER_HEIGHT: u16 = 2;

/// Header height assumed when the rendered header line count does not fit
/// `u16`.
const SESSION_HEADER_FALLBACK_HEIGHT: u16 = 2;

/// Height of the single-row footer reserved by non-prompt, non-question modes.
const SINGLE_ROW_FOOTER_HEIGHT: u16 = 1;

/// Prompt-panel data prepared once per render pass so layout and painting use
/// the same suggestion set.
struct PreparedPromptPanel {
    footer_text: Line<'static>,
    /// Whether the transcript above the composer currently holds focus, which
    /// dims the composer border and hides its cursor.
    is_chat_focused: bool,
    /// Whether the footer carries the clipboard image-capture loader.
    is_pasting_image: bool,
    status: String,
    suggestion_list: Option<SuggestionList>,
    title: String,
    total_height: u16,
}

impl PreparedPromptPanel {
    /// Returns the reserved prompt-panel height for the current render pass.
    fn panel_height(&self) -> u16 {
        self.total_height
    }
}

/// Complete geometry and prepared panel data for one session-chat frame.
///
/// Both painting and runtime scroll metrics construct this plan, keeping the
/// transcript viewport, prompt dropdown, and question-panel allocation on one
/// deterministic path.
struct SessionChatLayoutPlan {
    areas: layout::SessionChatAreas,
    header_lines: Vec<Line<'static>>,
    prompt_panel: Option<PreparedPromptPanel>,
    question_panel_areas: Option<layout::QuestionPanelAreas>,
}

impl SessionChatLayoutPlan {
    /// Resolves all session-chat geometry and prepared prompt data once.
    fn new(input: SessionChatLayoutInput<'_>) -> Self {
        let header_lines = session_format::session_header_lines(
            input.session,
            input.area.width.saturating_sub(2),
            input.has_merge_conflict,
        );
        let (prompt_panel, bottom_height) =
            if matches!(input.session.status, Status::Done | Status::Canceled) {
                (
                    None,
                    ARCHIVED_FOOTER_HEIGHT.min(input.area.height.saturating_sub(1)),
                )
            } else {
                let prompt_panel =
                    prepare_prompt_panel(input.area, input.mode, input.review_text, input.session);
                let bottom_height = prompt_panel.as_ref().map_or_else(
                    || non_prompt_bottom_height(input.area, input.mode),
                    PreparedPromptPanel::panel_height,
                );

                (prompt_panel, bottom_height)
            };
        let mut areas =
            layout::session_chat_areas(input.area, bottom_height, header_height(&header_lines));
        if !matches!(input.session.status, Status::Done | Status::Canceled)
            && !matches!(input.mode, AppMode::Question { .. })
        {
            // Keep the composer's top margin outside the scrollable transcript.
            areas.output_area.height = areas.output_area.height.saturating_sub(1);
        }
        let question_panel_areas = question_panel_areas(areas.bottom_area, input.mode);

        Self {
            areas,
            header_lines,
            prompt_panel,
            question_panel_areas,
        }
    }

    /// Returns the transcript rows inside the bordered output panel.
    fn transcript_view_height(&self) -> u16 {
        panel_inner_height(
            self.areas.output_area,
            session_format::session_output_panel_borders(),
        )
    }
}

/// Borrowed inputs needed to construct one session chat page renderer.
#[derive(Clone, Copy)]
pub struct SessionChatPageInput<'a> {
    /// Transient progress text rendered in the active-status loader row.
    pub active_progress: Option<&'a str>,
    /// Exact prompt transcript block for the currently active turn, when one
    /// has been submitted in this app process.
    pub active_prompt_output: Option<&'a str>,
    /// Whether the session branch currently conflicts with its base branch.
    pub has_merge_conflict: bool,
    /// Shared render cache for session transcript markdown.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current UI mode that determines view, prompt, and question rendering.
    pub mode: &'a AppMode,
    /// Shared output-layout cache for this render pass.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    /// Focused-review output for the rendered session.
    pub review_text: Option<&'a str>,
    /// Current vertical output scroll offset.
    pub scroll_offset: Option<usize>,
    /// Index of the session being rendered.
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    /// Session rows available to the page.
    pub sessions: &'a [Session],
    /// One coherent render-time clock snapshot.
    pub(crate) frame_time: FrameTime,
}

/// Chat page renderer for a single session.
///
/// Field access delegates to the stored inputs so callers can continue reading
/// and updating render state directly.
pub struct SessionChatPage<'a> {
    /// Whether the session worktree can be opened externally.
    pub can_open_worktree: bool,
    graph_chat: Option<&'a GraphChatSnapshot>,
    /// Borrowed render state shared with page construction.
    input: SessionChatPageInput<'a>,
}

impl<'a> SessionChatPage<'a> {
    /// Creates a session chat page renderer.
    pub fn new(input: SessionChatPageInput<'a>) -> Self {
        Self {
            can_open_worktree: false,
            graph_chat: None,
            input,
        }
    }

    /// Sets whether the rendered session currently exposes a materialized
    /// worktree that can be opened from the footer/help affordances.
    #[must_use]
    pub fn can_open_worktree(mut self, can_open_worktree: bool) -> Self {
        self.can_open_worktree = can_open_worktree;
        self
    }

    /// Supplies the current graph proposal and execution status.
    pub(crate) fn graph_chat(mut self, snapshot: Option<&'a GraphChatSnapshot>) -> Self {
        self.graph_chat = snapshot;

        self
    }

    /// Returns the rendered output line count for chat content at a given
    /// width and viewport height.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering, including review text, generic active-status loaders, and
    /// conditional scrollbar gutter reservation, so scroll math can stay in
    /// sync with what users see.
    pub(crate) fn rendered_output_line_count(
        session: &Session,
        output_width: u16,
        viewport_height: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: &markdown::MarkdownRenderCache,
        output_layout_cache: &SessionOutputLayoutCache,
    ) -> usize {
        SessionOutput::rendered_line_count(
            session,
            output_width,
            viewport_height,
            context,
            Some(markdown_render_cache),
            Some(output_layout_cache),
        )
    }

    /// Renders the session header, output panel, and context-aware bottom
    /// panel.
    fn render_session(&self, f: &mut Frame, area: Rect, session: &Session) {
        let (area, graph_area) = graph_chat::areas(area, self.mode, self.graph_chat.is_some());
        if let Some(graph_area) = graph_area {
            graph_chat::render(f, graph_area, self.graph_chat);
        }
        let layout_plan = SessionChatLayoutPlan::new(SessionChatLayoutInput {
            area,
            has_merge_conflict: self.has_merge_conflict,
            mode: self.mode,
            review_text: self.review_text,
            session,
        });

        let mut output = SessionOutput::new(session)
            .markdown_render_cache(self.markdown_render_cache)
            .output_layout_cache(self.output_layout_cache)
            .session_update_version(self.session_update_version)
            .spinner_frame(Icon::spinner_frame_from_millis(
                self.frame_time.unix_millis(),
            ));
        output = output.active_prompt_output(self.active_prompt_output);
        if let Some(scroll_offset) = self.scroll_offset {
            output = output.scroll_offset(scroll_offset);
        }
        if let Some(active_progress) = self.active_progress {
            output = output.active_progress(active_progress);
        }
        output.render(f, layout_plan.areas.output_area);
        let timing_padding = if matches!(session.status, Status::Done | Status::Canceled) {
            0
        } else {
            4
        };
        let timing_label = session_format::session_timing_label(
            session,
            self.frame_time,
            layout_plan
                .areas
                .bottom_area
                .width
                .saturating_sub(timing_padding),
        );
        self.render_bottom_panel(f, session, &layout_plan, &timing_label);
        Self::render_session_header(f, layout_plan.areas.header_area, layout_plan.header_lines);
    }

    /// Renders the header above the output panel border.
    fn render_session_header(f: &mut Frame, header_area: Rect, header_lines: Vec<Line<'static>>) {
        let header = Paragraph::new(header_lines);

        f.render_widget(header, header_area);
    }

    /// Renders the composer and footer, or the interactive question panel.
    /// Archived sessions keep timing and navigation rows without a composer.
    ///
    /// Every composer right-aligns timing in its bottom border, prioritizing
    /// the active-work timer when the session age does not fit alongside it.
    fn render_bottom_panel(
        &self,
        f: &mut Frame,
        session: &Session,
        layout_plan: &SessionChatLayoutPlan,
        timing_label: &str,
    ) {
        let bottom_area = layout_plan.areas.bottom_area;

        if matches!(session.status, Status::Done | Status::Canceled) {
            let footer_areas =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(bottom_area);
            f.render_widget(
                Paragraph::new(
                    Line::styled(
                        timing_label,
                        Style::default().fg(style::palette::text_muted()),
                    )
                    .right_aligned(),
                ),
                footer_areas[0],
            );
            f.render_widget(
                Paragraph::new(session_format::session_view_footer_line(
                    self.view_help_state(session),
                )),
                footer_areas[1],
            );

            return;
        }

        // Layout planning prepares a prompt panel for every non-archived prompt
        // mode.
        if let AppMode::Prompt { input, .. } = self.mode
            && let Some(prepared_prompt_panel) = layout_plan.prompt_panel.as_ref()
        {
            self.render_prompt_composer(f, bottom_area, prepared_prompt_panel, input, timing_label);

            return;
        }

        if let AppMode::Question {
            at_mention_state,
            focus,
            questions,
            current_index,
            input,
            selected_option_index,
            ..
        } = self.mode
        {
            render_question_panel(
                f,
                bottom_area,
                layout_plan.question_panel_areas,
                &QuestionPanelState {
                    at_mention_state: at_mention_state.as_ref(),
                    current_index: *current_index,
                    focus: *focus,
                    has_session_diff: session.stats.should_show_diff(),
                    input,
                    questions,
                    selected_option_index: *selected_option_index,
                    timing_label,
                },
            );

            return;
        }

        let title = session_composer_title(session);
        let status = session_format::prompt_session_status(session);
        let panel_areas = layout::prompt_panel_areas(bottom_area);
        ChatInput::new(&title, "", 0)
            .status(&status)
            .footer(timing_label)
            .active(false)
            .render(f, panel_areas.input_area);

        let help_message = Paragraph::new(session_format::session_view_footer_line(
            self.view_help_state(session),
        ));
        f.render_widget(help_message, panel_areas.footer_area);
    }

    /// Renders the active composer, its suggestion dropdown, and its footer.
    ///
    /// An armed `/graph` composer asks for the execution-graph task.
    fn render_prompt_composer(
        &self,
        f: &mut Frame,
        bottom_area: Rect,
        prepared_prompt_panel: &PreparedPromptPanel,
        input: &input::InputState,
        timing_label: &str,
    ) {
        let (title, placeholder) = if matches!(
            self.mode,
            AppMode::Prompt { slash_state, .. } if slash_state.graph.is_armed()
        ) {
            ("Graph", "Describe the task for the execution graph")
        } else {
            (
                prepared_prompt_panel.title.as_str(),
                DEFAULT_COMPOSER_PLACEHOLDER,
            )
        };
        let mut chat_input = ChatInput::new(title, input.text(), input.cursor)
            .placeholder(placeholder)
            .footer(timing_label)
            .active(!prepared_prompt_panel.is_chat_focused)
            .status(&prepared_prompt_panel.status);

        if let Some(suggestion_list) = &prepared_prompt_panel.suggestion_list {
            chat_input = chat_input.suggestion_list(suggestion_list);
        }

        if bottom_area.height <= 1 {
            chat_input.render(f, bottom_area);

            return;
        }

        let panel_areas = layout::prompt_panel_areas(bottom_area);

        chat_input.render(f, panel_areas.input_area);
        f.render_widget(
            Paragraph::new(prepared_prompt_panel.footer_text.clone()),
            panel_areas.footer_area,
        );
        if prepared_prompt_panel.is_pasting_image {
            TachyonLoaderEffect::apply_to_last_glyph(
                f.buffer_mut(),
                panel_areas.footer_area,
                Icon::spinner_frame_from_millis(self.frame_time.unix_millis()),
            );
        }
    }

    /// Resolves which view-mode footer actions apply to `session` within its
    /// stack.
    fn view_help_state(&self, session: &Session) -> ViewHelpState {
        let can_start_staged_session =
            can_start_staged_session_in_stack(self.sessions, session.id.as_str());
        let can_reply_to_session =
            can_reply_to_session_in_stack(self.sessions, session.id.as_str());
        let can_merge_session_branch =
            can_merge_session_branch_in_stack(self.sessions, session.id.as_str());
        let can_mutate_session_branch =
            can_mutate_session_branch_in_stack(self.sessions, session.id.as_str());
        let can_rebase_session_branch =
            can_rebase_session_branch_in_stack(self.sessions, session.id.as_str());

        ViewHelpState {
            can_fork_session: ViewActionAvailability::from_bool(session.allows_fork_action()),
            can_merge_session_branch: ViewActionAvailability::from_bool(can_merge_session_branch),
            can_mutate_session_branch: ViewActionAvailability::from_bool(can_mutate_session_branch),
            can_open_worktree: ViewActionAvailability::from_bool(
                self.can_open_worktree && session.allows_worktree_open_action(),
            ),
            can_rebase_session_branch: ViewActionAvailability::from_bool(can_rebase_session_branch),
            can_show_diff: ViewActionAvailability::from_bool(session.allows_diff_view()),
            reply_to_session: ViewActionAvailability::from_bool(can_reply_to_session),
            can_start_staged_session: ViewActionAvailability::from_bool(can_start_staged_session),
            publish_pull_request_action: session.publish_pull_request_action(),
            session_state: help_action::session_view_state(session),
        }
    }
}

impl<'a> Deref for SessionChatPage<'a> {
    type Target = SessionChatPageInput<'a>;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

impl DerefMut for SessionChatPage<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.input
    }
}

/// Returns the session settings title shared by active and inactive composers.
fn session_composer_title(session: &Session) -> String {
    format!(
        "{}/{} [{}]",
        session.agent.kind(),
        session.agent.model().as_str(),
        session.effective_reasoning_level().as_str()
    )
}

/// Session-chat geometry inputs available outside a render pass.
///
/// Runtime scroll handlers hold app state instead of a page instance, so the
/// values the page derives its geometry from are passed explicitly.
#[derive(Clone, Copy)]
pub(crate) struct SessionChatLayoutInput<'a> {
    /// Page area routed to the session chat page, excluding the status and
    /// footer bars.
    pub(crate) area: Rect,
    /// Whether the session branch currently conflicts with its base branch.
    pub(crate) has_merge_conflict: bool,
    /// Current UI mode, which determines the reserved bottom-panel height.
    pub(crate) mode: &'a AppMode,
    /// Focused-review output for the rendered session.
    pub(crate) review_text: Option<&'a str>,
    /// Session whose transcript is rendered.
    pub(crate) session: &'a Session,
}

/// Returns the transcript rows the session output panel paints for `input`.
///
/// Scroll math routes through the same header, bottom-panel, and border
/// geometry the renderer uses, so a tall composer or an open suggestion
/// dropdown shrinks the scroll viewport exactly as it does on screen.
pub(crate) fn transcript_view_height(input: SessionChatLayoutInput<'_>) -> u16 {
    SessionChatLayoutPlan::new(input).transcript_view_height()
}

/// Returns the header rows reserved above the session output panel.
fn header_height(header_lines: &[Line<'static>]) -> u16 {
    u16::try_from(header_lines.len()).unwrap_or(SESSION_HEADER_FALLBACK_HEIGHT)
}

/// Prepares prompt-panel layout and suggestion data once for a render pass.
///
/// Returns `None` outside prompt mode.
fn prepare_prompt_panel(
    area: Rect,
    mode: &AppMode,
    review_text: Option<&str>,
    session: &Session,
) -> Option<PreparedPromptPanel> {
    let AppMode::Prompt {
        at_mention_state,
        attachment_state,
        focus,
        input,
        slash_state,
        ..
    } = mode
    else {
        return None;
    };

    // An armed graph composer treats its task as literal text. A session
    // that queues chat messages offers only setting commands.
    let suppress_slash_dropdown = slash_state.graph.is_armed();
    let has_active_mention = at_mention_state.is_some() && input.at_mention_query().is_some();
    let command_set = PromptSlashCommandSet::for_session(
        review::has_actionable_review_suggestions(review_text),
        session.queues_chat_messages(),
    );
    let suggestion_list =
        if suppress_slash_dropdown && input.text().starts_with('/') && !has_active_mention {
            None
        } else {
            prompt_format::prompt_suggestion_list(
                input,
                slash_state,
                at_mention_state.as_ref(),
                session.agent.kind(),
                command_set,
            )
        };
    let dropdown_row_count =
        prompt_format::prompt_suggestion_dropdown_rows(suggestion_list.as_ref());
    let input_height = calculate_input_height(area.width.saturating_sub(2), input.text())
        .min(CHAT_INPUT_MAX_PANEL_HEIGHT);
    let desired_bottom_height = input_height
        .saturating_add(u16::try_from(dropdown_row_count).unwrap_or(u16::MAX))
        .saturating_add(1);
    let max_bottom_height = area.height.saturating_sub(1);

    Some(PreparedPromptPanel {
        footer_text: prompt_format::prompt_footer_line(
            session,
            attachment_state.attachments.len(),
            *focus,
            attachment_state.is_pasting_image(),
        ),
        is_chat_focused: *focus == ChatFocus::Chat,
        is_pasting_image: attachment_state.is_pasting_image(),
        status: session_format::prompt_session_status(session),
        suggestion_list,
        title: session_composer_title(session),
        total_height: desired_bottom_height.min(max_bottom_height),
    })
}

/// Returns the bottom-panel height reserved for non-prompt page modes.
///
/// Question mode derives its height from the question layout helper and the
/// visible option list. All other modes reserve the inactive composer borders
/// and a footer row, with no empty input row.
fn non_prompt_bottom_height(area: Rect, mode: &AppMode) -> u16 {
    let AppMode::Question {
        questions,
        current_index,
        input,
        selected_option_index,
        ..
    } = mode
    else {
        return INACTIVE_COMPOSER_HEIGHT
            .saturating_add(SINGLE_ROW_FOOTER_HEIGHT)
            .min(area.height.saturating_sub(1));
    };

    let question_item = questions.get(*current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item.map_or_default(|item| item.options.as_slice());
    let is_free_text_mode = selected_option_index.is_none();
    let input_text = if is_free_text_mode { input.text() } else { "" };

    layout::question_panel_reserved_height(
        area.width,
        area.height.saturating_sub(SINGLE_ROW_FOOTER_HEIGHT),
        question,
        input_text,
        options.len(),
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    )
}

/// Resolves question sub-areas from the already reserved bottom-panel area.
fn question_panel_areas(bottom_area: Rect, mode: &AppMode) -> Option<layout::QuestionPanelAreas> {
    let AppMode::Question {
        questions,
        current_index,
        input,
        selected_option_index,
        ..
    } = mode
    else {
        return None;
    };
    let question_item = questions.get(*current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item.map_or_default(|item| item.options.as_slice());
    let input_text = if selected_option_index.is_none() {
        input.text()
    } else {
        ""
    };

    Some(layout::question_panel_areas(
        bottom_area,
        question,
        input_text,
        options.len(),
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    ))
}

/// Bundled question-mode state passed to the panel renderer.
#[derive(Clone, Copy)]
struct QuestionPanelState<'a> {
    at_mention_state: Option<&'a PromptAtMentionState>,
    current_index: usize,
    focus: ChatFocus,
    has_session_diff: bool,
    input: &'a input::InputState,
    questions: &'a [QuestionItem],
    selected_option_index: Option<usize>,
    timing_label: &'a str,
}

/// Renders the question-mode bottom panel with question text, options, input,
/// and help footer.
fn render_question_panel(
    f: &mut Frame,
    bottom_area: Rect,
    panel_areas: Option<layout::QuestionPanelAreas>,
    state: &QuestionPanelState<'_>,
) {
    let QuestionPanelState {
        at_mention_state,
        current_index,
        focus,
        has_session_diff,
        input,
        questions,
        selected_option_index,
        timing_label,
    } = *state;
    let question_item = questions.get(current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item.map_or_default(|item| item.options.as_slice());
    let is_free_text_mode = selected_option_index.is_none();
    let Some(panel_areas) = panel_areas else {
        return;
    };

    let is_chat_focused = focus == ChatFocus::Chat;
    let question_title = format!("Question {}/{}", current_index + 1, questions.len());
    if panel_areas.question_area.height > 0 {
        let question_para = Paragraph::new(question_format::question_panel_lines(
            &question_title,
            question,
            is_chat_focused,
            bottom_area.width,
        ));
        f.render_widget(question_para, panel_areas.question_area);
    }

    if panel_areas.options_area.height > 0 {
        f.render_widget(
            Paragraph::new(question_format::question_option_lines(
                options,
                selected_option_index,
                is_chat_focused,
            )),
            panel_areas.options_area,
        );
    }

    // Always render the input widget so the panel height stays stable
    // across mode transitions. When navigating options the input shows a
    // placeholder; when in free-text mode it is fully editable.
    let (display_text, display_cursor) = if is_free_text_mode {
        (input.text(), input.cursor)
    } else {
        ("", 0)
    };
    let input_placeholder = "Type answer";
    let at_mention_max_visible =
        layout::question_at_mention_max_visible(bottom_area, panel_areas.input_area, 10);
    let at_mention_menu = if is_free_text_mode && at_mention_max_visible > 0 {
        at_mention_state.and_then(|state| {
            prompt_format::file_lookup_suggestion_list(
                display_text,
                display_cursor,
                state,
                at_mention_max_visible,
            )
        })
    } else {
        None
    };
    let chat_input = ChatInput::new("Answer", display_text, display_cursor)
        .placeholder(input_placeholder)
        .footer(timing_label)
        .active(is_free_text_mode && !is_chat_focused);
    if panel_areas.input_area.height > 0 {
        chat_input.render(f, panel_areas.input_area);
    }

    let is_at_mention_open =
        is_free_text_mode && at_mention_state.is_some() && input.at_mention_query().is_some();
    let lookup_state = if !is_at_mention_open {
        question_format::QuestionLookupState::Closed
    } else if at_mention_max_visible == 0 {
        question_format::QuestionLookupState::Clipped
    } else if at_mention_menu.is_some() {
        question_format::QuestionLookupState::Matches
    } else {
        question_format::QuestionLookupState::Empty
    };
    render_question_at_mention_overlay(f, bottom_area, panel_areas.input_area, at_mention_menu);
    render_question_help_footer(
        f,
        panel_areas.help_area,
        focus,
        has_session_diff,
        !is_free_text_mode,
        lookup_state,
    );
}

/// Renders the question-mode help footer with context-aware action hints.
///
/// `has_session_diff` controls whether the chat-focused footer advertises the
/// diff preview.
///
/// `is_navigating_options` mirrors the runtime predicate that treats plain `q`
/// as a sessions-list shortcut, so the footer can surface it whenever the
/// shortcut is actually wired up. `lookup_state` only advertises selection
/// and navigation when the prepared suggestion list contains matches.
fn render_question_help_footer(
    f: &mut Frame,
    area: Rect,
    focus: ChatFocus,
    has_session_diff: bool,
    is_navigating_options: bool,
    lookup_state: question_format::QuestionLookupState,
) {
    if area.height == 0 {
        return;
    }

    let help_para = Paragraph::new(question_format::question_help_footer_line(
        focus,
        has_session_diff,
        is_navigating_options,
        lookup_state,
    ))
    .alignment(ratatui::layout::Alignment::Left);
    f.render_widget(help_para, area);
}

/// Renders the at-mention file dropdown as an overlay above the input area.
///
/// The dropdown covers the options section so the file list is fully visible
/// without pushing the input line out of view.
fn render_question_at_mention_overlay(
    f: &mut Frame,
    bottom_area: Rect,
    input_area: Rect,
    at_mention_menu: Option<SuggestionList>,
) {
    // A menu exists only when at least one lookup row fits above the input,
    // so the dropdown area is available whenever a menu is present.
    if let Some(menu) = at_mention_menu
        && let Some(dropdown_area) = overlay_area_above(
            bottom_area,
            input_area,
            suggestion_dropdown_height(menu.items.len()),
        )
    {
        ChatInput::render_suggestion_dropdown(f, dropdown_area, &menu);
    }
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            self.render_session(f, area, session);
        }
    }
}

#[cfg(test)]
#[path = "session_chat_test.rs"]
mod tests;
