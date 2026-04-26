use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::domain::agent::ReasoningLevel;
use crate::domain::input;
use crate::domain::session::Session;
use crate::infra::agent::protocol::QuestionItem;
use crate::ui::component::chat_input::{ChatInput, SuggestionList};
use crate::ui::component::session_output::{
    SessionOutput, SessionOutputLayoutCache, SessionOutputLineContext,
};
use crate::ui::state::app_mode::{AppMode, QuestionFocus};
use crate::ui::state::prompt::PromptAtMentionState;
use crate::ui::util::{
    calculate_input_height, overlay_area_above, question_panel_areas,
    question_panel_reserved_height, suggestion_dropdown_height,
};
use crate::ui::{Component, Page, layout, markdown};

/// Maximum rendered height of the prompt input panel, including borders.
const CHAT_INPUT_MAX_PANEL_HEIGHT: u16 = 10;

/// Prompt-panel data prepared once per render pass so layout and painting use
/// the same suggestion set.
struct PreparedPromptPanel {
    footer_text: Line<'static>,
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

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    pub active_prompt_output: Option<&'a str>,
    pub active_progress: Option<&'a str>,
    pub can_open_worktree: bool,
    pub default_reasoning_level: ReasoningLevel,
    /// Shared markdown cache reused across transcript renders in this page.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    pub mode: &'a AppMode,
    /// Shared fully assembled output-layout cache for scroll metrics and
    /// frame rendering.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    pub scroll_offset: Option<u16>,
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    pub sessions: &'a [Session],
    pub wall_clock_unix_seconds: i64,
}

/// Borrowed inputs needed to construct one session chat page renderer.
#[derive(Clone, Copy)]
pub struct SessionChatPageInput<'a> {
    /// Exact prompt transcript block for the currently active turn, when one
    /// has been submitted in this app process.
    pub active_prompt_output: Option<&'a str>,
    /// Transient progress text rendered in the active-status loader row.
    pub active_progress: Option<&'a str>,
    /// Active project-scoped default reasoning level.
    pub default_reasoning_level: ReasoningLevel,
    /// Shared render cache for session transcript markdown.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current UI mode that determines view, prompt, and question rendering.
    pub mode: &'a AppMode,
    /// Shared output-layout cache for this render pass.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    /// Current vertical output scroll offset.
    pub scroll_offset: Option<u16>,
    /// Index of the session being rendered.
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    /// Session rows available to the page.
    pub sessions: &'a [Session],
    /// Render-time clock used for deterministic timers.
    pub wall_clock_unix_seconds: i64,
}

