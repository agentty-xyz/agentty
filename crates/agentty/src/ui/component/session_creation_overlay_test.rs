//! Session creation popup sizing, option availability, and rendering unit
//! tests.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier};

use super::{
    AGENT_CLI_REQUIRED_DETAIL, LAST_OPTION_INDEX, OVERLAY_DIMENSIONS, SessionCreationOverlay,
};
use crate::domain::theme::ColorTheme;
use crate::ui::Component;
use crate::ui::style::{palette, scoped_active_theme};

#[test]
fn test_default_theme_disabled_options_remain_readable_and_unselected() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let overlay = SessionCreationOverlay::new(0, false);

    // Act
    terminal
        .draw(|frame| Component::render(&overlay, frame, frame.area()))
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    for text in ["Regular", "Draft", AGENT_CLI_REQUIRED_DETAIL] {
        let (column, row) = text_position(buffer, text).expect("disabled option should render");
        for offset in 0..u16::try_from(text.len()).expect("option text fits terminal width") {
            let cell = &buffer[(column + offset, row)];
            assert_eq!(cell.fg, Color::Gray);
            assert_eq!(cell.bg, palette::surface_overlay());
            assert!(!cell.modifier.contains(Modifier::BOLD));
        }
    }
}

#[test]
fn test_session_creation_overlay_new_stores_selected_option() {
    // Arrange
    let selected_option_index = 1;

    // Act
    let overlay = SessionCreationOverlay::new(selected_option_index, true);

    // Assert
    assert_eq!(overlay.selected_option_index, selected_option_index);
    assert!(overlay.can_start_sessions);
}

#[test]
fn test_session_creation_overlay_popup_area_uses_compact_minimum() {
    // Arrange
    let area = Rect::new(0, 0, 80, 20);

    // Act
    let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert
    assert_eq!(popup_area.width, 45);
    assert_eq!(popup_area.height, 10);
    assert_eq!(popup_area.x, 17);
    assert_eq!(popup_area.y, 5);
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
    let overlay = SessionCreationOverlay::new(0, true);

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
    assert!(!text.contains("Harness"));
    assert!(!text.contains("Orchestrator"));
    assert!(!text.contains("Stacked"));
    assert!(!text.contains("Append to stack"));
    assert!(text.contains("j/k: move | Enter: select | q: close"));
}

#[test]
fn test_session_creation_overlay_aligns_option_text_left() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let overlay = SessionCreationOverlay::new(LAST_OPTION_INDEX, true);

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
    let overlay = SessionCreationOverlay::new(0, true);

    // Act
    let lines = overlay.lines();
    let header_line = lines.first().expect("overlay should include a header line");
    let help_line = lines.last().expect("overlay should include a help line");

    // Assert
    assert_eq!(header_line.alignment, Some(Alignment::Center));
    assert_eq!(help_line.alignment, Some(Alignment::Center));
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

#[test]
fn test_session_creation_overlay_highlights_only_selected_row() {
    // Arrange
    let overlay = SessionCreationOverlay::new(LAST_OPTION_INDEX, true);

    // Act
    let lines = overlay.lines();

    // Assert
    let regular_line = &lines[2];
    assert!(regular_line.spans.iter().any(|span| {
        span.style.fg == Some(palette::accent()) && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    let draft_line = &lines[2 + LAST_OPTION_INDEX];
    assert_eq!(draft_line.spans.len(), 1);
    assert!(draft_line.spans[0].content.contains("Stage locally first"));
    assert_eq!(
        draft_line.spans[0].style.bg,
        Some(palette::surface_selection())
    );
    assert_eq!(
        draft_line.spans[0].style.fg,
        Some(palette::text_selection())
    );
}

#[test]
fn test_session_creation_overlay_disables_rows_without_runnable_agent() {
    // Arrange
    let overlay = SessionCreationOverlay::new(0, false);

    // Act
    let lines = overlay.lines();

    // Assert
    for option_line in &lines[2..=2 + LAST_OPTION_INDEX] {
        assert!(
            option_line
                .spans
                .iter()
                .any(|span| span.content.contains(AGENT_CLI_REQUIRED_DETAIL))
        );
        assert!(option_line.spans.iter().all(|span| {
            span.style.bg != Some(palette::surface_selection())
                && !span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }
}
