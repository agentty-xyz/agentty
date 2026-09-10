use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::icon::Icon;
use crate::ui::markdown::render_markdown;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const MIN_OVERLAY_HEIGHT: u16 = 9;
const MIN_OVERLAY_WIDTH: u16 = 44;
const OVERLAY_HORIZONTAL_CHROME: u16 = 6;
const OVERLAY_HEIGHT_PERCENT: u16 = 26;
const OVERLAY_MAX_WIDTH_PERCENT: u16 = 96;
const OVERLAY_WIDTH_PERCENT: u16 = 52;

/// Centered informational popup used for non-destructive workflow guidance.
pub struct InfoOverlay<'a> {
    is_loading: bool,
    loading_label: &'a str,
    message: &'a str,
    spinner_frame: usize,
    title: &'a str,
}

impl<'a> InfoOverlay<'a> {
    /// Creates an informational popup with title and body message.
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self {
            is_loading: false,
            loading_label: "Sync in progress...",
            message,
            spinner_frame: 0,
            title,
        }
    }

    /// Sets whether the overlay should display a loading indicator.
    #[must_use]
    pub fn is_loading(mut self, loading: bool) -> Self {
        self.is_loading = loading;
        self
    }

    /// Sets the spinner label shown while the overlay is loading.
    #[must_use]
    pub fn loading_label(mut self, loading_label: &'a str) -> Self {
        self.loading_label = loading_label;
        self
    }

    /// Sets the deterministic animation frame for the loading indicator.
    #[must_use]
    pub fn spinner_frame(mut self, spinner_frame: usize) -> Self {
        self.spinner_frame = spinner_frame;

        self
    }

    /// Renders the body message as markdown with inline highlight styles and
    /// explicit line breaks.
    fn message_lines(&self, message_width: usize) -> Vec<Line<'static>> {
        let normalized_message = Self::markdown_message_with_block_headers(self.message);
        let mut message_lines = render_markdown(&normalized_message, message_width);

        if message_lines.is_empty() {
            message_lines.push(Line::from(""));
        }

        message_lines
    }

    /// Adds markdown block headers for numbered sync sections and inserts blank
    /// lines between each section.
    fn markdown_message_with_block_headers(message: &str) -> String {
        let mut normalized_lines: Vec<String> = Vec::new();

        for raw_line in message.split('\n') {
            if let Some(formatted_line) = Self::format_sync_block_title(raw_line) {
                if let Some(previous_line) = normalized_lines.last()
                    && !previous_line.is_empty()
                {
                    normalized_lines.push(String::new());
                }

                normalized_lines.push(formatted_line);
                continue;
            }

            normalized_lines.push(raw_line.to_string());
        }

        normalized_lines.join("\n")
    }

    /// Converts known sync section title lines (e.g., `1. Pull`, `2. Push`) to
    /// markdown heading text.
    fn format_sync_block_title(raw_line: &str) -> Option<String> {
        let trimmed_line = raw_line.trim();
        if trimmed_line.is_empty() {
            return None;
        }

        let heading_content = trimmed_line.strip_prefix("## ").unwrap_or(trimmed_line);

        split_prefixed_title(heading_content)?;
        let normalized_title = heading_content.to_ascii_lowercase();
        if !normalized_title.contains("pull")
            && !normalized_title.contains("push")
            && !normalized_title.contains("conflict")
        {
            return None;
        }

        if trimmed_line.starts_with("## ") {
            return Some(trimmed_line.to_string());
        }

        Some(format!("## {heading_content}"))
    }

    /// Centers the first body line when it matches the sync context header
    /// format (`Project ...` or `Main branch ...`).
    fn center_sync_context_header(message_lines: &mut [Line<'static>]) {
        let Some(first_line) = message_lines.first_mut() else {
            return;
        };

        if !Self::is_sync_context_header(first_line) {
            return;
        }

        *first_line = first_line.clone().alignment(Alignment::Center);
    }

    /// Returns whether a message line is the generated sync context header.
    fn is_sync_context_header(line: &Line<'_>) -> bool {
        let line_text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        line_text.starts_with("Project ") || line_text.starts_with("Main branch ")
    }

    /// Builds the styled body lines including the action row at the bottom.
    fn body_lines(&self, message_width: usize) -> Vec<Line<'static>> {
        let mut lines = self.message_lines(message_width);

        if !self.is_loading {
            Self::center_sync_context_header(&mut lines);
        }

        lines.push(Line::from(""));
        lines.push(self.action_row());

        lines
    }

    /// Returns the bottom action row: a spinner during loading or an `OK`
    /// button when complete.
    fn action_row(&self) -> Line<'static> {
        if self.is_loading {
            let loading_text = format!("{} {}", Icon::current_spinner(), self.loading_label);

            Line::from(vec![Span::styled(loading_text, loading_indicator_style())])
                .alignment(Alignment::Center)
        } else {
            Line::from(vec![Span::styled(" OK ", ok_button_style())]).alignment(Alignment::Center)
        }
    }

    /// Returns the popup border color: cyan during loading, yellow when
    /// complete.
    fn border_color(&self) -> Color {
        if self.is_loading {
            palette::accent()
        } else {
            palette::warning()
        }
    }

    /// Returns the body text alignment: centered during loading, left-aligned
    /// when complete.
    fn body_alignment(&self) -> Alignment {
        if self.is_loading {
            Alignment::Center
        } else {
            Alignment::Left
        }
    }

    /// Returns popup width constrained by overlay defaults and frame bounds.
    fn popup_width(&self, area: Rect) -> u16 {
        let default_width = (area.width * OVERLAY_WIDTH_PERCENT / 100)
            .max(MIN_OVERLAY_WIDTH)
            .min(area.width);
        let max_width = (area.width * OVERLAY_MAX_WIDTH_PERCENT / 100)
            .max(MIN_OVERLAY_WIDTH)
            .min(area.width);
        let preferred_width = self.preferred_popup_width(max_width);

        default_width.max(preferred_width).min(max_width)
    }

    /// Returns the popup width preferred by the current message content.
    fn preferred_popup_width(&self, max_width: u16) -> u16 {
        let longest_line_width = self.longest_unwrapped_line_width();
        let preferred_content_width = u16::try_from(longest_line_width)
            .unwrap_or(u16::MAX)
            .saturating_add(1);

        preferred_content_width
            .saturating_add(OVERLAY_HORIZONTAL_CHROME)
            .min(max_width)
    }

    /// Returns the terminal display width of the longest unwrapped message or
    /// action row line before the paragraph widget applies wrapping.
    fn longest_unwrapped_line_width(&self) -> usize {
        let message = Self::markdown_message_with_block_headers(self.message);
        let message_width = message
            .lines()
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(0);
        let action_width = if self.is_loading {
            UnicodeWidthStr::width(
                format!("{} {}", Icon::current_spinner(), self.loading_label).as_str(),
            )
        } else {
            UnicodeWidthStr::width(" OK ")
        };

        message_width.max(action_width)
    }

    /// Returns popup height sized to keep wrapped body content and the action
    /// row visible.
    fn popup_height(&self, area: Rect, width: u16) -> u16 {
        let min_height = (area.height * OVERLAY_HEIGHT_PERCENT / 100)
            .max(MIN_OVERLAY_HEIGHT)
            .min(area.height);
        let message_width = overlay::overlay_content_width(width);
        let required_inner_lines = self.body_lines(message_width).len();
        let required_height =
            overlay::overlay_required_height(required_inner_lines).min(area.height);

        required_height.max(min_height)
    }

    /// Returns the default body foreground used by unhighlighted overlay text.
    fn body_text_style() -> Style {
        Style::default().fg(palette::text())
    }
}