impl<'a> SessionChatPage<'a> {
    /// Creates a session chat page renderer.
    pub fn new(input: SessionChatPageInput<'a>) -> Self {
        let SessionChatPageInput {
            active_prompt_output,
            active_progress,
            default_reasoning_level,
            markdown_render_cache,
            mode,
            output_layout_cache,
            scroll_offset,
            session_index,
            session_update_version,
            sessions,
            wall_clock_unix_seconds,
        } = input;

        Self {
            active_prompt_output,
            active_progress,
            can_open_worktree: false,
            default_reasoning_level,
            markdown_render_cache,
            mode,
            output_layout_cache,
            scroll_offset,
            session_index,
            session_update_version,
            sessions,
            wall_clock_unix_seconds,
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
    /// width.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering, including review text and generic active-status loaders, so
    /// scroll math can stay in sync with what users see.
    pub(crate) fn rendered_output_line_count(
        session: &Session,
        output_width: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: &markdown::MarkdownRenderCache,
        output_layout_cache: &SessionOutputLayoutCache,
    ) -> u16 {
        SessionOutput::rendered_line_count(
            session,
            output_width,
            context,
            Some(markdown_render_cache),
            Some(output_layout_cache),
        )
    }

    /// Returns focused-review status text for the active session transcript.
    ///
    /// Prompt mode preserves the last focused-review loader text so the output
    /// panel does not flicker when users open the composer and then cancel.
    fn review_status_message(&self) -> Option<&str> {
        match self.mode {
            AppMode::View {
                review_status_message,
                ..
            }
            | AppMode::Prompt {
                review_status_message,
                ..
            }
            | AppMode::Question {
                review_status_message,
                ..
            } => review_status_message.as_deref(),
            AppMode::OpenCommandSelector { restore_view, .. }
            | AppMode::PublishBranchInput { restore_view, .. }
            | AppMode::ViewInfoPopup { restore_view, .. } => {
                restore_view.review_status_message.as_deref()
            }
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => None,
        }
    }

    /// Returns focused-review text for the active session transcript.
    ///
    /// Prompt mode preserves the appended focused review until the next prompt
    /// is submitted so reopening the composer does not hide the review block.
    fn review_text(&self) -> Option<&str> {
        match self.mode {
            AppMode::View { review_text, .. }
            | AppMode::Prompt { review_text, .. }
            | AppMode::Question { review_text, .. } => review_text.as_deref(),
            AppMode::OpenCommandSelector { restore_view, .. }
            | AppMode::PublishBranchInput { restore_view, .. }
            | AppMode::ViewInfoPopup { restore_view, .. } => restore_view.review_text.as_deref(),
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. } => None,
        }
    }

    /// Prepares prompt-panel layout and suggestion data once for a render
    /// pass.
    fn prepare_prompt_panel(&self, area: Rect, session: &Session) -> Option<PreparedPromptPanel> {
        let AppMode::Prompt {
            at_mention_state,
            attachment_state,
            input,
            slash_state,
            ..
        } = self.mode
        else {
            return None;
        };

        // While the session is `InProgress` the composer queues a leading
        // `/` as plain text instead of running a slash command, so suppress
        // the slash dropdown to avoid implying the menu is actionable. The
        // `@` mention dropdown is still useful for editing queued messages.
        let suppress_slash_dropdown = session.status == crate::domain::session::Status::InProgress;
        let suggestion_list = if suppress_slash_dropdown && input.text().starts_with('/') {
            None
        } else {
            layout::prompt_suggestion_list(
                input,
                slash_state,
                at_mention_state.as_ref(),
                session.model.kind(),
            )
        };
        let dropdown_row_count = layout::prompt_suggestion_dropdown_rows(suggestion_list.as_ref());
        let input_height = calculate_input_height(area.width.saturating_sub(2), input.text())
            .min(CHAT_INPUT_MAX_PANEL_HEIGHT);
        let desired_bottom_height = input_height
            .saturating_add(u16::try_from(dropdown_row_count).unwrap_or(u16::MAX))
            .saturating_add(1);
        let max_bottom_height = area.height.saturating_sub(1);

        Some(PreparedPromptPanel {
            footer_text: layout::prompt_footer_line(session, attachment_state.attachments.len()),
            suggestion_list,
            title: format!(" [{}] ", session.model.as_str()),
            total_height: desired_bottom_height.min(max_bottom_height),
        })
    }

    /// Renders the session header, output panel, and context-aware bottom
    /// panel.
    fn render_session(&self, f: &mut Frame, area: Rect, session: &Session) {
        let prepared_prompt_panel = self.prepare_prompt_panel(area, session);
        let bottom_height = prepared_prompt_panel.as_ref().map_or_else(
            || self.bottom_height(area),
            PreparedPromptPanel::panel_height,
        );
        let session_areas = layout::session_chat_areas(area, bottom_height);

        let mut output = SessionOutput::new(session)
            .markdown_render_cache(self.markdown_render_cache)
            .output_layout_cache(self.output_layout_cache)
            .session_update_version(self.session_update_version);
        output = output.active_prompt_output(self.active_prompt_output);
        output = output.review_status_message(self.review_status_message());
        output = output.review_text(self.review_text());
        if let Some(scroll_offset) = self.scroll_offset {
            output = output.scroll_offset(scroll_offset);
        }
        if let Some(active_progress) = self.active_progress {
            output = output.active_progress(active_progress);
        }
        self.render_session_header(f, session_areas.header_area, session);
        output.render(f, session_areas.output_area);
        self.render_bottom_panel(
            f,
            session_areas.bottom_area,
            session,
            prepared_prompt_panel.as_ref(),
        );
    }

    /// Renders a standalone two-line header above the output panel border.
    fn render_session_header(&self, f: &mut Frame, header_area: Rect, session: &Session) {
        let header = Paragraph::new(layout::session_header_lines(
            session,
            header_area.width,
            self.default_reasoning_level,
            self.wall_clock_unix_seconds,
        ));

        f.render_widget(header, header_area);
    }

    /// Returns the reserved bottom-panel height for the active page mode.
    ///
    /// Prompt mode mirrors the render-time prompt panel calculation so layout
    /// tests and the live view stay in sync. Question mode derives its height
    /// from the question layout helper and the visible option list. All other
    /// modes reserve a single footer row.
    fn bottom_height(&self, area: Rect) -> u16 {
        if matches!(self.mode, AppMode::Prompt { .. }) {
            let Some(session) = self.sessions.get(self.session_index) else {
                return 1;
            };

            return self
                .prepare_prompt_panel(area, session)
                .map_or(1, |prepared_prompt_panel| {
                    prepared_prompt_panel.panel_height()
                });
        }

        if let AppMode::Question {
            questions,
            current_index,
            input,
            selected_option_index,
            ..
        } = self.mode
        {
            let question_item = questions.get(*current_index);
            let question = question_item.map_or("", |item| item.text.as_str());
            let options = question_item
                .map(|item| item.options.as_slice())
                .unwrap_or_default();
            let is_free_text_mode = selected_option_index.is_none();
            let input_text = if is_free_text_mode { input.text() } else { "" };
            return question_panel_reserved_height(
                area.width,
                area.height.saturating_sub(1),
                question,
                input_text,
                options.len(),
                CHAT_INPUT_MAX_PANEL_HEIGHT,
            );
        }

        1
    }

    /// Renders the context-aware bottom panel for prompt and question modes.
    fn render_bottom_panel(
        &self,
        f: &mut Frame,
        bottom_area: Rect,
        session: &Session,
        prepared_prompt_panel: Option<&PreparedPromptPanel>,
    ) {
        if let AppMode::Prompt { input, .. } = self.mode {
            let Some(prepared_prompt_panel) = prepared_prompt_panel else {
                return;
            };

            let mut chat_input =
                ChatInput::new(&prepared_prompt_panel.title, input.text(), input.cursor)
                    .placeholder("Type your message");

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
                &QuestionPanelState {
                    at_mention_state: at_mention_state.as_ref(),
                    current_index: *current_index,
                    focus: *focus,
                    input,
                    questions,
                    selected_option_index: *selected_option_index,
                },
            );

            return;
        }

        let help_message = Paragraph::new(layout::session_view_footer_line(
            session,
            self.can_open_worktree,
        ));
        f.render_widget(help_message, bottom_area);
    }
}

