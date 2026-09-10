use ratatui::layout::Rect;

use super::{
    OVERLAY_DIMENSIONS, PublishBranchOverlay, REVIEW_REQUEST_EDITABLE_HELP_TEXT,
    REVIEW_REQUEST_LOCKED_HELP_TEXT, REVIEW_REQUEST_TITLE,
};
use crate::domain::input::InputState;
use crate::domain::theme::ColorTheme;
use crate::test_support;
use crate::ui::render::Component;
use crate::ui::style;

#[test]
fn test_publish_branch_overlay_popup_area_is_centered() {
    // Arrange
    let area = Rect::new(0, 0, 120, 40);

    // Act
    let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert
    assert_eq!(popup_area.width, 74);
    assert_eq!(popup_area.height, 16);
    assert_eq!(popup_area.x, 23);
    assert_eq!(popup_area.y, 12);
}

#[test]
fn test_publish_branch_overlay_render_contains_placeholder_and_help_text() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let input = InputState::default();
    let overlay = PublishBranchOverlay::new(&input, "wt/ff45463f", None);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains(REVIEW_REQUEST_TITLE));
    assert!(text.contains("Enter: publish review request"));
    assert!(text.contains("Leave blank to push as `wt/ff45463f`"));
    assert!(text.contains("create or refresh"));
    assert!(text.contains("review request"));
}

#[test]
fn test_publish_branch_overlay_render_shows_locked_upstream_message() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let input = InputState::with_text("review/custom".to_string());
    let overlay = PublishBranchOverlay::new(&input, "wt/ff45463f", Some("origin/review/custom"));

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("origin/review/custom"));
    assert!(text.contains("review/custom"));
    assert!(text.contains("review request"));
    assert!(text.contains(REVIEW_REQUEST_LOCKED_HELP_TEXT));
}

#[test]
fn test_publish_branch_overlay_green_theme_uses_session_list_text_color_for_locked_branch() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Green);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let input = InputState::with_text("review/custom".to_string());
    let overlay = PublishBranchOverlay::new(&input, "wt/ff45463f", Some("origin/review/custom"));

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
        })
        .expect("failed to draw");

    // Assert
    let branch_cells =
        test_support::rendered_text_start_cells(terminal.backend().buffer(), "review/custom");
    assert!(
        branch_cells
            .iter()
            .any(|branch_cell| branch_cell.fg == style::palette::text())
    );
}

#[test]
fn test_publish_pull_request_overlay_render_shows_pull_request_copy() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let input = InputState::with_text("review/custom".to_string());
    let overlay = PublishBranchOverlay::new(&input, "wt/ff45463f", None);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            overlay.render(frame, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains(REVIEW_REQUEST_TITLE));
    assert!(text.contains("review request"));
    assert!(text.contains(REVIEW_REQUEST_EDITABLE_HELP_TEXT));
}
