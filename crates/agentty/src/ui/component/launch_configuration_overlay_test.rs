use ratatui::layout::{Alignment, Rect};

use super::{LaunchConfigurationOverlay, OVERLAY_DIMENSIONS};
use crate::ui::render::Component;
use crate::ui::style::palette;

#[test]
fn test_launch_configuration_overlay_new_stores_default_selection() {
    // Arrange
    let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

    // Act
    let overlay = LaunchConfigurationOverlay::new(&commands);

    // Assert
    assert_eq!(overlay.commands, commands.as_slice());
    assert_eq!(overlay.selected_command_index, 0);
}

#[test]
fn test_launch_configuration_overlay_popup_area_is_centered() {
    // Arrange
    let area = Rect::new(0, 0, 120, 40);

    // Act
    let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);

    // Assert
    assert_eq!(popup_area.width, 74);
    assert_eq!(popup_area.height, 15);
    assert_eq!(popup_area.x, 23);
    assert_eq!(popup_area.y, 12);
}

#[test]
fn test_launch_configuration_overlay_render_contains_hint_text() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];
    let overlay = LaunchConfigurationOverlay::new(&commands).selected_command_index(1);

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
    assert!(text.contains("Select launch configuration"));
    assert!(text.contains("j/k: move | Enter: open | Esc: cancel"));
}

#[test]
fn test_launch_configuration_overlay_lines_selected_row_uses_background_without_marker() {
    // Arrange
    let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];
    let overlay = LaunchConfigurationOverlay::new(&commands).selected_command_index(1);

    // Act
    let lines = overlay.lines(24);
    let selected_line = &lines[3];
    let selected_text: String = selected_line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();

    // Assert
    assert!(!selected_text.contains('>'));
    assert!(
        selected_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(palette::accent()))
    );
}

#[test]
fn test_launch_configuration_overlay_lines_center_bottom_help_text() {
    // Arrange
    let commands = vec!["cargo test".to_string()];
    let overlay = LaunchConfigurationOverlay::new(&commands);

    // Act
    let lines = overlay.lines(24);
    let help_line = lines
        .last()
        .expect("overlay should include a bottom help line");

    // Assert
    assert_eq!(help_line.alignment, Some(Alignment::Center));
}

#[test]
fn test_launch_configuration_overlay_lines_center_header_text() {
    // Arrange
    let commands = vec!["cargo test".to_string()];
    let overlay = LaunchConfigurationOverlay::new(&commands);

    // Act
    let lines = overlay.lines(24);
    let header_line = lines.first().expect("overlay should include a header line");

    // Assert
    assert_eq!(header_line.alignment, Some(Alignment::Center));
}