/// Bundled question-mode state passed to the panel renderer.
#[derive(Clone, Copy)]
struct QuestionPanelState<'a> {
    at_mention_state: Option<&'a PromptAtMentionState>,
    current_index: usize,
    focus: QuestionFocus,
    input: &'a input::InputState,
    questions: &'a [QuestionItem],
    selected_option_index: Option<usize>,
}

/// Renders the question-mode bottom panel with question text, options, input,
/// and help footer.
fn render_question_panel(f: &mut Frame, bottom_area: Rect, state: &QuestionPanelState<'_>) {
    let QuestionPanelState {
        at_mention_state,
        current_index,
        focus,
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
    let input_text = if is_free_text_mode { input.text() } else { "" };
    let panel_areas = question_panel_areas(
        bottom_area,
        question,
        input_text,
        options.len(),
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    );

    let is_chat_focused = focus == QuestionFocus::Chat;
    let question_title = format!("Question {}/{}", current_index + 1, questions.len());
    if panel_areas.question_area.height > 0 {
        let question_para = Paragraph::new(layout::question_panel_lines(
            &question_title,
            question,
            is_chat_focused,
            bottom_area.width,
        ));
        f.render_widget(question_para, panel_areas.question_area);
    }

    if panel_areas.options_area.height > 0 {
        f.render_widget(
            Paragraph::new(layout::question_option_lines(
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
    let input_placeholder = "Type answer (Enter: send, Esc: end turn)";
    let at_mention_max_visible =
        layout::question_at_mention_max_visible(bottom_area, panel_areas.input_area, 10);
    let at_mention_menu = if is_free_text_mode {
        at_mention_state.and_then(|state| {
            layout::file_lookup_suggestion_list(
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

    render_question_at_mention_overlay(f, bottom_area, panel_areas.input_area, at_mention_menu);
    render_question_help_footer(
        f,
        panel_areas.help_area,
        panel_areas.help_area.height,
        focus,
    );
}

/// Renders the question-mode help footer with context-aware action hints.
fn render_question_help_footer(f: &mut Frame, area: Rect, help_height: u16, focus: QuestionFocus) {
    if help_height == 0 {
        return;
    }

    let help_para = Paragraph::new(layout::question_help_footer_line(focus))
        .alignment(ratatui::layout::Alignment::Right);
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
mod tests {
    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::input::InputState;
    use crate::domain::session::Status;
    use crate::infra::agent::protocol::QuestionItem;
    use crate::ui::state::app_mode::QuestionFocus;
    use crate::ui::state::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};

    fn session_fixture() -> Session {
        crate::domain::session::tests::SessionFixtureBuilder::new()
            .folder(std::env::temp_dir())
            .status(Status::New)
            .build()
    }

    /// Builds a default test page for one session and mode.
    fn test_session_chat_page<'a>(session: &'a Session, mode: &'a AppMode) -> SessionChatPage<'a> {
        SessionChatPage::new(SessionChatPageInput {
            active_prompt_output: None,
            active_progress: None,
            default_reasoning_level: ReasoningLevel::default(),
            markdown_render_cache: test_markdown_render_cache(),
            mode,
            output_layout_cache: test_output_layout_cache(),
            scroll_offset: None,
            session_index: 0,
            session_update_version: 0,
            sessions: std::slice::from_ref(session),
            wall_clock_unix_seconds: 0,
        })
    }

    /// Returns a leaked markdown cache for test page builders that need a
    /// stable borrow across the page lifetime.
    fn test_markdown_render_cache() -> &'static markdown::MarkdownRenderCache {
        Box::leak(Box::new(markdown::MarkdownRenderCache::default()))
    }

    /// Returns a leaked output-layout cache for test page builders that need a
    /// stable borrow across the page lifetime.
    fn test_output_layout_cache() -> &'static SessionOutputLayoutCache {
        Box::leak(Box::new(SessionOutputLayoutCache::default()))
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        let start = usize::from(row) * usize::from(width);
        let end = start + usize::from(width);

        buffer.content()[start..end]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn test_status_bar_fyi_rotates_between_session_chat_messages() {
        // Arrange
        let expected_messages = [
            "Press ? to inspect the shortcuts available for the current session state.",
            "Press / to open slash commands without typing into the composer first.",
            "Press d to review the current worktree diff for this session.",
            "Use @ to attach files or project context to the next prompt.",
            "You can queue new messages while a session is in progress.",
            "Press o to open the session worktree when one is available.",
            "Agentty starts focused review automatically after each turn.",
            "Press f to open or regenerate focused review output on demand.",
            "Type /apply after focused review completes to verify and apply its suggestions.",
        ];

        // Act
        let actual_messages = crate::ui::page::fyi::session_chat_messages();
        let wrapped_message = crate::ui::page::fyi::rotating_message(
            actual_messages,
            u64::try_from(expected_messages.len()).unwrap_or_default(),
        );

        // Assert
        assert_eq!(actual_messages, expected_messages);
        assert_eq!(
            wrapped_message,
            Some("Press ? to inspect the shortcuts available for the current session state.")
        );
    }

    #[test]
    fn test_rendered_output_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.output = "word ".repeat(40);
        let raw_line_count = u16::try_from(session.output.lines().count()).unwrap_or(u16::MAX);
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();

        // Act
        let rendered_line_count = SessionChatPage::rendered_output_line_count(
            &session,
            20,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                review_status_message: None,
                review_text: None,
                session_update_version: 0,
            },
            &markdown_render_cache,
            &output_layout_cache,
        );

        // Assert
        assert!(rendered_line_count > raw_line_count);
    }

    #[test]
    fn test_review_text_reads_preserved_prompt_review_output() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::default(),
            review_status_message: None,
            review_text: Some("Focused review".to_string()),
            slash_state: PromptSlashState::default(),
            session_id: "session-id".into(),
            input: InputState::default(),
            scroll_offset: None,
        };
        let page = test_session_chat_page(&session, &mode);

        // Act
        let review_text = page.review_text();

        // Assert
        assert_eq!(review_text, Some("Focused review"));
    }

    #[test]
    fn test_review_text_reads_preserved_question_review_output() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            review_status_message: Some("Preparing review...".to_string()),
            review_text: Some("Focused review".to_string()),
            session_id: "session-id".into(),
            questions: vec![QuestionItem::new("Need tests?")],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };
        let page = test_session_chat_page(&session, &mode);

        // Act
        let review_text = page.review_text();
        let review_status_message = page.review_status_message();

        // Assert
        assert_eq!(review_text, Some("Focused review"));
        assert_eq!(review_status_message, Some("Preparing review..."));
    }

    #[test]
    fn test_rendered_output_line_count_includes_question_review_output() {
        // Arrange
        let session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let without_review = SessionChatPage::rendered_output_line_count(
            &session,
            40,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                review_status_message: None,
                review_text: None,
                session_update_version: 0,
            },
            &markdown_render_cache,
            &output_layout_cache,
        );

        // Act
        let with_review = SessionChatPage::rendered_output_line_count(
            &session,
            40,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                review_status_message: Some("Preparing review..."),
                review_text: Some("## Review\n\n- Focused finding"),
                session_update_version: 0,
            },
            &markdown_render_cache,
            &output_layout_cache,
        );

        // Assert
        assert!(with_review > without_review);
    }

    #[test]
    fn test_render_question_mode_keeps_typed_answer_visible_in_tight_layout() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            review_status_message: None,
            review_text: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Need a detailed migration plan with rollback guidance?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::with_text("typed answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(32, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("typed answer"));
    }

    #[test]
    fn test_render_question_mode_includes_blank_row_between_question_and_input() {
        // Arrange
        let session = session_fixture();
        let question = "Need a detailed migration plan with rollback guidance?".to_string();
        let mode = AppMode::Question {
            at_mention_state: None,
            review_status_message: None,
            review_text: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: question.clone(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::with_text("typed answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let width = 40;
        let height = 12;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode");

        // Assert
        let area = Rect::new(0, 0, width, height);
        let bottom_height = page.bottom_height(area);
        let bottom_top = 1 + height.saturating_sub(2).saturating_sub(bottom_height);
        let panel_areas = question_panel_areas(
            Rect::new(1, bottom_top, width.saturating_sub(2), bottom_height),
            &question,
            "typed answer",
            0,
            CHAT_INPUT_MAX_PANEL_HEIGHT,
        );
        let spacer_row = panel_areas.spacer_area.y;
        let spacer_text = buffer_row_text(terminal.backend().buffer(), spacer_row, width);
        assert!(spacer_text.trim().is_empty());
    }

    #[test]
    fn test_render_question_mode_with_options_shows_option_text() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            review_status_message: None,
            review_text: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Continue?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        };
        let mut page = test_session_chat_page(&session, &mode);
        let width = 50;
        let height = 14;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode with options");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Options:"), "should render options header");
        assert!(text.contains("Yes"), "should render first option");
        assert!(text.contains("No"), "should render second option");
    }

    #[test]
    fn test_render_question_mode_with_options_in_small_terminal_does_not_panic() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            review_status_message: None,
            review_text: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: vec![
                    "A".to_string(),
                    "B".to_string(),
                    "C".to_string(),
                    "D".to_string(),
                    "E".to_string(),
                ],
                text: "Pick one of the many options?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(30, 6);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act + Assert (should not panic)
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode with many options in small terminal");
    }
}
