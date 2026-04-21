use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Borders;

use crate::domain::agent::ReasoningLevel;
use crate::domain::input::{is_at_mention_boundary, is_at_mention_query_character};
use crate::domain::session::{PublishedBranchSyncStatus, Session, Status};
use crate::icon::Icon;
use crate::ui::state::app_mode::{DoneSessionOutputMode, QuestionFocus};
use crate::ui::state::help_action::{self, ViewHelpState, ViewSessionState};
use crate::ui::{markdown, style, text_util};

/// Maximum number of visible content lines inside the chat input viewport.
pub const CHAT_INPUT_MAX_VISIBLE_LINES: u16 = 10;

const CHAT_INPUT_BORDER_HEIGHT: u16 = 2;
const CHAT_INPUT_MIN_PANEL_HEIGHT: u16 = CHAT_INPUT_BORDER_HEIGHT + 1;
const CHAT_INPUT_INNER_OFFSET: u16 = 1;
const CHAT_INPUT_PROMPT_PREFIX_WIDTH: u16 = 3;
const QUESTION_PANEL_HELP_HEIGHT: u16 = 1;
const QUESTION_PANEL_SPACER_HEIGHT: u16 = 1;
const SESSION_HEADER_HEIGHT: u16 = 2;
const SLASH_MENU_BORDER_HEIGHT: u16 = 2;
/// Foreground color used for chat input `@` lookup tokens.
const CHAT_INPUT_AT_MENTION_COLOR: Color = Color::LightBlue;
/// Foreground color used for inline prompt image placeholders.
const CHAT_INPUT_IMAGE_TOKEN_COLOR: Color = Color::Yellow;
const SINGLE_LINE_FOOTER_HEIGHT: u16 = 1;

const NEW_SESSION_PROMPT_FOOTER_ACTIONS: [help_action::HelpAction; 4] = [
    help_action::HelpAction::new("stage draft", "Enter", "Stage draft"),
    help_action::HelpAction::new("newline", "Alt+Enter", "Insert newline"),
    help_action::HelpAction::new("paste image", "Ctrl+V/Alt+V", "Paste image"),
    help_action::HelpAction::new("cancel", "Esc", "Cancel prompt"),
];
const PROMPT_FOOTER_ACTIONS: [help_action::HelpAction; 4] = [
    help_action::HelpAction::new("submit", "Enter", "Submit prompt"),
    help_action::HelpAction::new("newline", "Alt+Enter", "Insert newline"),
    help_action::HelpAction::new("paste image", "Ctrl+V/Alt+V", "Paste image"),
    help_action::HelpAction::new("cancel", "Esc", "Cancel prompt"),
];

/// Height allocation for question mode's prompt, answer input, and footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuestionPanelLayout {
    /// Height reserved for the trailing help footer row.
    pub help_height: u16,
    /// Height reserved for the bordered answer input widget.
    pub input_height: u16,
    /// Height reserved for the question title and wrapped question text.
    pub question_height: u16,
    /// Height reserved for the blank spacer between question and input.
    pub spacer_height: u16,
}

/// Concrete sub-areas used to paint the question-mode bottom panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuestionPanelAreas {
    /// Area used for the footer help text row.
    pub help_area: Rect,
    /// Area used for the bordered answer input widget.
    pub input_area: Rect,
    /// Area used for the predefined answer options.
    pub options_area: Rect,
    /// Area used for the question title and wrapped question text.
    pub question_area: Rect,
    /// Area used for the blank spacer between question text and input.
    pub spacer_area: Rect,
}

/// Top-level frame areas for one rendered session chat page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionChatAreas {
    /// Area used for the footer/help row below the session transcript.
    pub bottom_area: Rect,
    /// Area used for the two-line title and metadata header.
    pub header_area: Rect,
    /// Area used for the bordered transcript/output panel.
    pub output_area: Rect,
}

/// Sub-areas used to render the prompt composer and its footer help row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptPanelAreas {
    /// Area used for the footer/help row below the composer.
    pub footer_area: Rect,
    /// Area used for the bordered prompt input widget.
    pub input_area: Rect,
}

/// Split an area into a centered content column with side gutters.
pub fn centered_horizontal_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(2),
            Constraint::Percentage(80),
            Constraint::Min(2),
        ])
        .split(area)
}

/// Returns a fixed-size content area centered within the available `area`.
///
/// The returned rectangle is clamped to the bounds of `area` so callers can
/// safely request a preferred content size even when the terminal is smaller.
pub fn centered_content_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let origin_x = area.x + area.width.saturating_sub(width) / 2;
    let origin_y = area.y + area.height.saturating_sub(height) / 2;

    Rect::new(origin_x, origin_y, width, height)
}

/// Splits one session chat page into header, transcript, and bottom panel.
///
/// The outer frame keeps a one-cell margin, reserves `bottom_height` for the
/// prompt/help region, and then dedicates a fixed two-row header above the
/// bordered session output panel.
pub fn session_chat_areas(area: Rect, bottom_height: u16) -> SessionChatAreas {
    let vertical_chunks = Layout::default()
        .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
        .margin(1)
        .split(area);
    let output_chunks = Layout::default()
        .constraints([
            Constraint::Length(SESSION_HEADER_HEIGHT),
            Constraint::Min(0),
        ])
        .split(vertical_chunks[0]);

    SessionChatAreas {
        bottom_area: vertical_chunks[1],
        header_area: output_chunks[0],
        output_area: output_chunks[1],
    }
}

/// Splits one prompt bottom panel into input and footer rows.
///
/// Panels that only have one visible row keep the whole area for the input and
/// collapse the footer to height `0`.
pub fn prompt_panel_areas(area: Rect) -> PromptPanelAreas {
    if area.height <= SINGLE_LINE_FOOTER_HEIGHT {
        return PromptPanelAreas {
            footer_area: Rect::new(area.x, area.y.saturating_add(area.height), area.width, 0),
            input_area: area,
        };
    }

    let sections = Layout::default()
        .constraints([
            Constraint::Min(0),
            Constraint::Length(SINGLE_LINE_FOOTER_HEIGHT),
        ])
        .split(area);

    PromptPanelAreas {
        footer_area: sections[1],
        input_area: sections[0],
    }
}

/// Formats the session title and metadata lines rendered above the output
/// panel.
pub fn session_header_lines(
    session: &Session,
    header_width: u16,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> Vec<Line<'static>> {
    let title_width = usize::from(header_width);
    let title_text = text_util::inline_text(session.display_title());
    let base_style = Style::default()
        .fg(session.status.color())
        .add_modifier(Modifier::BOLD);
    let title_spans = markdown::parse_inline_spans(&title_text, base_style);
    let title_spans = text_util::truncate_spans_with_ellipsis(title_spans, title_width);
    let metadata_text = session_metadata_text(
        session,
        header_width,
        default_reasoning_level,
        wall_clock_unix_seconds,
    );

    vec![
        Line::from(title_spans),
        Line::from(Span::styled(
            metadata_text,
            Style::default().fg(style::palette::TEXT_MUTED),
        )),
    ]
}

