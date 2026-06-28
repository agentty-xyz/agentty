//! Chat-input text layout and shared geometry helpers.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Borders;

use crate::domain::input::{is_at_mention_boundary, is_at_mention_query_character};
use crate::ui::style;

/// Maximum number of visible content lines inside the chat input viewport.
pub const CHAT_INPUT_MAX_VISIBLE_LINES: u16 = 10;

const CHAT_INPUT_BORDER_HEIGHT: u16 = 2;
const CHAT_INPUT_INNER_OFFSET: u16 = 1;
const CHAT_INPUT_PROMPT_PREFIX_WIDTH: u16 = 3;
const SLASH_MENU_BORDER_HEIGHT: u16 = 2;

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
    let input_chars = input.chars().collect::<Vec<_>>();
    let mut builder = InputLayoutBuilder::new(width, input_chars.len());

    for (character_index, ch) in input_chars.iter().copied().enumerate() {
        builder.push_character(&input_chars, character_index, ch);
    }

    builder.finish()
}

/// Incrementally builds wrapped input lines and cursor positions.
struct InputLayoutBuilder {
    continuation_padding: String,
    current_line_spans: Vec<Span<'static>>,
    current_width: usize,
    cursor_positions: Vec<(usize, usize)>,
    display_lines: Vec<Line<'static>>,
    image_token_end: Option<usize>,
    in_mention: bool,
    inner_width: usize,
    last_ch: Option<char>,
    line_index: usize,
    prefix_width: usize,
}

impl InputLayoutBuilder {
    /// Creates a builder initialized with the prompt prefix span.
    fn new(width: u16, input_char_count: usize) -> Self {
        let prefix_span = Span::styled(
            " › ",
            Style::default()
                .fg(style::palette::accent())
                .add_modifier(Modifier::BOLD),
        );
        let prefix_width = prefix_span.width();

        Self {
            continuation_padding: " ".repeat(prefix_width),
            current_line_spans: vec![prefix_span],
            current_width: prefix_width,
            cursor_positions: Vec::with_capacity(input_char_count + 1),
            display_lines: Vec::new(),
            image_token_end: None,
            in_mention: false,
            inner_width: width.saturating_sub(2) as usize,
            last_ch: None,
            line_index: 0,
            prefix_width,
        }
    }

    /// Adds one input character, applying explicit and automatic wrapping.
    fn push_character(&mut self, input_chars: &[char], character_index: usize, ch: char) {
        self.expire_image_token(character_index);
        if ch == '\n' {
            self.push_explicit_newline(ch);

            return;
        }

        self.wrap_before_word_if_needed(input_chars, character_index, ch);
        self.update_token_state(input_chars, character_index, ch);
        self.push_styled_character(ch, self.character_style(character_index));
        self.last_ch = Some(ch);
    }

    /// Finishes the last line and returns the complete render layout.
    fn finish(mut self) -> InputLayout {
        if self.current_width >= self.inner_width {
            self.cursor_positions
                .push((self.prefix_width, self.line_index + 1));
        } else {
            self.cursor_positions
                .push((self.current_width, self.line_index));
        }

        if !self.current_line_spans.is_empty() {
            self.display_lines.push(Line::from(self.current_line_spans));
        }

        if self.display_lines.is_empty() {
            self.display_lines.push(Line::from(""));
        }

        InputLayout {
            cursor_positions: self.cursor_positions,
            display_lines: self.display_lines,
        }
    }

    /// Clears stale image-token state once the cursor has advanced beyond it.
    fn expire_image_token(&mut self, character_index: usize) {
        if self
            .image_token_end
            .is_some_and(|end_index| character_index >= end_index)
        {
            self.image_token_end = None;
        }
    }

    /// Records an explicit newline and starts a continuation line.
    fn push_explicit_newline(&mut self, ch: char) {
        self.in_mention = false;
        self.image_token_end = None;
        self.cursor_positions
            .push((self.current_width, self.line_index));
        self.start_new_line();
        self.last_ch = Some(ch);
    }

    /// Wraps before a whole word when it would overflow the current line.
    fn wrap_before_word_if_needed(
        &mut self,
        input_chars: &[char],
        character_index: usize,
        ch: char,
    ) {
        if !is_word_start(input_chars, character_index, ch) {
            return;
        }

        let word_width = input_chars
            .iter()
            .skip(character_index)
            .take_while(|next_ch| !next_ch.is_whitespace())
            .map(|next_ch| Span::raw(next_ch.to_string()).width())
            .sum::<usize>();

        if self.current_width > self.prefix_width
            && self.current_width + word_width > self.inner_width
        {
            self.start_new_line();
        }
    }

    /// Updates mention and image-token tracking before styling a character.
    fn update_token_state(&mut self, input_chars: &[char], character_index: usize, ch: char) {
        if ch == '@' && is_at_mention_boundary(self.last_ch) {
            self.in_mention = true;
        } else if self.in_mention && !is_at_mention_query_character(ch) {
            self.in_mention = false;
        }

        if self.image_token_end.is_none() && ch == '[' {
            self.image_token_end = image_token_end_index(input_chars, character_index);
        }
    }

    /// Returns the display style for the current token state.
    fn character_style(&self, character_index: usize) -> Style {
        if self
            .image_token_end
            .is_some_and(|end_index| character_index < end_index)
        {
            return Style::default()
                .fg(style::palette::warning())
                .add_modifier(Modifier::BOLD);
        }

        if self.in_mention {
            return Style::default().fg(style::palette::info());
        }

        Style::default()
    }

    /// Appends one styled character, wrapping a too-wide character to the next
    /// line.
    fn push_styled_character(&mut self, ch: char, style: Style) {
        let char_span = Span::styled(ch.to_string(), style);
        let char_width = char_span.width();

        if self.current_width + char_width > self.inner_width {
            self.start_new_line();
        }

        self.cursor_positions
            .push((self.current_width, self.line_index));
        self.current_line_spans.push(char_span);
        self.current_width += char_width;
    }

    /// Closes the current line and starts a padded continuation line.
    fn start_new_line(&mut self) {
        self.display_lines
            .push(Line::from(std::mem::take(&mut self.current_line_spans)));
        self.current_line_spans = vec![Span::raw(self.continuation_padding.clone())];
        self.current_width = self.prefix_width;
        self.line_index += 1;
    }
}

/// Returns whether `ch` starts a word in `input_chars`.
fn is_word_start(input_chars: &[char], character_index: usize, ch: char) -> bool {
    if ch.is_whitespace() {
        return false;
    }

    if character_index == 0 {
        return true;
    }

    input_chars[character_index - 1].is_whitespace()
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
