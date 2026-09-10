use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const MIN_OVERLAY_HEIGHT: u16 = 7;
const MIN_OVERLAY_WIDTH: u16 = 30;
const OPTION_LAYOUT_CHROME: usize = 13;
const OVERLAY_HEIGHT_PERCENT: u16 = 20;
const OVERLAY_WIDTH_PERCENT: u16 = 40;

/// Centered binary-choice popup used for confirmations and explicit choices.
///
/// The message body is truncated to a single visible line so confirmation
/// choices remain visible even when session titles are very long.
pub struct ConfirmationOverlay<'a> {
    first_option_label: &'a str,
    message: &'a str,
    second_option_label: &'a str,
    selected_first: bool,
    title: &'a str,
}

impl<'a> ConfirmationOverlay<'a> {
    /// Creates a confirmation popup with title and body message.
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self {
            first_option_label: "Yes",
            message,
            second_option_label: "No",
            selected_first: false,
            title,
        }
    }

    /// Sets the two visible option labels.
    #[must_use]
    pub fn option_labels(mut self, first: &'a str, second: &'a str) -> Self {
        self.first_option_label = first;
        self.second_option_label = second;

        self
    }

    /// Sets whether the first option is currently selected.
    #[must_use]
    pub fn selected_first(mut self, selected_first: bool) -> Self {
        self.selected_first = selected_first;

        self
    }
}

impl Component for ConfirmationOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let option_width = self.first_option_label.chars().count()
            + self.second_option_label.chars().count()
            + OPTION_LAYOUT_CHROME;
        let min_overlay_width = u16::try_from(option_width)
            .unwrap_or(u16::MAX)
            .max(MIN_OVERLAY_WIDTH);
        let popup_area = overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            OVERLAY_HEIGHT_PERCENT,
            min_overlay_width,
            MIN_OVERLAY_HEIGHT,
        );
        let message_width = overlay::overlay_content_width(popup_area.width);
        let message = truncate_with_ellipsis(self.message, message_width);

        let selected_option_style = Style::default()
            .fg(palette::surface_overlay())
            .bg(palette::accent())
            .add_modifier(Modifier::BOLD);
        let unselected_option_style = Style::default().fg(palette::text());
        let first_option_style = if self.selected_first {
            selected_option_style
        } else {
            unselected_option_style
        };
        let second_option_style = if self.selected_first {
            unselected_option_style
        } else {
            selected_option_style
        };

        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(message, Style::default().fg(palette::text()))),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!(" {} ", self.first_option_label), first_option_style),
                Span::styled("   ", Style::default()),
                Span::styled(
                    format!(" {} ", self.second_option_label),
                    second_option_style,
                ),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(overlay::overlay_block(self.title, palette::warning()));

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
#[path = "confirmation_overlay_test.rs"]
mod tests;
