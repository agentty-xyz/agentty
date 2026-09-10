use ratatui::layout::{Alignment, Rect};

use super::{
    APPEND_TO_STACK_DISABLED_PREVIEW_DETAIL, APPEND_TO_STACK_PREVIEW_DETAIL,
    ORCHESTRATOR_SESSION_PREVIEW_DETAIL, OVERLAY_DIMENSIONS, STACKED_SESSION_DETAIL,
    SessionCreationOverlay,
};
use crate::ui::Component;
use crate::ui::style::palette;

#[test]
fn test_session_creation_overlay_new_stores_selected_option() {
    // Arrange
    let selected_option_index = 1;

    // Act
    let overlay = SessionCreationOverlay::new(selected_option_index, true, true);

    // Assert
    assert_eq!(overlay.selected_option_index, selected_option_index);
    assert!(overlay.can_create_stacked_session);
    assert!(overlay.can_append_to_stack);
}

#[test]
fn test_session_creation_overlay_popup_area_uses_compact_minimum() {
    // Arrange
    let area = Rect::new(0, 0, 80, 20);

    // Act
    let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert
    assert_eq!(popup_area.width, 53);
    assert_eq!(popup_area.height, 13);
    assert_eq!(popup_area.x, 13);
    assert_eq!(popup_area.y, 3);
}

#[test]
fn test_session_creation_overlay_popup_area_scales_modestly_on_wide_terminals() {
    // Arrange
    let area = Rect::new(0, 0, 240, 60);

    // Act
    let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert
    assert_eq!(popup_area.width, 72);
    assert_eq!(popup_area.height, 13);
    assert_eq!(popup_area.x, 84);
    assert_eq!(popup_area.y, 23);
}

#[test]
fn test_session_creation_overlay_render_shows_session_options() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let overlay = SessionCreationOverlay::new(0, true, true);

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
    assert!(text.contains(ORCHESTRATOR_SESSION_PREVIEW_DETAIL));
    assert!(text.contains("Stacked"));
    assert!(text.contains(STACKED_SESSION_DETAIL));
    assert!(text.contains("Append to stack"));
    assert!(text.contains(APPEND_TO_STACK_PREVIEW_DETAIL));
    assert!(!text.contains("[Preview] Stack on selected"));
    assert!(text.contains("j/k: move | Enter: select | q: close"));
}

#[test]
fn test_session_creation_overlay_aligns_option_text_left() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let overlay = SessionCreationOverlay::new(1, false, false);

    // Act
    terminal
        .draw(|frame| {
            Component::render(&overlay, frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let regular_position = text_position(buffer, "Regular").expect("regular option should render");
    let draft_position = text_position(buffer, "Draft").expect("draft option should render");
    assert_eq!(regular_position.0, draft_position.0);
}

#[test]
fn test_session_creation_overlay_lines_center_header_and_help_text() {
    // Arrange
    let overlay = SessionCreationOverlay::new(0, false, false);

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
    let overlay = SessionCreationOverlay::new(2, false, false);

    // Act
    let lines = overlay.lines();
    let stacked_line = &lines[5];

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
            .any(|span| span.content.contains("Select parent first"))
    );
}

#[test]
fn test_session_creation_overlay_lines_enable_stacked_option() {
    // Arrange
    let overlay = SessionCreationOverlay::new(3, true, false);

    // Act
    let lines = overlay.lines();
    let stacked_line = &lines[5];

    // Assert
    assert!(
        stacked_line
            .spans
            .iter()
            .any(|span| span.style.bg == Some(palette::accent()))
    );
    assert!(
        stacked_line
            .spans
            .iter()
            .any(|span| span.content.contains(STACKED_SESSION_DETAIL))
    );
}

#[test]
fn test_session_creation_overlay_lines_gate_append_to_stack_option() {
    // Arrange
    let disabled_overlay = SessionCreationOverlay::new(2, true, false);
    let enabled_overlay = SessionCreationOverlay::new(4, true, true);

    // Act
    let disabled_lines = disabled_overlay.lines();
    let enabled_lines = enabled_overlay.lines();
    let disabled_line = &disabled_lines[6];
    let enabled_line = &enabled_lines[6];

    // Assert
    assert!(disabled_line.spans.iter().any(|span| {
        span.content
            .contains(APPEND_TO_STACK_DISABLED_PREVIEW_DETAIL)
            && span.style.bg != Some(palette::accent())
    }));
    assert!(enabled_line.spans.iter().any(|span| {
        span.content.contains(APPEND_TO_STACK_PREVIEW_DETAIL)
            && span.style.bg == Some(palette::accent())
    }));
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
