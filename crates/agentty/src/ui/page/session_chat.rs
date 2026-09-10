use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::domain::agent::ReasoningLevel;
use crate::domain::question::QuestionItem;
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    Session, can_merge_session_branch_in_stack, can_mutate_session_branch_in_stack,
    can_rebase_session_branch_in_stack, can_reply_to_session_in_stack,
    can_start_staged_session_in_stack,
};
use crate::domain::{input, review};
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::help_action::{self, ViewActionAvailability, ViewHelpState};
use crate::presentation::prompt::PromptAtMentionState;
use crate::ui::component::chat_input::{ChatInput, SuggestionList};
use crate::ui::component::session_output::{
    SessionOutput, SessionOutputLayoutCache, SessionOutputLineContext,
};
use crate::ui::icon::Icon;
use crate::ui::input_layout::{
    calculate_input_height, overlay_area_above, panel_inner_height, suggestion_dropdown_height,
};
use crate::ui::{
    Component, Page, layout, markdown, prompt_format, question_format, session_format,
};

/// Maximum rendered height of the prompt input panel, including borders.
const CHAT_INPUT_MAX_PANEL_HEIGHT: u16 = 10;

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
    status: Option<String>,
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
        let mut header_lines = session_format::session_header_lines(
            input.session,
            input.area.width.saturating_sub(2),
            input.default_reasoning_level,
            input.wall_clock_unix_seconds,
            input.has_merge_conflict,
        );
        header_lines.push(session_format::session_resources_line(
            None,
            None,
            input.area.width.saturating_sub(2),
        ));
        let prompt_panel =
            prepare_prompt_panel(input.area, input.mode, input.review_text, input.session);
        let bottom_height = prompt_panel.as_ref().map_or_else(
            || non_prompt_bottom_height(input.area, input.mode),
            PreparedPromptPanel::panel_height,
        );
        let areas =
            layout::session_chat_areas(input.area, bottom_height, header_height(&header_lines));
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
    /// Active project-scoped default reasoning level.
    pub default_reasoning_level: ReasoningLevel,
    /// Whether the session branch currently conflicts with its base branch.
    pub has_merge_conflict: bool,
    /// Shared render cache for session transcript markdown.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current UI mode that determines view, prompt, and question rendering.
    pub mode: &'a AppMode,
    /// Shared output-layout cache for this render pass.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    /// Most recent tracked agent process-tree totals.
    pub resources: Option<SessionResources>,
    /// Focused-review output for the rendered session.
    pub review_text: Option<&'a str>,
    /// Current vertical output scroll offset.
    pub scroll_offset: Option<u16>,
    /// Index of the session being rendered.
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    /// Session rows available to the page.
    pub sessions: &'a [Session],
    /// One coherent render-time clock snapshot.
    pub(crate) frame_time: FrameTime,
    /// Host temperature supplied by the internal monitoring sidecar.
    pub(crate) host_cpu_temperature_celsius: Option<f32>,
}

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    /// Transient progress text for the active agent turn.
    pub active_progress: Option<&'a str>,
    /// Exact prompt transcript block for the active turn, when available.
    pub active_prompt_output: Option<&'a str>,
    /// Whether the session worktree can be opened externally.
    pub can_open_worktree: bool,
    /// Active project-scoped default reasoning level.
    pub default_reasoning_level: ReasoningLevel,
    /// Whether the session branch currently conflicts with its base branch.
    pub has_merge_conflict: bool,
    /// Shared markdown cache reused across transcript renders in this page.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current UI mode that controls the bottom panel and focus.
    pub mode: &'a AppMode,
    /// Shared fully assembled output-layout cache for scroll metrics and
    /// frame rendering.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    /// Most recent tracked agent process-tree totals.
    pub resources: Option<SessionResources>,
    /// Focused-review output for the rendered session.
    pub review_text: Option<&'a str>,
    /// Current vertical transcript scroll offset.
    pub scroll_offset: Option<u16>,
    /// Index of the session being rendered.
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    /// Session rows available to the page.
    pub sessions: &'a [Session],
    /// One coherent render-time clock snapshot.
    pub(crate) frame_time: FrameTime,
    /// Host temperature supplied by the internal monitoring sidecar.
    pub(crate) host_cpu_temperature_celsius: Option<f32>,
}

impl<'a> SessionChatPage<'a> {
    /// Creates a session chat page renderer.
    pub fn new(input: SessionChatPageInput<'a>) -> Self {
        let SessionChatPageInput {
            active_prompt_output,
            active_progress,
            resources,
            host_cpu_temperature_celsius,
            default_reasoning_level,
            frame_time,
            has_merge_conflict,
            markdown_render_cache,
            mode,
            output_layout_cache,
            review_text,
            scroll_offset,
            session_index,
            session_update_version,
            sessions,
        } = input;

        Self {
            active_prompt_output,
            active_progress,
            resources,
            host_cpu_temperature_celsius,
            can_open_worktree: false,
            default_reasoning_level,
            frame_time,
            has_merge_conflict,
            markdown_render_cache,
            mode,
            output_layout_cache,
            review_text,
            scroll_offset,
            session_index,
            session_update_version,
            sessions,
        }
    }

