use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::{Component, style};

/// Symbol used to draw the scrollbar track.
pub(crate) const SCROLLBAR_TRACK_SYMBOL: &str = "│";
/// Symbol used to draw the scrollbar thumb.
pub(crate) const SCROLLBAR_THUMB_SYMBOL: &str = "█";
/// Slim vertical scrollbar rendered within a caller-provided track area.
pub(crate) struct VerticalScrollbar {
    scroll_offset: u16,
    total_lines: usize,
}

impl VerticalScrollbar {
    /// Creates a scrollbar for the current offset and full content length.
    pub(crate) fn new(scroll_offset: u16, total_lines: usize) -> Self {
        Self {
            scroll_offset,
            total_lines,
        }
    }

    /// Returns whether the content extends beyond the visible viewport.
    fn has_scrollable_overflow(&self, viewport_height: u16) -> bool {
        viewport_height > 0 && self.total_lines > usize::from(viewport_height)
    }

    /// Returns the thumb offset and height for the provided track height.
    fn thumb_geometry(&self, track_height: usize) -> (usize, usize) {
        let thumb_height = (track_height * track_height / self.total_lines).max(1);
        let max_scroll = self.total_lines.saturating_sub(track_height);
        let max_thumb_offset = track_height.saturating_sub(thumb_height);
        let thumb_offset = usize::from(self.scroll_offset)
            .min(max_scroll)
            .saturating_mul(max_thumb_offset)
            .checked_div(max_scroll)
            .unwrap_or(0);

        (thumb_offset, thumb_height)
    }
}

impl Component for VerticalScrollbar {
    fn render(&self, f: &mut Frame, area: Rect) {
        if !self.has_scrollable_overflow(area.height) {
            return;
        }

        let track_height = usize::from(area.height);
        let (thumb_offset, thumb_height) = self.thumb_geometry(track_height);
        let mut scrollbar_lines = Vec::with_capacity(track_height);

        for line_index in 0..track_height {
            let is_thumb_line =
                line_index >= thumb_offset && line_index < thumb_offset + thumb_height;
            let (symbol, symbol_style) = if is_thumb_line {
                (
                    SCROLLBAR_THUMB_SYMBOL,
                    Style::default().fg(style::palette::warning()),
                )
            } else {
                (
                    SCROLLBAR_TRACK_SYMBOL,
                    Style::default().fg(style::palette::text_subtle()),
                )
            };

            scrollbar_lines.push(Line::from(Span::styled(symbol, symbol_style)));
        }

        f.render_widget(Paragraph::new(scrollbar_lines), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thumb_geometry_clamps_overscroll_to_track_bottom() {
        // Arrange
        let scrollbar = VerticalScrollbar::new(u16::MAX, 40);

        // Act
        let (thumb_offset, thumb_height) = scrollbar.thumb_geometry(8);

        // Assert
        assert_eq!(thumb_offset + thumb_height, 8);
    }
}
