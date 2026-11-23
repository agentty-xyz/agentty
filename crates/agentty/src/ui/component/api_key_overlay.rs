//! Popup that collects a harness provider's API key with every character
//! masked.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::presentation::api_key::API_KEY_TITLE;
use crate::ui::component::chat_input::ChatInput;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const HELP_TEXT: &str = "Enter: save | Esc: cancel";
const INPUT_TITLE: &str = "API Key";
const INPUT_PLACEHOLDER: &str = "Paste the key";
const MIN_OVERLAY_HEIGHT: u16 = 12;
const MIN_OVERLAY_WIDTH: u16 = 58;
/// Popup dimensions for API key entry.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(62, 42, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);

/// Centered popup with an explanation above a masked API key field.
pub struct ApiKeyOverlay<'a> {
    cursor: usize,
    masked_input: &'a str,
    message: &'a str,
}

impl<'a> ApiKeyOverlay<'a> {
    /// Creates the popup showing `message` above `masked_input`, with the
    /// cursor at character `cursor`.
    pub fn new(message: &'a str, masked_input: &'a str, cursor: usize) -> Self {
        Self {
            cursor,
            masked_input,
            message,
        }
    }
}

impl Component for ApiKeyOverlay<'_> {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);
        let block = overlay::overlay_block(API_KEY_TITLE, palette::accent());
        let inner_area = block.inner(popup_area);
        let sections = Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner_area);
        let muted = Style::default().fg(palette::text_muted());
        let message = Paragraph::new(
            self.message
                .lines()
                .map(|line| Line::from(Span::styled(line, muted)))
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: true });
        let help =
            Paragraph::new(Line::from(Span::styled(HELP_TEXT, muted)).alignment(Alignment::Center));
        let input = ChatInput::new(INPUT_TITLE, self.masked_input, self.cursor)
            .placeholder(INPUT_PLACEHOLDER)
            .clear_style(
                Style::default()
                    .fg(palette::text())
                    .bg(palette::surface_overlay()),
            );

        overlay::clear_popup_area(frame, popup_area);
        frame.render_widget(block, popup_area);
        frame.render_widget(message, sections[0]);
        input.render(frame, sections[1]);
        frame.render_widget(help, sections[2]);
    }
}

#[cfg(test)]
#[path = "api_key_overlay_test.rs"]
mod tests;
