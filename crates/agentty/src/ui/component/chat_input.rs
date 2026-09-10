use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::ui::input_layout::{
    CHAT_INPUT_MAX_VISIBLE_LINES, calculate_input_viewport, compute_input_layout,
    input_cursor_position, placeholder_cursor_position, suggestion_dropdown_height,
};
use crate::ui::{Component, style};

/// One row rendered inside a prompt suggestion dropdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuggestionItem {
    /// Optional compact badge rendered before the main label.
    pub badge: Option<String>,
    /// Optional explanatory text rendered after the label.
    pub detail: Option<String>,
    /// Primary row label used for selection and insertion.
    pub label: String,
    /// Optional trailing metadata rendered with subdued styling.
    pub metadata: Option<String>,
}

/// Suggestion dropdown rendered above or alongside the prompt input block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuggestionList {
    /// Dropdown rows in display order.
    pub items: Vec<SuggestionItem>,
    /// Highlighted row index in `items`.
    pub selected_index: usize,
    /// Dropdown title shown in the rounded border chrome.
    pub title: String,
}

/// Prompt input component with optional rich suggestion dropdown.
pub struct ChatInput<'a> {
    /// Placeholder rendered while the input is empty.
    pub placeholder: &'a str,
    active: bool,
    clear_style: Option<Style>,
    cursor: usize,
    input: &'a str,
    status: Option<&'a str>,
    suggestion_list: Option<&'a SuggestionList>,
    title: &'a str,
}

impl<'a> ChatInput<'a> {
    /// Creates a new prompt input component.
    pub fn new(title: &'a str, input: &'a str, cursor: usize) -> Self {
        Self {
            placeholder: "",
            active: true,
            clear_style: None,
            cursor,
            input,
            status: None,
            suggestion_list: None,
            title,
        }
    }

    /// Sets the input placeholder text.
    #[must_use]
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// Marks the input as inactive (dimmed border, no cursor).
    ///
    /// When `false`, the border uses a muted color and the terminal cursor
    /// is not rendered. Defaults to `true`.
    #[must_use]
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Sets the suggestion dropdown shown next to the prompt input.
    #[must_use]
    pub fn suggestion_list(mut self, suggestion_list: &'a SuggestionList) -> Self {
        self.suggestion_list = Some(suggestion_list);
        self
    }

    /// Sets compact session status text rendered beside the input title.
    #[must_use]
    pub fn status(mut self, status: &'a str) -> Self {
        self.status = Some(status);
        self
    }

    /// Sets the style reapplied after clearing the input area.
    ///
    /// Overlay-hosted inputs use this to keep popup-local cells on the
    /// semantic overlay surface instead of terminal-default colors.
    #[must_use]
    pub fn clear_style(mut self, clear_style: Style) -> Self {
        self.clear_style = Some(clear_style);
        self
    }