/// Formats the size, timer, token usage, model, and reasoning row shown in
/// the session header.
pub fn session_metadata_text(
    session: &Session,
    header_width: u16,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> String {
    let added_lines = session.stats.added_lines;
    let deleted_lines = session.stats.deleted_lines;
    let timer = text_util::format_duration_compact(
        session.in_progress_duration_seconds(wall_clock_unix_seconds),
    );
    let reasoning_level = session.effective_reasoning_level(default_reasoning_level);
    let input_tokens = text_util::format_token_count(session.stats.input_tokens);
    let output_tokens = text_util::format_token_count(session.stats.output_tokens);
    let metadata = format!(
        "Size: {}  Lines: +{added_lines} / -{deleted_lines}  Timer: {timer}  Model: {}  \
         Reasoning: {}  Tokens: {input_tokens}/{output_tokens}",
        session.size,
        session.model.as_str(),
        reasoning_level.as_str(),
    );
    let metadata_width = usize::from(header_width);

    text_util::truncate_with_ellipsis(&metadata, metadata_width)
}

/// Builds the footer help line shown in session view mode.
pub fn session_view_footer_line(
    session: &Session,
    can_open_worktree: bool,
    done_session_output_mode: DoneSessionOutputMode,
) -> Line<'static> {
    help_action::footer_line(&session_view_footer_actions(
        session,
        can_open_worktree,
        done_session_output_mode,
    ))
}

/// Builds the prompt-mode footer help line shown below the composer.
pub fn prompt_footer_line(session: &Session, attachment_count: usize) -> Line<'static> {
    let mut footer_line = help_action::footer_line(prompt_footer_actions(session));

    if attachment_count > 0 {
        let suffix = if attachment_count == 1 { "" } else { "s" };
        footer_line.spans.push(help_action::footer_separator_span());
        footer_line
            .spans
            .push(help_action::footer_muted_span(format!(
                "{attachment_count} image{suffix} ready"
            )));
    }

    footer_line
}

/// Builds the question-mode help footer line for the current focus target.
pub fn question_help_footer_line(focus: QuestionFocus) -> Line<'static> {
    let is_chat_focused = focus == QuestionFocus::Chat;
    let mut help_actions = Vec::new();

    if is_chat_focused {
        help_actions.push(help_action::HelpAction::new("scroll", "j/k", "Scroll chat"));
        help_actions.push(help_action::HelpAction::new("diff", "d", "Diff"));
        help_actions.push(help_action::HelpAction::new(
            "answer",
            "Esc/Enter",
            "Answer",
        ));
    } else {
        help_actions.push(help_action::HelpAction::new("send", "Enter", "Submit"));
    }

    let focus_label = if is_chat_focused { "Answer" } else { "Chat" };
    help_actions.push(help_action::HelpAction::new("focus", "Tab", focus_label));

    if !is_chat_focused {
        help_actions.push(help_action::HelpAction::new("end turn", "Esc", "End turn"));
    }

    help_action::footer_line(&help_actions)
}

/// Returns borders used for the session output panel.
///
/// Vertical borders stay hidden so terminal copy/select flows do not pick up
/// extra gutter characters.
pub fn session_output_panel_borders() -> Borders {
    Borders::TOP | Borders::BOTTOM
}

/// Returns the border style used for the session output panel.
pub fn session_output_panel_border_style(status: Status) -> Style {
    Style::default().fg(style::status_color(status))
}

/// Builds the inline shortcut hint for toggling done-session content.
pub fn session_output_done_toggle_line(
    done_session_output_mode: DoneSessionOutputMode,
) -> Line<'static> {
    let toggle_target = done_toggle_action_label(done_session_output_mode);

    Line::from(vec![Span::styled(
        format!("Press t to switch to {toggle_target}."),
        Style::default().fg(style::palette::TEXT_SUBTLE),
    )])
}

/// Builds the active-status line shown at the end of an in-flight session
/// transcript.
pub fn session_output_status_line(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
) -> Option<Line<'static>> {
    if !matches!(
        status,
        Status::InProgress
            | Status::AgentReview
            | Status::Queued
            | Status::Rebasing
            | Status::Merging
    ) {
        return None;
    }

    let status_message =
        session_output_status_message(status, active_progress, review_status_message);

    Some(Line::from(vec![Span::styled(
        format!("{} {status_message}", session_output_status_icon(status)),
        Style::default().fg(style::status_color(status)),
    )]))
}

/// Builds the published-branch sync status line appended after transcript
/// content when auto-push state is available.
pub fn session_output_published_branch_sync_line(session: &Session) -> Option<Line<'static>> {
    let sync_message = session.published_branch_sync_message()?;

    Some(Line::from(vec![Span::styled(
        format!(
            "{} {sync_message}",
            session_output_published_branch_sync_icon(session.published_branch_sync_status)
        ),
        Style::default().fg(session_output_published_branch_sync_color(
            session.published_branch_sync_status,
        )),
    )]))
}

/// Calculate the chat input widget height with a capped visible viewport.
///
/// The returned height includes top and bottom borders and limits the visible
/// content area to [`CHAT_INPUT_MAX_VISIBLE_LINES`]. `width` is the full widget
/// width, including borders.
pub fn calculate_input_height(width: u16, input: &str) -> u16 {
    let char_count = input.chars().count();
    let (_, _, cursor_y) = compute_input_layout(input, width, char_count);

    let content_line_count = cursor_y.saturating_add(1);

    content_line_count
        .min(CHAT_INPUT_MAX_VISIBLE_LINES)
        .saturating_add(CHAT_INPUT_BORDER_HEIGHT)
}

/// Calculates the height split for question mode's prompt, input, and footer.
///
/// The answer input keeps its visible content row whenever `available_height`
/// allows it. When space is tight, the wrapped question text yields height
/// before the spacer row and bordered input collapse.
pub fn question_panel_layout(
    width: u16,
    available_height: u16,
    question: &str,
    input: &str,
    max_input_panel_height: u16,
) -> QuestionPanelLayout {
    let question_text_height = wrapped_text_height(question, width);
    // +1 for the "Question N/M" title line when there is question text.
    let requested_question_height = if question_text_height > 0 {
        question_text_height.saturating_add(1)
    } else {
        0
    };
    let requested_input_height = calculate_input_height(width, input)
        .min(max_input_panel_height.max(CHAT_INPUT_MIN_PANEL_HEIGHT));
    let requested_spacer_height = if requested_question_height > 0 {
        QUESTION_PANEL_SPACER_HEIGHT
    } else {
        0
    };
    let preferred_total_height = requested_question_height
        .saturating_add(requested_input_height)
        .saturating_add(requested_spacer_height)
        .saturating_add(QUESTION_PANEL_HELP_HEIGHT);
    let total_height = preferred_total_height.min(available_height);
    let help_height = total_height.min(QUESTION_PANEL_HELP_HEIGHT);
    let remaining_height = total_height.saturating_sub(help_height);
    let input_height = remaining_height.min(requested_input_height);
    let question_and_spacer_height = remaining_height.saturating_sub(input_height);
    let spacer_height =
        if question_and_spacer_height >= requested_question_height + requested_spacer_height {
            requested_spacer_height
        } else {
            0
        };
    let question_height = question_and_spacer_height.saturating_sub(spacer_height);

    QuestionPanelLayout {
        help_height,
        input_height,
        question_height,
        spacer_height,
    }
}

/// Returns the total height reserved for a question-mode bottom panel.
///
/// The caller supplies the maximum panel height budget; this helper accounts
/// for question text, predefined options, the answer input, spacer, and help
/// footer using the same allocation logic used during rendering.
pub fn question_panel_reserved_height(
    width: u16,
    available_height: u16,
    question: &str,
    input: &str,
    option_count: usize,
    max_input_panel_height: u16,
) -> u16 {
    let options_height = question_options_height(option_count, available_height);
    let panel_layout = question_panel_layout(
        width,
        available_height.saturating_sub(options_height),
        question,
        input,
        max_input_panel_height,
    );

    panel_layout
        .question_height
        .saturating_add(options_height)
        .saturating_add(panel_layout.spacer_height)
        .saturating_add(panel_layout.input_height)
        .saturating_add(panel_layout.help_height)
}