impl Component for InfoOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let width = self.popup_width(area);
        let message_width = overlay::overlay_content_width(width);
        let border_color = self.border_color();
        let paragraph = Paragraph::new(self.body_lines(message_width))
            .alignment(self.body_alignment())
            .style(Self::body_text_style())
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block(self.title, border_color));

        let height = self.popup_height(area, width);
        let popup_area = overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            OVERLAY_HEIGHT_PERCENT,
            width,
            height,
        );

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(paragraph, popup_area);

        if self.is_loading {
            TachyonLoaderEffect::apply_to_last_glyph(
                f.buffer_mut(),
                popup_area,
                self.spinner_frame,
            );
        }
    }
}

/// Style for the `OK` confirmation button.
fn ok_button_style() -> Style {
    Style::default()
        .fg(palette::surface_overlay())
        .bg(palette::accent())
        .add_modifier(Modifier::BOLD)
}

/// Style for the loading spinner text.
fn loading_indicator_style() -> Style {
    Style::default()
        .fg(palette::accent())
        .add_modifier(Modifier::BOLD)
}

/// Splits numbered section titles like `1. Pull` or `2) Push`.
fn split_prefixed_title(line: &str) -> Option<&str> {
    if let Some(dot_index) = line.find('.') {
        if dot_index == 0 {
            return None;
        }

        if !line[..dot_index]
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            return None;
        }

        return Some(line[dot_index + 1..].trim());
    }

    let parenthesis_index = line.find(')')?;
    if parenthesis_index == 0 {
        return None;
    }

    if !line[..parenthesis_index]
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        return None;
    }

    Some(line[parenthesis_index + 1..].trim())
}

#[cfg(test)]
#[path = "info_overlay_test.rs"]
mod tests;
