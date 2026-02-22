use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::ui::Component;

const MIN_OVERLAY_HEIGHT: u16 = 7;
const MIN_OVERLAY_WIDTH: u16 = 38;
const OVERLAY_HEIGHT_PERCENT: u16 = 20;
const OVERLAY_WIDTH_PERCENT: u16 = 45;

/// Centered informational popup used for non-destructive workflow guidance.
pub struct InfoOverlay<'a> {
    message: &'a str,
    title: &'a str,
}

impl<'a> InfoOverlay<'a> {
    /// Creates an informational popup with title and body message.
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self { message, title }
    }
}

impl Component for InfoOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let width = (area.width * OVERLAY_WIDTH_PERCENT / 100)
            .max(MIN_OVERLAY_WIDTH)
            .min(area.width);
        let height = (area.height * OVERLAY_HEIGHT_PERCENT / 100)
            .max(MIN_OVERLAY_HEIGHT)
            .min(area.height);
        let popup_area = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        );

        let title = format!(" {} ", self.title);
        let ok_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(
                self.message,
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(Span::styled(" OK ", ok_style)),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Span::styled(title, Style::default().fg(Color::Yellow))),
        );

        f.render_widget(Clear, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_info_overlay_new_stores_fields() {
        // Arrange
        let message = "Sync is blocked";
        let title = "Sync blocked";

        // Act
        let overlay = InfoOverlay::new(title, message);

        // Assert
        assert_eq!(overlay.message, message);
        assert_eq!(overlay.title, title);
    }

    #[test]
    fn test_info_overlay_render_includes_ok_indicator() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay = InfoOverlay::new("Sync blocked", "Main has uncommitted changes");

        // Act
        terminal
            .draw(|f| {
                let area = f.area();
                crate::ui::Component::render(&overlay, f, area);
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let content = buffer.content();
        let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(text.contains("OK"));
        assert!(text.contains("Main has uncommitted changes"));
    }
}