/// Splits an already reserved question-mode bottom area into paintable rows.
///
/// The returned rectangles mirror [`question_panel_reserved_height`] so render
/// code can consume precomputed geometry instead of repeating the area math.
pub fn question_panel_areas(
    area: Rect,
    question: &str,
    input: &str,
    option_count: usize,
    max_input_panel_height: u16,
) -> QuestionPanelAreas {
    let options_height = question_options_height(option_count, area.height);
    let panel_layout = question_panel_layout(
        area.width,
        area.height.saturating_sub(options_height),
        question,
        input,
        max_input_panel_height,
    );
    let chunks = Layout::default()
        .constraints([
            Constraint::Length(panel_layout.question_height),
            Constraint::Length(options_height),
            Constraint::Length(panel_layout.spacer_height),
            Constraint::Length(panel_layout.input_height),
            Constraint::Length(panel_layout.help_height),
        ])
        .split(area);

    QuestionPanelAreas {
        help_area: chunks[4],
        input_area: chunks[3],
        options_area: chunks[1],
        question_area: chunks[0],
        spacer_area: chunks[2],
    }
}

/// Returns the total height for a question-mode options section.
///
/// The section contains one header row plus one row per predefined option and
/// clamps to `max_height` so callers can preserve space for surrounding UI.
pub fn question_options_height(option_count: usize, max_height: u16) -> u16 {
    if option_count == 0 {
        return 0;
    }

    u16::try_from(option_count)
        .unwrap_or(u16::MAX)
        .saturating_add(1)
        .min(max_height)
}

/// Compute chat input lines and the cursor position for rendering.
///
/// The first line starts with the visible prompt prefix (` › `). Continuation
/// lines (from wrapping or explicit newlines) keep the same horizontal padding
/// as spaces, so text never appears under the prompt icon. Wrapping prefers
/// whole words when possible; only words that exceed the full content width are
/// split across lines. Tokens that start with `@` at a lookup boundary are
/// highlighted to make file lookup references easy to spot while typing.
/// Lookup boundaries include the start of the line, whitespace, and opening
/// delimiters such as `(`. Highlighting continues only while the token is
/// composed of path-like characters (`[A-Za-z0-9/._-]`), so trailing
/// punctuation is excluded.
pub fn compute_input_layout(
    input: &str,
    width: u16,
    cursor: usize,
) -> (Vec<Line<'static>>, u16, u16) {
    let input_layout = compute_input_layout_data(input, width);
    let clamped_cursor = cursor.min(input_layout.cursor_positions.len().saturating_sub(1));
    let (cursor_x, cursor_y) = input_layout.cursor_positions[clamped_cursor];

    (
        input_layout.display_lines,
        u16::try_from(cursor_x).unwrap_or(u16::MAX),
        u16::try_from(cursor_y).unwrap_or(u16::MAX),
    )
}

/// Calculate the input viewport scroll offset and cursor row inside it.
///
/// Returns `(scroll_offset, cursor_row)` where:
/// - `scroll_offset` is the number of content lines hidden above the viewport.
/// - `cursor_row` is the cursor's row relative to the viewport top.
pub fn calculate_input_viewport(
    total_line_count: usize,
    cursor_y: u16,
    viewport_height: u16,
) -> (u16, u16) {
    if viewport_height == 0 {
        return (0, 0);
    }

    let total_line_count = u16::try_from(total_line_count).unwrap_or(u16::MAX).max(1);
    let clamped_cursor_y = cursor_y.min(total_line_count.saturating_sub(1));
    let viewport_height = viewport_height.min(total_line_count);
    let max_scroll = total_line_count.saturating_sub(viewport_height);
    let scroll_offset = clamped_cursor_y
        .saturating_sub(viewport_height.saturating_sub(1))
        .min(max_scroll);
    let cursor_row = clamped_cursor_y.saturating_sub(scroll_offset);

    (scroll_offset, cursor_row)
}

/// Returns an overlay rectangle directly above `anchor_area` when one fits.
///
/// The overlay is clamped to the vertical space available inside
/// `container_area` and preserves the anchor width and horizontal origin.
pub fn overlay_area_above(
    container_area: Rect,
    anchor_area: Rect,
    desired_height: u16,
) -> Option<Rect> {
    let available_above = anchor_area.y.saturating_sub(container_area.y);
    let clamped_height = desired_height.min(available_above);
    if clamped_height == 0 {
        return None;
    }

    Some(Rect::new(
        anchor_area.x,
        anchor_area.y.saturating_sub(clamped_height),
        anchor_area.width,
        clamped_height,
    ))
}

/// Returns the inner text width of a bordered panel area.
///
/// Horizontal space consumed by left and right borders is excluded from the
/// result so markdown wrapping and other width-based rendering stay aligned
/// with the actual drawable region.
pub fn panel_inner_width(area: Rect, borders: Borders) -> usize {
    let left_border_width = u16::from(borders.intersects(Borders::LEFT));
    let right_border_width = u16::from(borders.intersects(Borders::RIGHT));

    usize::from(
        area.width
            .saturating_sub(left_border_width)
            .saturating_sub(right_border_width),
    )
}

/// Returns the bottom-pinned scroll offset for a bordered panel.
///
/// When `scroll_offset` is already set, that explicit value wins. Otherwise
/// the content is scrolled just enough to keep the newest lines visible within
/// the panel's inner height after top and bottom borders are removed.
pub fn bottom_pinned_scroll_offset(
    area: Rect,
    borders: Borders,
    line_count: usize,
    scroll_offset: Option<u16>,
) -> u16 {
    if let Some(scroll_offset) = scroll_offset {
        return scroll_offset;
    }

    let top_border_height = u16::from(borders.intersects(Borders::TOP));
    let bottom_border_height = u16::from(borders.intersects(Borders::BOTTOM));
    let inner_height = usize::from(
        area.height
            .saturating_sub(top_border_height)
            .saturating_sub(bottom_border_height),
    );

    u16::try_from(line_count.saturating_sub(inner_height)).unwrap_or(u16::MAX)
}

/// Calculates the cursor position for an empty chat input with placeholder
/// text.
pub fn placeholder_cursor_position(area: Rect) -> (u16, u16) {
    (
        area.x
            .saturating_add(CHAT_INPUT_INNER_OFFSET)
            .saturating_add(CHAT_INPUT_PROMPT_PREFIX_WIDTH),
        area.y.saturating_add(CHAT_INPUT_INNER_OFFSET),
    )
}

/// Calculates the cursor position for chat input content rendered inside the
/// bordered input block.
pub fn input_cursor_position(area: Rect, cursor_x: u16, cursor_row: u16) -> (u16, u16) {
    (
        area.x
            .saturating_add(CHAT_INPUT_INNER_OFFSET)
            .saturating_add(cursor_x),
        area.y
            .saturating_add(CHAT_INPUT_INNER_OFFSET)
            .saturating_add(cursor_row),
    )
}

/// Calculates suggestion-dropdown height, including top and bottom borders.
pub fn suggestion_dropdown_height(option_count: usize) -> u16 {
    u16::try_from(option_count)
        .unwrap_or(u16::MAX)
        .saturating_add(SLASH_MENU_BORDER_HEIGHT)
}

/// Move the cursor one visual line up in the wrapped chat input layout.
pub fn move_input_cursor_up(input: &str, width: u16, cursor: usize) -> usize {
    move_input_cursor_vertical(input, width, cursor, VerticalDirection::Up)
}

