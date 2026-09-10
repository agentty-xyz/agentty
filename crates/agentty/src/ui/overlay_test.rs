use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Block;

use super::{
    centered_popup_area, clear_popup_area, overlay_content_width, overlay_required_height,
    sync_popup_message,
};
use crate::ui::style::palette;

#[test]
fn test_sync_popup_message_with_project_and_branch() {
    // Arrange
    let default_branch = Some("develop");
    let detail_message = "Synchronizing with its upstream.";
    let project_name = Some("agentty");

    // Act
    let message = sync_popup_message(default_branch, detail_message, project_name);

    // Assert
    assert_eq!(
        message,
        "Project `agentty` on main branch `develop`.\n\nSynchronizing with its upstream."
    );
}

#[test]
fn test_sync_popup_message_with_project_only() {
    // Arrange
    let default_branch = None;
    let detail_message = "Synchronization is blocked.";
    let project_name = Some("agentty");

    // Act
    let message = sync_popup_message(default_branch, detail_message, project_name);

    // Assert
    assert_eq!(message, "Project `agentty`.\n\nSynchronization is blocked.");
}

#[test]
fn test_sync_popup_message_with_branch_only() {
    // Arrange
    let default_branch = Some("main");
    let detail_message = "Synchronization is blocked.";
    let project_name = None;

    // Act
    let message = sync_popup_message(default_branch, detail_message, project_name);

    // Assert
    assert_eq!(
        message,
        "Main branch `main`.\n\nSynchronization is blocked."
    );
}

#[test]
fn test_sync_popup_message_without_project_or_branch() {
    // Arrange
    let default_branch = None;
    let detail_message = "Synchronization is blocked.";
    let project_name = None;

    // Act
    let message = sync_popup_message(default_branch, detail_message, project_name);

    // Assert
    assert_eq!(message, "Synchronization is blocked.");
}

#[test]
fn test_centered_popup_area_centers_within_bounds() {
    // Arrange
    let area = Rect::new(0, 0, 100, 50);

    // Act
    let popup_area = centered_popup_area(area, 40, 20, 30, 7);

    // Assert
    assert_eq!(popup_area.width, 40);
    assert_eq!(popup_area.height, 10);
    assert_eq!(popup_area.x, 30);
    assert_eq!(popup_area.y, 20);
}

#[test]
fn test_centered_popup_area_clamps_to_small_terminal() {
    // Arrange
    let area = Rect::new(0, 0, 20, 6);

    // Act
    let popup_area = centered_popup_area(area, 50, 50, 30, 10);

    // Assert
    assert_eq!(popup_area.width, 20);
    assert_eq!(popup_area.height, 6);
    assert_eq!(popup_area.x, 0);
    assert_eq!(popup_area.y, 0);
}

#[test]
fn test_centered_popup_area_respects_minimum_size_before_centering() {
    // Arrange
    let area = Rect::new(10, 5, 80, 40);

    // Act
    let popup_area = centered_popup_area(area, 10, 10, 30, 12);

    // Assert
    assert_eq!(popup_area.width, 30);
    assert_eq!(popup_area.height, 12);
    assert_eq!(popup_area.x, 35);
    assert_eq!(popup_area.y, 19);
}

#[test]
fn test_overlay_content_width_subtracts_shared_frame_chrome() {
    // Arrange
    let popup_width = 40;

    // Act
    let content_width = overlay_content_width(popup_width);

    // Assert
    assert_eq!(content_width, 34);
}

#[test]
fn test_overlay_content_width_keeps_minimum_width_for_tiny_popup() {
    // Arrange
    let popup_width = 1;

    // Act
    let content_width = overlay_content_width(popup_width);

    // Assert
    assert_eq!(content_width, 1);
}

#[test]
fn test_overlay_required_height_adds_shared_frame_chrome() {
    // Arrange
    let inner_line_count = 8;

    // Act
    let total_height = overlay_required_height(inner_line_count);

    // Assert
    assert_eq!(total_height, 12);
}

#[test]
fn test_overlay_required_height_saturates_at_u16_max() {
    // Arrange
    let inner_line_count = usize::MAX;

    // Act
    let total_height = overlay_required_height(inner_line_count);

    // Assert
    assert_eq!(total_height, u16::MAX);
}

#[test]
fn test_clear_popup_area_uses_overlay_surface_style() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(8, 4);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let initial_style = Style::default()
        .fg(palette::warning())
        .bg(palette::surface());

    // Act
    terminal
        .draw(|frame| {
            let area = Rect::new(2, 1, 3, 2);
            frame.render_widget(Block::default().style(initial_style), frame.area());
            clear_popup_area(frame, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    for y in 1..3 {
        for x in 2..5 {
            let cell = &buffer[(x, y)];
            assert_eq!(cell.symbol(), " ");
            assert_eq!(cell.fg, palette::text());
            assert_eq!(cell.bg, palette::surface_overlay());
        }
    }
}
