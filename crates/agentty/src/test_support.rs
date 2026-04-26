//! Hidden support APIs for tests.
//!
//! These helpers intentionally live outside the production-facing module
//! surface so tests can share canonical naming and render-buffer rules without
//! widening app APIs.

use std::path::{Path, PathBuf};

use ratatui::buffer::{Buffer, Cell};

use crate::app;

/// Returns the canonical session folder path for integration-test fixtures.
pub fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    app::session::session_folder(base, session_id)
}

/// Returns the first rendered cell for a contiguous text match in a test
/// buffer.
pub fn rendered_text_start_cell<'a>(buffer: &'a Buffer, needle: &str) -> Option<&'a Cell> {
    rendered_text_start_cells(buffer, needle).into_iter().next()
}

/// Returns rendered start cells for every contiguous text match in a test
/// buffer.
pub fn rendered_text_start_cells<'a>(buffer: &'a Buffer, needle: &str) -> Vec<&'a Cell> {
    let width = usize::from(buffer.area.width.max(1));
    let needle_symbols = needle.chars().map(|character| character.to_string());
    let needle_symbols = needle_symbols.collect::<Vec<_>>();
    let content = buffer.content();
    let mut cells = Vec::new();

    for row_start in (0..content.len()).step_by(width) {
        let row_end = row_start + width.min(content.len().saturating_sub(row_start));
        let row = &content[row_start..row_end];

        for (index, window) in row.windows(needle_symbols.len()).enumerate() {
            let window_matches = window
                .iter()
                .zip(&needle_symbols)
                .all(|(cell, symbol)| cell.symbol() == symbol);

            if window_matches {
                cells.push(&row[index]);
            }
        }
    }

    cells
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    #[test]
    fn rendered_text_start_cell_returns_first_match() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
        buffer.set_string(1, 0, "one", Style::default().fg(Color::Green));
        buffer.set_string(1, 1, "one", Style::default().fg(Color::Yellow));

        // Act
        let cell = rendered_text_start_cell(&buffer, "one").expect("text should render");

        // Assert
        assert_eq!(cell.fg, Color::Green);
    }

    #[test]
    fn rendered_text_start_cells_returns_all_matches() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
        buffer.set_string(1, 0, "same", Style::default().fg(Color::Green));
        buffer.set_string(1, 1, "same", Style::default().fg(Color::Yellow));

        // Act
        let cells = rendered_text_start_cells(&buffer, "same");
        let colors = cells.iter().map(|cell| cell.fg).collect::<Vec<_>>();

        // Assert
        assert_eq!(colors, vec![Color::Green, Color::Yellow]);
    }

    #[test]
    fn rendered_text_start_cell_returns_none_for_missing_text() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 1));
        buffer.set_string(1, 0, "present", Style::default());

        // Act
        let cell = rendered_text_start_cell(&buffer, "missing");

        // Assert
        assert!(cell.is_none());
    }
}