/// Move the cursor one visual line down in the wrapped chat input layout.
pub fn move_input_cursor_down(input: &str, width: u16, cursor: usize) -> usize {
    move_input_cursor_vertical(input, width, cursor, VerticalDirection::Down)
}

/// Calculate the rendered width of the first table column.
///
/// This mirrors ratatui's table layout behavior, including highlight selection
/// space and column spacing.
pub fn first_table_column_width(
    table_width: u16,
    column_constraints: &[Constraint],
    column_spacing: u16,
    selection_width: u16,
) -> usize {
    if column_constraints.is_empty() {
        return 0;
    }

    let [_selection_area, columns_area] =
        Layout::horizontal([Constraint::Length(selection_width), Constraint::Fill(0)])
            .areas(Rect::new(0, 0, table_width, 1));
    let columns = Layout::horizontal(column_constraints.iter().copied())
        .spacing(column_spacing)
        .split(columns_area);

    columns
        .first()
        .map_or(0, |column| usize::from(column.width))
}

/// Returns the footer action list for one session view state.
fn session_view_footer_actions(
    session: &Session,
    can_open_worktree: bool,
    done_session_output_mode: DoneSessionOutputMode,
) -> Vec<help_action::HelpAction> {
    let session_state = help_action::session_view_state(session);
    let mut actions = help_action::view_footer_actions(ViewHelpState {
        can_open_worktree,
        publish_pull_request_action: session.publish_pull_request_action(),
        session_state,
    });

    if session_state == ViewSessionState::Done {
        let toggle_action_label = done_toggle_action_label(done_session_output_mode);
        if let Some(toggle_action_index) = actions.iter().position(|action| action.key == "t") {
            actions[toggle_action_index] =
                help_action::HelpAction::new(toggle_action_label, "t", "Switch summary/output");
        }
    }

    actions
}

/// Returns the `t` footer label for `Status::Done` output mode toggling.
fn done_toggle_action_label(done_session_output_mode: DoneSessionOutputMode) -> &'static str {
    match done_session_output_mode {
        DoneSessionOutputMode::Summary => "output",
        DoneSessionOutputMode::Output | DoneSessionOutputMode::Review => "summary",
    }
}

/// Returns the fixed prompt-mode actions rendered in the composer footer.
fn prompt_footer_actions(session: &Session) -> &'static [help_action::HelpAction] {
    if session.status == Status::New && session.is_draft_session() {
        return &NEW_SESSION_PROMPT_FOOTER_ACTIONS;
    }

    &PROMPT_FOOTER_ACTIONS
}

/// Returns the wrapped line count for plain text rendered in a paragraph.
fn wrapped_text_height(text: &str, width: u16) -> u16 {
    let wrapped_line_count = text_util::wrap_lines(text, usize::from(width.max(1))).len();

    u16::try_from(wrapped_line_count).unwrap_or(u16::MAX).max(1)
}

/// Returns the loader label for active session states.
fn session_output_status_message(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
) -> String {
    match status {
        Status::InProgress => active_progress
            .map(str::trim)
            .filter(|progress| !progress.is_empty())
            .map_or_else(
                || "Working...".to_string(),
                |progress| format!("Working... {progress}"),
            ),
        Status::AgentReview => review_status_message
            .map(str::trim)
            .filter(|status_message| !status_message.is_empty())
            .map_or_else(|| "Preparing review...".to_string(), ToString::to_string),
        Status::Queued => "Waiting in merge queue...".to_string(),
        Status::Rebasing => "Rebasing...".to_string(),
        Status::Merging => "Merging...".to_string(),
        Status::New | Status::Review | Status::Question | Status::Done | Status::Canceled => {
            String::new()
        }
    }
}

/// Returns the status indicator icon used for inline session-output messages.
fn session_output_status_icon(status: Status) -> Icon {
    match status {
        Status::InProgress | Status::AgentReview | Status::Rebasing | Status::Merging => {
            Icon::current_spinner()
        }
        Status::Queued
        | Status::New
        | Status::Review
        | Status::Question
        | Status::Done
        | Status::Canceled => Icon::Pending,
    }
}

/// Returns the icon used for published-branch sync status lines.
fn session_output_published_branch_sync_icon(sync_status: PublishedBranchSyncStatus) -> Icon {
    match sync_status {
        PublishedBranchSyncStatus::Idle => Icon::Pending,
        PublishedBranchSyncStatus::InProgress => Icon::current_spinner(),
        PublishedBranchSyncStatus::Succeeded => Icon::Check,
        PublishedBranchSyncStatus::Failed => Icon::Warn,
    }
}

/// Returns the color used for published-branch sync status lines.
fn session_output_published_branch_sync_color(
    sync_status: PublishedBranchSyncStatus,
) -> ratatui::style::Color {
    match sync_status {
        PublishedBranchSyncStatus::Idle => style::palette::TEXT_MUTED,
        PublishedBranchSyncStatus::InProgress | PublishedBranchSyncStatus::Failed => {
            style::palette::WARNING
        }
        PublishedBranchSyncStatus::Succeeded => style::palette::SUCCESS,
    }
}

fn move_input_cursor_vertical(
    input: &str,
    width: u16,
    cursor: usize,
    direction: VerticalDirection,
) -> usize {
    let input_layout = compute_input_layout_data(input, width);
    let clamped_cursor = cursor.min(input_layout.cursor_positions.len().saturating_sub(1));
    let (current_x, current_y) = input_layout.cursor_positions[clamped_cursor];

    let Some(target_y) = target_line_index(current_y, &input_layout.cursor_positions, direction)
    else {
        return clamped_cursor;
    };

    let target_line_width = input_layout
        .display_lines
        .get(target_y)
        .map_or(0, Line::width);
    let target_x = current_x.min(target_line_width);

    select_cursor_on_line(
        target_y,
        target_x,
        &input_layout.cursor_positions,
        clamped_cursor,
    )
}