    /// Sets whether the rendered session currently exposes a materialized
    /// worktree that can be opened from the footer/help affordances.
    #[must_use]
    pub fn can_open_worktree(mut self, can_open_worktree: bool) -> Self {
        self.can_open_worktree = can_open_worktree;
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
    ) -> u16 {
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
        let mut layout_plan = SessionChatLayoutPlan::new(SessionChatLayoutInput {
            area,
            default_reasoning_level: self.default_reasoning_level,
            has_merge_conflict: self.has_merge_conflict,
            mode: self.mode,
            review_text: self.review_text,
            session,
            wall_clock_unix_seconds: self.frame_time.unix_seconds(),
        });

        if let Some(line) = layout_plan.header_lines.last_mut() {
            *line = session_format::session_resources_line(
                self.resources,
                self.host_cpu_temperature_celsius,
                area.width.saturating_sub(2),
            );
        }

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
        self.render_bottom_panel(f, session, &layout_plan);
        Self::render_session_header(f, layout_plan.areas.header_area, layout_plan.header_lines);
    }

    /// Renders the header above the output panel border.
    fn render_session_header(f: &mut Frame, header_area: Rect, header_lines: Vec<Line<'static>>) {
        let header = Paragraph::new(header_lines);

        f.render_widget(header, header_area);
    }

    /// Renders the context-aware bottom panel for prompt and question modes.
    fn render_bottom_panel(
        &self,
        f: &mut Frame,
        session: &Session,
        layout_plan: &SessionChatLayoutPlan,
    ) {
        let bottom_area = layout_plan.areas.bottom_area;

        if let AppMode::Prompt { input, .. } = self.mode {
            let Some(prepared_prompt_panel) = layout_plan.prompt_panel.as_ref() else {
                return;
            };

            let mut chat_input =
                ChatInput::new(&prepared_prompt_panel.title, input.text(), input.cursor)
                    .placeholder("Type your message")
                    .active(!prepared_prompt_panel.is_chat_focused);

            if let Some(status) = prepared_prompt_panel.status.as_deref() {
                chat_input = chat_input.status(status);
            }

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
                },
            );

            return;
        }

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
        let view_help_state = ViewHelpState {
            can_fork_session: ViewActionAvailability::from_bool(session.allows_fork_action()),
            can_merge_session_branch: ViewActionAvailability::from_bool(can_merge_session_branch),
            can_mutate_session_branch: ViewActionAvailability::from_bool(can_mutate_session_branch),
            can_open_worktree: ViewActionAvailability::from_bool(
                self.can_open_worktree && session.allows_worktree_open_action(),
            ),
            can_rebase_session_branch: ViewActionAvailability::from_bool(can_rebase_session_branch),
            can_show_diff: ViewActionAvailability::from_bool(session.stats.should_show_diff()),
            reply_to_session: ViewActionAvailability::from_bool(can_reply_to_session),
            can_start_staged_session: ViewActionAvailability::from_bool(can_start_staged_session),
            publish_pull_request_action: session.publish_pull_request_action(),
            session_state: help_action::session_view_state(session),
        };
        let help_message =
            Paragraph::new(session_format::session_view_footer_line(view_help_state));
        f.render_widget(help_message, bottom_area);
    }
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
    /// Active project-scoped default reasoning level shown in the header.
    pub(crate) default_reasoning_level: ReasoningLevel,
    /// Whether the session branch currently conflicts with its base branch.
    pub(crate) has_merge_conflict: bool,
    /// Current UI mode, which determines the reserved bottom-panel height.
    pub(crate) mode: &'a AppMode,
    /// Focused-review output for the rendered session.
    pub(crate) review_text: Option<&'a str>,
    /// Session whose transcript is rendered.
    pub(crate) session: &'a Session,
    /// Render-time clock used for the header's deterministic timers.
    pub(crate) wall_clock_unix_seconds: i64,
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

    // While the session is `InProgress` the composer queues a leading
    // `/` as plain text instead of running a slash command, so suppress
    // the slash dropdown to avoid implying the menu is actionable. The
    // `@` mention dropdown is still useful for editing queued messages.
    let suppress_slash_dropdown = session.status == crate::domain::session::Status::InProgress;
    let allow_apply_command = review::has_actionable_review_suggestions(review_text);
    let suggestion_list = if suppress_slash_dropdown && input.text().starts_with('/') {
        None
    } else {
        prompt_format::prompt_suggestion_list(
            input,
            slash_state,
            at_mention_state.as_ref(),
            session.agent.kind(),
            allow_apply_command,
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
        ),
        is_chat_focused: *focus == ChatFocus::Chat,
        status: Some(session_format::prompt_session_status(session)),
        suggestion_list,
        title: format!("[{}]", session.agent.model().as_str()),
        total_height: desired_bottom_height.min(max_bottom_height),
    })
}

/// Returns the bottom-panel height reserved for non-prompt page modes.
///
/// Question mode derives its height from the question layout helper and the
/// visible option list. All other modes reserve a single footer row.
fn non_prompt_bottom_height(area: Rect, mode: &AppMode) -> u16 {
    let AppMode::Question {
        questions,
        current_index,
        input,
        selected_option_index,
        ..
    } = mode
    else {
        return SINGLE_ROW_FOOTER_HEIGHT;
    };

    let question_item = questions.get(*current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
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
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
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
    } = *state;
    let question_item = questions.get(current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
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
        panel_areas.help_area.height,
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
    help_height: u16,
    focus: ChatFocus,
    has_session_diff: bool,
    is_navigating_options: bool,
    lookup_state: question_format::QuestionLookupState,
) {
    if help_height == 0 {
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
    let Some(menu) = at_mention_menu else {
        return;
    };

    let dropdown_height = suggestion_dropdown_height(menu.items.len());
    let Some(dropdown_area) = overlay_area_above(bottom_area, input_area, dropdown_height) else {
        return;
    };

    ChatInput::render_suggestion_dropdown(f, dropdown_area, &menu);
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
