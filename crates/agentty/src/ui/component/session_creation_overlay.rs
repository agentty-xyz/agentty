use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::ui::style::palette;
use crate::ui::{Component, overlay};

/// Minimum popup height that leaves room for title, options, and hints
/// without adding unused vertical space.
const MIN_OVERLAY_HEIGHT: u16 = 11;
/// Minimum popup width sized for the longest hint plus shared overlay chrome.
const MIN_OVERLAY_WIDTH: u16 = 44;
/// Fixed description width so option labels share one left edge.
const OPTION_DETAIL_WIDTH: usize = 20;
/// Fixed label width for `Regular`, `Draft`, and `Stacked`.
const OPTION_LABEL_WIDTH: usize = 7;
/// Popup height as a percentage of the available render area on tall terminals.
const OVERLAY_HEIGHT_PERCENT: u16 = 22;
/// Popup width as a percentage of the available render area on wide terminals.
const OVERLAY_WIDTH_PERCENT: u16 = 30;

/// Centered popup used to choose the type of session to create.
pub struct SessionCreationOverlay {
    selected_option_index: usize,
}

impl SessionCreationOverlay {
    /// Creates a session creation selector with the provided highlighted row.
    pub fn new(selected_option_index: usize) -> Self {
        Self {
            selected_option_index,
        }
    }

    /// Returns the centered popup rectangle for the compact session selector.
    fn popup_area(area: Rect) -> Rect {
        overlay::centered_popup_area(
            area,
            OVERLAY_WIDTH_PERCENT,
            OVERLAY_HEIGHT_PERCENT,
            MIN_OVERLAY_WIDTH,
            MIN_OVERLAY_HEIGHT,
        )
    }

    /// Returns all render lines for this popup.
    ///
    /// The header and bottom help hint rows are centered, while option rows
    /// keep a fixed text width so labels align while the option block stays
    /// visually centered.
    fn lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::from(vec![Span::styled(
                "Select session type",
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center),
            Line::from(""),
            self.option_line(0, "Regular", "Start immediately", false),
            self.option_line(1, "Draft", "Stage locally first", false),
            self.option_line(2, "Stacked", "Coming soon...", true),
            Line::from(""),
            Line::from(vec![Span::styled(
                "j/k: move | Enter: select | q: close",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        ]
    }

    /// Builds one option row and applies the active selection style.
    fn option_line(
        &self,
        option_index: usize,
        label: &'static str,
        detail: &'static str,
        is_disabled: bool,
    ) -> Line<'static> {
        let is_selected = self.selected_option_index == option_index && !is_disabled;
        let row_text = format!(" {label:<OPTION_LABEL_WIDTH$}  {detail:<OPTION_DETAIL_WIDTH$} ");

        if is_selected {
            return Line::from(vec![Span::styled(
                row_text,
                Style::default()
                    .fg(palette::surface_overlay())
                    .bg(palette::accent())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center);
        }

        let (label_style, detail_style) = if is_disabled {
            (
                Style::default().fg(palette::text_subtle()),
                Style::default().fg(palette::text_subtle()),
            )
        } else {
            (
                Style::default()
                    .fg(palette::accent())
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(palette::text_muted()),
            )
        };

        Line::from(vec![
            Span::styled(" ", Style::default().fg(palette::text_subtle())),
            Span::styled(format!("{label:<OPTION_LABEL_WIDTH$}"), label_style),
            Span::styled("  ", Style::default().fg(palette::text_subtle())),
            Span::styled(format!("{detail:<OPTION_DETAIL_WIDTH$}"), detail_style),
            Span::styled(" ", Style::default().fg(palette::text_subtle())),
        ])
        .alignment(Alignment::Center)
    }
}

impl Component for SessionCreationOverlay {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = Self::popup_area(area);
        let paragraph = Paragraph::new(self.lines())
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block("New Session", palette::accent()));

        f.render_widget(Clear, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_creation_overlay_new_stores_selected_option() {
        // Arrange
        let selected_option_index = 1;

        // Act
        let overlay = SessionCreationOverlay::new(selected_option_index);

        // Assert
        assert_eq!(overlay.selected_option_index, selected_option_index);
    }

    #[test]
    fn test_session_creation_overlay_popup_area_uses_compact_minimum() {
        // Arrange
        let area = Rect::new(0, 0, 80, 20);

        // Act
        let popup_area = SessionCreationOverlay::popup_area(area);

        // Assert
        assert_eq!(popup_area.width, 44);
        assert_eq!(popup_area.height, 11);
        assert_eq!(popup_area.x, 18);
        assert_eq!(popup_area.y, 4);
    }

    #[test]
    fn test_session_creation_overlay_popup_area_scales_modestly_on_wide_terminals() {
        // Arrange
        let area = Rect::new(0, 0, 240, 60);

        // Act
        let popup_area = SessionCreationOverlay::popup_area(area);

        // Assert
        assert_eq!(popup_area.width, 72);
        assert_eq!(popup_area.height, 13);
        assert_eq!(popup_area.x, 84);
        assert_eq!(popup_area.y, 23);
    }

    #[test]
    fn test_session_creation_overlay_render_shows_regular_and_draft_options() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay = SessionCreationOverlay::new(0);

        // Act
        terminal
            .draw(|frame| {
                Component::render(&overlay, frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("Regular"));
        assert!(text.contains("Draft"));
        assert!(text.contains("Stacked"));
        assert!(text.contains("Coming soon..."));
        assert!(text.contains("j/k: move | Enter: select | q: close"));
    }

    #[test]
    fn test_session_creation_overlay_aligns_option_text_left() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let overlay = SessionCreationOverlay::new(1);

        // Act
        terminal
            .draw(|frame| {
                Component::render(&overlay, frame, frame.area());
            })
            .expect("failed to draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let regular_position =
            text_position(buffer, "Regular").expect("regular option should render");
        let draft_position = text_position(buffer, "Draft").expect("draft option should render");
        assert_eq!(regular_position.0, draft_position.0);
    }

    #[test]
    fn test_session_creation_overlay_lines_center_header_and_help_text() {
        // Arrange
        let overlay = SessionCreationOverlay::new(0);

        // Act
        let lines = overlay.lines();
        let header_line = lines.first().expect("overlay should include a header line");
        let help_line = lines.last().expect("overlay should include a help line");

        // Assert
        assert_eq!(header_line.alignment, Some(Alignment::Center));
        assert_eq!(help_line.alignment, Some(Alignment::Center));
    }

    #[test]
    fn test_session_creation_overlay_lines_disable_stacked_option() {
        // Arrange
        let overlay = SessionCreationOverlay::new(2);

        // Act
        let lines = overlay.lines();
        let stacked_line = &lines[4];

        // Assert
        assert!(
            stacked_line
                .spans
                .iter()
                .all(|span| span.style.bg != Some(palette::accent()))
        );
        assert!(
            stacked_line
                .spans
                .iter()
                .any(|span| span.content.contains("Coming soon..."))
        );
    }

    /// Finds the first row and column where `needle` starts in the test buffer.
    fn text_position(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
        let needle_symbols = needle
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        let width = usize::from(buffer.area.width.max(1));

        for (row_index, row) in buffer.content().chunks(width).enumerate() {
            for (column_index, window) in row.windows(needle_symbols.len()).enumerate() {
                let window_matches = window
                    .iter()
                    .zip(&needle_symbols)
                    .all(|(cell, symbol)| cell.symbol() == symbol);

                if window_matches {
                    let column = u16::try_from(column_index).ok()?;
                    let row = u16::try_from(row_index).ok()?;

                    return Some((column, row));
                }
            }
        }

        None
    }
}