/// Build wrapped chat input lines with cursor mapping and token-level styling.
fn compute_input_layout_data(input: &str, width: u16) -> InputLayout {
    let inner_width = width.saturating_sub(2) as usize;
    let prefix = " › ";
    let prefix_span = Span::styled(
        prefix,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let prefix_width = prefix_span.width();
    let continuation_padding = " ".repeat(prefix_width);

    let mut display_lines = Vec::new();
    let mut cursor_positions = Vec::with_capacity(input.chars().count() + 1);
    let mut current_line_spans = vec![prefix_span];
    let mut current_width = prefix_width;
    let mut line_index: usize = 0;

    let mut in_mention = false;
    let mut image_token_end: Option<usize> = None;
    let mut last_ch = None;

    let input_chars = input.chars().collect::<Vec<_>>();
    for (character_index, ch) in input_chars.iter().copied().enumerate() {
        if image_token_end.is_some_and(|end_index| character_index >= end_index) {
            image_token_end = None;
        }

        if ch == '\n' {
            in_mention = false;
            image_token_end = None;
            cursor_positions.push((current_width, line_index));
            display_lines.push(Line::from(std::mem::take(&mut current_line_spans)));
            current_line_spans = vec![Span::raw(continuation_padding.clone())];
            current_width = prefix_width;
            line_index += 1;
            last_ch = Some(ch);

            continue;
        }

        let is_word_start = !ch.is_whitespace()
            && (character_index == 0 || input_chars[character_index - 1].is_whitespace());
        if is_word_start {
            let word_width = input_chars
                .iter()
                .skip(character_index)
                .take_while(|next_ch| !next_ch.is_whitespace())
                .map(|next_ch| Span::raw(next_ch.to_string()).width())
                .sum::<usize>();
            let line_has_content = current_width > prefix_width;

            if line_has_content && current_width + word_width > inner_width {
                display_lines.push(Line::from(std::mem::take(&mut current_line_spans)));
                current_line_spans = vec![Span::raw(continuation_padding.clone())];
                current_width = prefix_width;
                line_index += 1;
            }
        }

        if ch == '@' && is_at_mention_boundary(last_ch) {
            in_mention = true;
        } else if in_mention && !is_at_mention_query_character(ch) {
            in_mention = false;
        }

        if image_token_end.is_none() && ch == '[' {
            image_token_end = image_token_end_index(&input_chars, character_index);
        }

        let is_image_token = image_token_end.is_some_and(|end_index| character_index < end_index);

        let style = if is_image_token {
            Style::default()
                .fg(CHAT_INPUT_IMAGE_TOKEN_COLOR)
                .add_modifier(Modifier::BOLD)
        } else if in_mention {
            Style::default().fg(CHAT_INPUT_AT_MENTION_COLOR)
        } else {
            Style::default()
        };

        let char_span = Span::styled(ch.to_string(), style);
        let char_width = char_span.width();

        if current_width + char_width > inner_width {
            display_lines.push(Line::from(std::mem::take(&mut current_line_spans)));
            current_line_spans = vec![Span::raw(continuation_padding.clone())];
            current_width = prefix_width;
            line_index += 1;
        }

        cursor_positions.push((current_width, line_index));
        current_line_spans.push(char_span);
        current_width += char_width;
        last_ch = Some(ch);
    }

    if current_width >= inner_width {
        cursor_positions.push((prefix_width, line_index + 1));
    } else {
        cursor_positions.push((current_width, line_index));
    }

    if !current_line_spans.is_empty() {
        display_lines.push(Line::from(current_line_spans));
    }

    if display_lines.is_empty() {
        display_lines.push(Line::from(""));
    }

    InputLayout {
        cursor_positions,
        display_lines,
    }
}

/// Returns the exclusive end index for an `[Image #n]` placeholder token that
/// starts at `start_index`.
fn image_token_end_index(input_chars: &[char], start_index: usize) -> Option<usize> {
    let token_body = input_chars.get(start_index..)?;

    if token_body.len() < "[Image #1]".chars().count() || token_body.first() != Some(&'[') {
        return None;
    }

    let image_prefix = ['[', 'I', 'm', 'a', 'g', 'e', ' ', '#'];
    if token_body.get(..image_prefix.len())? != image_prefix {
        return None;
    }

    let mut scan_index = start_index + image_prefix.len();
    let mut saw_digit = false;

    while let Some(ch) = input_chars.get(scan_index) {
        if ch.is_ascii_digit() {
            saw_digit = true;
            scan_index += 1;

            continue;
        }

        if *ch == ']' && saw_digit {
            return Some(scan_index + 1);
        }

        return None;
    }

    None
}

fn target_line_index(
    current_y: usize,
    cursor_positions: &[(usize, usize)],
    direction: VerticalDirection,
) -> Option<usize> {
    match direction {
        VerticalDirection::Up => current_y.checked_sub(1),
        VerticalDirection::Down => {
            let max_y = cursor_positions
                .iter()
                .map(|(_, cursor_y)| *cursor_y)
                .max()
                .unwrap_or(0);
            if current_y >= max_y {
                None
            } else {
                Some(current_y + 1)
            }
        }
    }
}

fn select_cursor_on_line(
    target_y: usize,
    target_x: usize,
    cursor_positions: &[(usize, usize)],
    fallback_cursor: usize,
) -> usize {
    let mut best_cursor_on_left: Option<(usize, usize)> = None;
    let mut nearest_cursor_on_right: Option<(usize, usize)> = None;

    for (cursor_index, (cursor_x, cursor_y)) in cursor_positions.iter().copied().enumerate() {
        if cursor_y != target_y {
            continue;
        }

        if cursor_x <= target_x {
            match best_cursor_on_left {
                Some((_, best_x)) if cursor_x < best_x => {}
                _ => {
                    best_cursor_on_left = Some((cursor_index, cursor_x));
                }
            }
        } else {
            match nearest_cursor_on_right {
                Some((_, nearest_x)) if cursor_x > nearest_x => {}
                _ => {
                    nearest_cursor_on_right = Some((cursor_index, cursor_x));
                }
            }
        }
    }

    best_cursor_on_left
        .or(nearest_cursor_on_right)
        .map_or(fallback_cursor, |(cursor_index, _)| cursor_index)
}

struct InputLayout {
    cursor_positions: Vec<(usize, usize)>,
    display_lines: Vec<Line<'static>>,
}

#[derive(Clone, Copy)]
enum VerticalDirection {
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::style::{Color, Modifier, Style};

    use super::*;
    use crate::domain::agent::{AgentModel, ReasoningLevel};
    use crate::domain::session::{PublishedBranchSyncStatus, Session, Status};
    use crate::ui::state::app_mode::{DoneSessionOutputMode, QuestionFocus};

    fn session_fixture() -> Session {
        crate::domain::session::tests::SessionFixtureBuilder::new()
            .status(Status::New)
            .build()
    }

    fn view_footer_text(
        session: &Session,
        can_open_worktree: bool,
        done_session_output_mode: DoneSessionOutputMode,
    ) -> String {
        session_view_footer_line(session, can_open_worktree, done_session_output_mode).to_string()
    }

    #[test]
    fn test_session_chat_areas_reserve_margin_header_and_bottom_panel() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let chat_areas = session_chat_areas(area, 5);

        // Assert
        assert_eq!(chat_areas.header_area, Rect::new(1, 1, 78, 2));
        assert_eq!(chat_areas.output_area, Rect::new(1, 3, 78, 15));
        assert_eq!(chat_areas.bottom_area, Rect::new(1, 18, 78, 5));
    }

    #[test]
    fn test_prompt_panel_areas_split_input_and_footer_rows() {
        // Arrange
        let area = Rect::new(3, 10, 50, 6);

        // Act
        let panel_areas = prompt_panel_areas(area);

        // Assert
        assert_eq!(panel_areas.input_area, Rect::new(3, 10, 50, 5));
        assert_eq!(panel_areas.footer_area, Rect::new(3, 15, 50, 1));
    }

    #[test]
    fn test_prompt_panel_areas_collapse_footer_when_only_one_row_fits() {
        // Arrange
        let area = Rect::new(3, 10, 50, 1);

        // Act
        let panel_areas = prompt_panel_areas(area);

        // Assert
        assert_eq!(panel_areas.input_area, area);
        assert_eq!(panel_areas.footer_area, Rect::new(3, 11, 50, 0));
    }

    #[test]
    fn test_session_header_lines_truncate_long_titles_and_keep_metadata() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;
        session.title = Some("This is a very long timer-aware session header title".to_string());
        session.model = AgentModel::Gpt54;
        session.in_progress_started_at = Some(0);

        // Act
        let header_lines = session_header_lines(&session, 50, ReasoningLevel::default(), 3_660);

        // Assert
        assert_eq!(header_lines.len(), 2);
        assert!(header_lines[0].to_string().contains("..."));
        assert!(header_lines[1].to_string().contains("Timer: 1h1m0s"));
    }

    #[test]
    fn test_session_metadata_text_ticks_live_in_progress_timer() {
        // Arrange
        let mut session = session_fixture();
        session.model = AgentModel::Gpt54;
        session.stats.added_lines = 9;
        session.stats.deleted_lines = 3;
        session.status = Status::InProgress;
        session.in_progress_started_at = Some(60);

        // Act
        let early_metadata = session_metadata_text(&session, 120, ReasoningLevel::default(), 90);
        let later_metadata = session_metadata_text(&session, 120, ReasoningLevel::default(), 3_720);

        // Assert
        assert!(early_metadata.contains("Lines: +9 / -3"));
        assert!(early_metadata.contains("Timer: 30s"));
        assert!(early_metadata.contains("Model: gpt-5.4"));
        assert!(
            early_metadata.find("Model: gpt-5.4") < early_metadata.find("Reasoning: high"),
            "model should appear before reasoning in metadata text"
        );
        assert!(later_metadata.contains("Timer: 1h1m0s"));
    }

    #[test]
    fn test_session_metadata_text_freezes_timer_after_in_progress_ends() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        session.in_progress_total_seconds = 3_660;

        // Act
        let earlier_metadata =
            session_metadata_text(&session, 80, ReasoningLevel::default(), 4_000);
        let later_metadata = session_metadata_text(&session, 80, ReasoningLevel::default(), 40_000);

        // Assert
        assert_eq!(earlier_metadata, later_metadata);
        assert!(earlier_metadata.contains("Timer: 1h1m0s"));
    }

    #[test]
    fn test_session_view_footer_line_in_progress_shows_stop_and_open_and_hides_diff() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;

        // Act
        let help_text = view_footer_text(&session, true, DoneSessionOutputMode::Summary);

        // Assert
        assert!(help_text.contains("q: back"));
        assert!(help_text.contains("Ctrl+c: stop"));
        assert!(help_text.contains("j/k: scroll"));
        assert!(help_text.contains("o: open"));
        assert!(!help_text.contains("d: diff"));
        assert!(!help_text.contains("Enter: reply"));
    }

    #[test]
    fn test_session_view_footer_line_merge_queue_statuses_hide_worktree_open_hint() {
        // Arrange
        let merge_queue_statuses = [Status::Queued, Status::Merging];

        // Act
        let help_texts: Vec<String> = merge_queue_statuses
            .iter()
            .map(|session_status| {
                let mut session = session_fixture();
                session.status = *session_status;

                view_footer_text(&session, true, DoneSessionOutputMode::Summary)
            })
            .collect();

        // Assert
        for help_text in help_texts {
            assert!(help_text.contains("q: back"));
            assert!(help_text.contains("j/k: scroll"));
            assert!(!help_text.contains("o: open"));
            assert!(!help_text.contains("Ctrl+c: stop"));
        }
    }

    #[test]
    fn test_session_view_footer_line_new_session_shows_draft_and_start_actions() {
        // Arrange
        let mut session = session_fixture();
        session.folder = PathBuf::new();
        session.is_draft = true;

        // Act
        let help_text = view_footer_text(&session, false, DoneSessionOutputMode::Summary);

        // Assert
        assert!(help_text.contains("Enter: add draft"));
        assert!(help_text.contains("s: start"));
        assert!(!help_text.contains("o: open"));
        assert!(help_text.contains("m: add to merge queue"));
        assert!(help_text.contains("r: rebase"));
        assert!(help_text.contains("/: commands menu"));
        assert!(!help_text.contains("d: diff"));
    }

    #[test]
    fn test_session_view_footer_line_done_modes_switch_toggle_label() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Done;

        // Act
        let summary_help_text = view_footer_text(&session, true, DoneSessionOutputMode::Summary);
        let output_help_text = view_footer_text(&session, true, DoneSessionOutputMode::Output);
        let review_help_text = view_footer_text(&session, true, DoneSessionOutputMode::Review);

        // Assert
        assert!(summary_help_text.contains("t: output"));
        assert!(output_help_text.contains("t: summary"));
        assert!(review_help_text.contains("t: summary"));
    }

    #[test]
    fn test_prompt_footer_line_shows_highlighted_actions_and_attachment_count() {
        // Arrange
        let session = session_fixture();

        // Act
        let footer_line = prompt_footer_line(&session, 2);

        // Assert
        assert_eq!(
            footer_line.to_string(),
            "Enter: submit | Alt+Enter: newline | Ctrl+V/Alt+V: paste image | Esc: cancel | 2 \
             images ready"
        );
        assert_eq!(
            footer_line.spans[0].style,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(footer_line.spans[1].style, Style::default().fg(Color::Gray));
        assert_eq!(
            footer_line.spans[footer_line.spans.len() - 2].style,
            Style::default().fg(Color::DarkGray)
        );
        assert_eq!(
            footer_line.spans[footer_line.spans.len() - 1].style,
            Style::default().fg(Color::Gray)
        );
    }

    #[test]
    fn test_prompt_footer_line_uses_stage_label_for_new_sessions() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::New;
        session.is_draft = true;

        // Act
        let footer_line = prompt_footer_line(&session, 0);

        // Assert
        assert!(footer_line.to_string().contains("Enter: stage draft"));
        assert!(!footer_line.to_string().contains("Enter: submit"));
    }

    #[test]
    fn test_question_help_footer_line_switches_actions_by_focus() {
        // Arrange

        // Act
        let chat_focus_line = question_help_footer_line(QuestionFocus::Chat);
        let answer_focus_line = question_help_footer_line(QuestionFocus::Answer);

        // Assert
        assert!(chat_focus_line.to_string().contains("j/k: scroll"));
        assert!(chat_focus_line.to_string().contains("Esc/Enter: answer"));
        assert!(answer_focus_line.to_string().contains("Enter: send"));
        assert!(answer_focus_line.to_string().contains("Esc: end turn"));
    }

    #[test]
    fn test_session_output_panel_borders_leave_full_inner_width_without_vertical_edges() {
        // Arrange & Act
        let inner_width = panel_inner_width(Rect::new(0, 0, 80, 5), session_output_panel_borders());
        let output_panel_borders = session_output_panel_borders();

        // Assert
        assert_eq!(inner_width, 80);
        assert!(!output_panel_borders.intersects(Borders::LEFT));
        assert!(!output_panel_borders.intersects(Borders::RIGHT));
    }

    #[test]
    fn test_session_output_done_toggle_line_switches_targets() {
        // Arrange

        // Act
        let summary_line = session_output_done_toggle_line(DoneSessionOutputMode::Summary);
        let output_line = session_output_done_toggle_line(DoneSessionOutputMode::Output);
        let review_line = session_output_done_toggle_line(DoneSessionOutputMode::Review);

        // Assert
        assert_eq!(summary_line.to_string(), "Press t to switch to output.");
        assert_eq!(output_line.to_string(), "Press t to switch to summary.");
        assert_eq!(review_line.to_string(), "Press t to switch to summary.");
    }

    #[test]
    fn test_session_output_status_line_for_in_progress_includes_progress_text() {
        // Arrange

        // Act
        let status_line =
            session_output_status_line(Status::InProgress, Some("Inspecting changed files"), None)
                .expect("in-progress sessions should render a status line");

        // Assert
        assert!(
            status_line
                .to_string()
                .contains("Working... Inspecting changed files")
        );
    }

    #[test]
    fn test_session_output_status_line_for_agent_review_uses_review_loader_text() {
        // Arrange

        // Act
        let status_line = session_output_status_line(
            Status::AgentReview,
            None,
            Some("Reviewing changes with gpt-5.4"),
        )
        .expect("agent-review sessions should render a status line");

        // Assert
        assert!(
            status_line
                .to_string()
                .contains("Reviewing changes with gpt-5.4")
        );
    }

    #[test]
    fn test_session_output_status_line_for_merging_uses_status_label() {
        // Arrange

        // Act
        let status_line = session_output_status_line(Status::Merging, None, None)
            .expect("merging sessions should render a status line");

        // Assert
        assert!(status_line.to_string().contains("Merging..."));
    }

    #[test]
    fn test_session_output_published_branch_sync_line_uses_sync_message() {
        // Arrange
        let mut session = session_fixture();
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());
        session.published_branch_sync_status = PublishedBranchSyncStatus::InProgress;

        // Act
        let sync_line = session_output_published_branch_sync_line(&session)
            .expect("published branch sync should render a status line");

        // Assert
        assert!(
            sync_line
                .to_string()
                .contains("Auto-pushing published branch after completed turn...")
        );
    }

    #[test]
    fn test_centered_content_rect_centers_requested_size() {
        // Arrange
        let area = Rect::new(10, 5, 40, 12);

        // Act
        let centered_area = centered_content_rect(area, 20, 5);

        // Assert
        assert_eq!(centered_area, Rect::new(20, 8, 20, 5));
    }

    #[test]
    fn test_centered_content_rect_clamps_requested_size_to_available_area() {
        // Arrange
        let area = Rect::new(3, 2, 12, 4);

        // Act
        let centered_area = centered_content_rect(area, 20, 6);

        // Assert
        assert_eq!(centered_area, area);
    }

    #[test]
    fn test_calculate_input_height() {
        // Arrange & Act & Assert
        assert_eq!(calculate_input_height(20, ""), 3);
        assert_eq!(calculate_input_height(12, "1234567"), 4);
        assert_eq!(calculate_input_height(12, "12345678"), 4);
        assert_eq!(calculate_input_height(12, "12345671234567890"), 5);
        assert_eq!(calculate_input_height(12, &"a".repeat(120)), 12);
    }

    #[test]
    fn test_question_panel_layout_preserves_input_height_before_trimming_question() {
        // Arrange
        let question = "Need a detailed migration plan with rollback guidance?";
        let input = "Use two phases.";

        // Act
        let panel_layout = question_panel_layout(28, 4, question, input, 10);

        // Assert
        assert_eq!(
            panel_layout,
            QuestionPanelLayout {
                help_height: 1,
                input_height: 3,
                question_height: 0,
                spacer_height: 0,
            }
        );
    }

    #[test]
    fn test_question_panel_layout_uses_wrapped_question_height_when_space_allows() {
        // Arrange
        let question = "Need a detailed migration plan with rollback guidance?";
        let input = "Use two phases.";

        // Act
        let panel_layout = question_panel_layout(28, 9, question, input, 10);

        // Assert — question_height includes +1 for the "Question N/M" title
        // line.
        assert_eq!(
            panel_layout,
            QuestionPanelLayout {
                help_height: 1,
                input_height: 3,
                question_height: 3,
                spacer_height: 1,
            }
        );
    }

    #[test]
    fn test_question_panel_layout_drops_spacer_before_last_question_line() {
        // Arrange
        let question = "Need a detailed migration plan with rollback guidance?";
        let input = "Use two phases.";

        // Act
        let panel_layout = question_panel_layout(28, 6, question, input, 10);

        // Assert — question_height includes +1 for the "Question N/M" title
        // line but the spacer is dropped when space is tight.
        assert_eq!(
            panel_layout,
            QuestionPanelLayout {
                help_height: 1,
                input_height: 3,
                question_height: 2,
                spacer_height: 0,
            }
        );
    }

    #[test]
    fn test_question_panel_reserved_height_includes_options_and_input_rows() {
        // Arrange
        let question = "Continue?";
        let input = "";

        // Act
        let reserved_height = question_panel_reserved_height(40, 12, question, input, 2, 10);

        // Assert
        assert_eq!(reserved_height, 10);
    }

    #[test]
    fn test_question_panel_areas_assign_expected_rows() {
        // Arrange
        let area = Rect::new(4, 10, 40, 10);
        let question = "Continue?";
        let input = "";

        // Act
        let panel_areas = question_panel_areas(area, question, input, 2, 10);

        // Assert
        assert_eq!(panel_areas.question_area, Rect::new(4, 10, 40, 2));
        assert_eq!(panel_areas.options_area, Rect::new(4, 12, 40, 3));
        assert_eq!(panel_areas.spacer_area, Rect::new(4, 15, 40, 1));
        assert_eq!(panel_areas.input_area, Rect::new(4, 16, 40, 3));
        assert_eq!(panel_areas.help_area, Rect::new(4, 19, 40, 1));
    }

    #[test]
    fn test_question_options_height_adds_header_and_clamps() {
        // Arrange & Act & Assert
        assert_eq!(question_options_height(0, 10), 0);
        assert_eq!(question_options_height(2, 10), 3);
        assert_eq!(question_options_height(5, 3), 3);
    }

    #[test]
    fn test_calculate_input_viewport_without_scroll() {
        // Arrange
        let total_line_count = 4;
        let cursor_y = 2;
        let viewport_height = 10;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 0);
        assert_eq!(cursor_row, 2);
    }

    #[test]
    fn test_calculate_input_viewport_with_scroll() {
        // Arrange
        let total_line_count = 20;
        let cursor_y = 15;
        let viewport_height = 10;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 6);
        assert_eq!(cursor_row, 9);
    }

    #[test]
    fn test_calculate_input_viewport_clamps_cursor_to_last_line() {
        // Arrange
        let total_line_count = 3;
        let cursor_y = 10;
        let viewport_height = 2;

        // Act
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);

        // Assert
        assert_eq!(scroll_offset, 1);
        assert_eq!(cursor_row, 1);
    }

    #[test]
    fn test_overlay_area_above_clamps_to_available_height() {
        // Arrange
        let container_area = Rect::new(0, 5, 30, 12);
        let anchor_area = Rect::new(2, 9, 20, 3);

        // Act
        let overlay_area = overlay_area_above(container_area, anchor_area, 6);

        // Assert
        assert_eq!(overlay_area, Some(Rect::new(2, 5, 20, 4)));
    }

    #[test]
    fn test_overlay_area_above_returns_none_without_space() {
        // Arrange
        let container_area = Rect::new(0, 5, 30, 12);
        let anchor_area = Rect::new(2, 5, 20, 3);

        // Act
        let overlay_area = overlay_area_above(container_area, anchor_area, 2);

        // Assert
        assert_eq!(overlay_area, None);
    }

    #[test]
    fn test_panel_inner_width_excludes_horizontal_borders() {
        // Arrange
        let area = Rect::new(0, 0, 10, 5);

        // Act
        let inner_width = panel_inner_width(area, Borders::LEFT | Borders::RIGHT);

        // Assert
        assert_eq!(inner_width, 8);
    }

    #[test]
    fn test_bottom_pinned_scroll_offset_uses_inner_height() {
        // Arrange
        let area = Rect::new(0, 0, 20, 5);

        // Act
        let scroll_offset =
            bottom_pinned_scroll_offset(area, Borders::TOP | Borders::BOTTOM, 6, None);

        // Assert
        assert_eq!(scroll_offset, 3);
    }

    #[test]
    fn test_bottom_pinned_scroll_offset_preserves_explicit_value() {
        // Arrange
        let area = Rect::new(0, 0, 20, 5);

        // Act
        let scroll_offset =
            bottom_pinned_scroll_offset(area, Borders::TOP | Borders::BOTTOM, 6, Some(2));

        // Assert
        assert_eq!(scroll_offset, 2);
    }

    #[test]
    fn test_placeholder_cursor_position_accounts_for_border_and_prompt_prefix() {
        // Arrange
        let area = Rect::new(10, 5, 40, 4);

        // Act
        let cursor_position = placeholder_cursor_position(area);

        // Assert
        assert_eq!(cursor_position, (14, 6));
    }

    #[test]
    fn test_input_cursor_position_accounts_for_border_and_offsets() {
        // Arrange
        let area = Rect::new(10, 5, 40, 6);
        let cursor_x = 7;
        let cursor_row = 2;

        // Act
        let cursor_position = input_cursor_position(area, cursor_x, cursor_row);

        // Assert
        assert_eq!(cursor_position, (18, 8));
    }

    #[test]
    fn test_suggestion_dropdown_height_includes_border_lines() {
        // Arrange
        let option_count = 4;

        // Act
        let dropdown_height = suggestion_dropdown_height(option_count);

        // Assert
        assert_eq!(dropdown_height, 6);
    }

    #[test]
    fn test_suggestion_dropdown_height_saturates_at_u16_max() {
        // Arrange
        let option_count = usize::MAX;

        // Act
        let dropdown_height = suggestion_dropdown_height(option_count);

        // Assert
        assert_eq!(dropdown_height, u16::MAX);
    }

    #[test]
    fn test_compute_input_layout_empty() {
        // Arrange
        let input = "";
        let width = 20;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 3); // prefix " › "
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_single_line() {
        // Arrange
        let input = "test";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 7); // 3 (prefix) + 4 (text)
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_exact_fit() {
        // Arrange
        let input = "1234567";
        let width = 12; // Inner width 10, Prefix 3, Available 7
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_wrap() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 4);
        assert_eq!(lines[1].to_string(), "   8");
        assert_eq!(cursor_x, 4);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_wraps_whole_words_when_they_fit_on_next_line() {
        // Arrange
        let input = "abc def";
        let width = 11;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].to_string(), " › abc ");
        assert_eq!(lines[1].to_string(), "   def");
        assert_eq!(cursor_x, 6);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiline_exact_fit() {
        // Arrange
        let input = "1234567".to_owned() + "1234567890";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(&input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 10);
        assert_eq!(lines[2].width(), 6);
        assert_eq!(cursor_x, 6);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_compute_input_layout_cursor_at_start() {
        // Arrange
        let input = "hello";
        let width = 20;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 0);

        // Assert — cursor sits right after prefix
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_cursor_in_middle() {
        // Arrange
        let input = "hello";
        let width = 20;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 2);

        // Assert — prefix(3) + 2 chars
        assert_eq!(cursor_x, 5);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_cursor_before_wrapped_char() {
        // Arrange
        let input = "12345678";
        let width = 12;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 7);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_cursor_moves_to_next_line_before_wrapped_word() {
        // Arrange
        let input = "abc def";
        let width = 11;

        // Act
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 4);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_move_input_cursor_up_on_wrapped_layout() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = input.chars().count();

        // Act
        let cursor = move_input_cursor_up(input, width, cursor);

        // Assert
        assert_eq!(cursor, 1);
    }

    #[test]
    fn test_move_input_cursor_down_on_wrapped_layout() {
        // Arrange
        let input = "12345678";
        let width = 12;
        let cursor = 1;

        // Act
        let cursor = move_input_cursor_down(input, width, cursor);

        // Assert
        assert_eq!(cursor, input.chars().count());
    }

    #[test]
    fn test_compute_input_layout_explicit_newline() {
        // Arrange
        let input = "ab\ncd";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].to_string(), "   cd");
        assert_eq!(cursor_x, 5); // continuation padding + "cd"
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiple_newlines() {
        // Arrange
        let input = "a\n\nb";
        let width = 20;
        let cursor = input.chars().count();

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width, cursor);

        // Assert — 3 lines: "a", padded continuation line, "b"
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].to_string(), "   ");
        assert_eq!(cursor_x, 4);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_compute_input_layout_cursor_on_second_line() {
        // Arrange — cursor right after the newline
        let input = "ab\ncd";
        let width = 20;

        // Act — cursor at char index 3 = 'a','b','\n' -> position 0 of second line
        let (_, cursor_x, cursor_y) = compute_input_layout(input, width, 3);

        // Assert
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_first_table_column_width_uses_remaining_layout_space() {
        // Arrange
        let constraints = [
            Constraint::Fill(1),
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Length(6),
        ];

        // Act
        let width = first_table_column_width(50, &constraints, 1, 3);

        // Assert
        assert_eq!(width, 21);
    }

    #[test]
    fn test_compute_input_layout_at_mention_highlighting() {
        // Arrange
        let input = "hello @file world";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        // Prefix is at index 0: " › "
        assert_eq!(spans[0].content, " › ");

        // "hello " (indices 1..7) should be normal style
        for span in spans.iter().take(7).skip(1) {
            assert_eq!(span.style.fg, None);
        }

        // "@file" (indices 7..12) should be highlighted
        for span in spans.iter().take(12).skip(7) {
            assert_eq!(span.style.fg, Some(CHAT_INPUT_AT_MENTION_COLOR));
        }

        // " world" (indices 12..18) should be normal style
        for span in spans.iter().take(18).skip(12) {
            assert_eq!(span.style.fg, None);
        }
    }

    #[test]
    fn test_compute_input_layout_at_mention_highlighting_stops_before_trailing_comma() {
        // Arrange
        let input = "hello @file, world";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().take(12).skip(7) {
            assert_eq!(span.style.fg, Some(CHAT_INPUT_AT_MENTION_COLOR));
        }
        assert_eq!(spans[12].content, ",");
        assert_eq!(spans[12].style.fg, None);
    }

    #[test]
    fn test_compute_input_layout_at_mention_highlighting_stops_before_trailing_parenthesis() {
        // Arrange
        let input = "hello @file) world";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().take(12).skip(7) {
            assert_eq!(span.style.fg, Some(CHAT_INPUT_AT_MENTION_COLOR));
        }
        assert_eq!(spans[12].content, ")");
        assert_eq!(spans[12].style.fg, None);
    }

    #[test]
    fn test_compute_input_layout_highlights_parenthesized_at_mention() {
        // Arrange
        let input = "review (@src/main.rs)";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().take(21).skip(9) {
            assert_eq!(span.style.fg, Some(CHAT_INPUT_AT_MENTION_COLOR));
        }
        assert_eq!(spans[21].content, ")");
        assert_eq!(spans[21].style.fg, None);
    }

    #[test]
    fn test_compute_input_layout_no_highlight_for_email() {
        // Arrange
        let input = "email@example.com";
        let width = 40;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;

        for span in spans.iter().skip(1) {
            assert_eq!(span.style.fg, None);
        }
    }

    #[test]
    fn test_compute_input_layout_highlights_image_placeholder_tokens() {
        // Arrange
        let input = "Review [Image #12] before merge";
        let width = 60;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;
        let image_start = 1 + "Review ".chars().count();
        let image_end = image_start + "[Image #12]".chars().count();

        for span in spans.iter().take(image_end).skip(image_start) {
            assert_eq!(span.style.fg, Some(CHAT_INPUT_IMAGE_TOKEN_COLOR));
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn test_compute_input_layout_does_not_highlight_invalid_image_like_tokens() {
        // Arrange
        let input = "Review [Image #] before merge";
        let width = 60;

        // Act
        let (lines, _, _) = compute_input_layout(input, width, 0);

        // Assert
        let line = &lines[0];
        let spans = &line.spans;
        let token_start = 1 + "Review ".chars().count();
        let token_end = token_start + "[Image #]".chars().count();

        for span in spans.iter().take(token_end).skip(token_start) {
            assert_eq!(span.style.fg, None);
        }
    }
}