    /// Returns the shared block styling for the prompt input frame.
    ///
    /// Uses accent styling when active and muted styling when inactive.
    fn input_block(&self) -> Block<'a> {
        let title = self.status.map_or_else(
            || format!(" {} ", self.title),
            |status| format!(" {} · {status} ", self.title),
        );
        let (border_style, title_style) = if self.active {
            (Self::focused_border_style(), Self::focused_title_style())
        } else {
            (Self::inactive_border_style(), Self::inactive_title_style())
        };

        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(Span::styled(title, title_style))
    }

    /// Returns the border style used to keep the active prompt field visually
    /// prominent.
    fn focused_border_style() -> Style {
        Style::default()
            .fg(style::palette::accent())
            .add_modifier(Modifier::BOLD)
    }

    /// Returns the title style used by the focused prompt input frame.
    fn focused_title_style() -> Style {
        Style::default()
            .fg(style::palette::accent())
            .add_modifier(Modifier::BOLD)
    }

    /// Returns the border style for an inactive (dimmed) prompt input frame.
    fn inactive_border_style() -> Style {
        Style::default().fg(style::palette::border())
    }

    /// Returns the title style for an inactive (dimmed) prompt input frame.
    fn inactive_title_style() -> Style {
        Style::default().fg(style::palette::border())
    }

    /// Returns the shared block styling for prompt suggestion dropdowns.
    fn dropdown_block(title: &str) -> Block<'_> {
        let title = format!(" {title} ");

        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(style::palette::accent_soft()))
            .title(Span::styled(
                title,
                Style::default().fg(style::palette::accent_soft()),
            ))
    }

    /// Returns the default foreground style for typed prompt content.
    fn input_text_style() -> Style {
        Style::default().fg(style::palette::text())
    }

    /// Clears an input rectangle and optionally renders an empty styled block
    /// so overlay-hosted input cells keep semantic colors.
    fn clear_area(f: &mut Frame, area: Rect, clear_style: Option<Style>) {
        f.render_widget(Clear, area);

        let Some(clear_style) = clear_style else {
            return;
        };

        f.render_widget(Block::default().style(clear_style), area);
    }

    /// Renders the suggestion dropdown using the shared chat input chrome.
    ///
    /// This method is also used by the question-mode panel to render the
    /// at-mention file dropdown as an overlay above the input area.
    pub(crate) fn render_suggestion_dropdown(
        f: &mut Frame,
        area: Rect,
        suggestion_list: &SuggestionList,
    ) {
        let rows = suggestion_list
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let is_selected = index == suggestion_list.selected_index;
                let prefix = if is_selected { ">" } else { " " };
                let label_style = if is_selected {
                    Style::default()
                        .fg(style::palette::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(style::palette::text_muted())
                };
                let description_style = if is_selected {
                    Style::default().fg(style::palette::text_muted())
                } else {
                    Style::default().fg(style::palette::text_subtle())
                };

                let mut spans = Vec::new();
                spans.push(Span::styled(format!("{prefix} "), label_style));

                if let Some(badge) = &item.badge {
                    spans.push(Span::styled(format!("[{badge}] "), description_style));
                }

                spans.push(Span::styled(item.label.as_str(), label_style));

                if let Some(metadata) = &item.metadata {
                    spans.push(Span::styled(format!("  {metadata}"), description_style));
                }

                if let Some(detail) = &item.detail {
                    spans.push(Span::styled(format!("  {detail}"), description_style));
                }

                Line::from(spans)
            })
            .collect::<Vec<_>>();

        let dropdown = Paragraph::new(rows)
            .style(Self::input_text_style())
            .block(Self::dropdown_block(&suggestion_list.title));

        Self::clear_area(f, area, None);
        f.render_widget(dropdown, area);
    }

    /// Render the prompt input with an internally scrollable viewport.
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let block = self.input_block();

        if self.input.is_empty() {
            let prefix_style = if self.active {
                Style::default()
                    .fg(style::palette::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(style::palette::border())
            };
            let prefix = " › ";
            let display_lines = vec![Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::raw("  "),
                Span::styled(
                    self.placeholder,
                    Style::default().fg(style::palette::text_subtle()),
                ),
            ])];

            let widget = Paragraph::new(display_lines)
                .style(Self::input_text_style())
                .block(block);
            Self::clear_area(f, area, self.clear_style);
            f.render_widget(widget, area);
            if self.active {
                f.set_cursor_position(placeholder_cursor_position(area));
            }

            return;
        }

        let (display_lines, cursor_x, cursor_y) =
            compute_input_layout(self.input, area.width, self.cursor);
        let viewport_height = area
            .height
            .saturating_sub(2)
            .min(CHAT_INPUT_MAX_VISIBLE_LINES);
        let total_line_count = Self::total_viewport_line_count(display_lines.len(), cursor_y);
        let (scroll_offset, cursor_row) =
            calculate_input_viewport(total_line_count, cursor_y, viewport_height);
        let widget = Paragraph::new(display_lines)
            .style(Self::input_text_style())
            .scroll((scroll_offset, 0))
            .block(block);

        Self::clear_area(f, area, self.clear_style);
        f.render_widget(widget, area);
        if self.active {
            f.set_cursor_position(input_cursor_position(area, cursor_x, cursor_row));
        }
    }

    /// Computes the total line count used by input viewport scrolling.
    ///
    /// The cursor can legally point to a trailing wrapped line that has no
    /// visible characters yet (exact line-fit case), so viewport calculations
    /// must account for whichever line index is greater.
    fn total_viewport_line_count(display_line_count: usize, cursor_y: u16) -> usize {
        display_line_count.max(usize::from(cursor_y).saturating_add(1))
    }
}

impl Component for ChatInput<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        if let Some(suggestion_list) = &self.suggestion_list {
            let dropdown_height = suggestion_dropdown_height(suggestion_list.items.len());
            let sections = Layout::default()
                .constraints([Constraint::Length(dropdown_height), Constraint::Min(0)])
                .split(area);

            Self::render_suggestion_dropdown(f, sections[0], suggestion_list);
            self.render_input(f, sections[1]);

            return;
        }

        self.render_input(f, area);
    }
}

#[cfg(test)]
#[path = "chat_input_test.rs"]
mod tests;
