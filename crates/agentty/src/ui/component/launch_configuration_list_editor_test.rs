use super::{LaunchConfigurationListEditor, command_window_start, format_input_with_cursor};
use crate::domain::input::InputState;
use crate::presentation::setting::{
    LaunchConfigurationListEditorMode, LaunchConfigurationListEditorSnapshot,
};
use crate::ui::render::Component;
use crate::ui::style::palette;

#[test]
fn test_launch_configuration_list_editor_renders_browse_help() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let editor = LaunchConfigurationListEditorSnapshot {
        commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
        input: None,
        mode: LaunchConfigurationListEditorMode::Browse,
        selected_index: 1,
    };
    let component = LaunchConfigurationListEditor::new(&editor);

    // Act
    terminal
        .draw(|frame| {
            component.render(frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("Launch Configurations"));
    assert!(text.contains("cargo test"));
    assert!(text.contains("npm run dev"));
    assert!(text.contains("a: add"));
    assert!(text.contains("J/K: reorder"));
}

#[test]
fn test_launch_configuration_list_editor_selected_row_uses_background_without_marker() {
    // Arrange
    let editor = LaunchConfigurationListEditorSnapshot {
        commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
        input: None,
        mode: LaunchConfigurationListEditorMode::Browse,
        selected_index: 1,
    };
    let component = LaunchConfigurationListEditor::new(&editor);

    // Act
    let lines = component.lines(24, 12);
    let selected_line = &lines[1];
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
fn test_launch_configuration_list_editor_input_lines_show_cursor() {
    // Arrange
    let input = InputState::with_text("cargo test".to_string());
    let editor = LaunchConfigurationListEditorSnapshot {
        commands: Vec::new(),
        input: Some(input),
        mode: LaunchConfigurationListEditorMode::Add,
        selected_index: 0,
    };
    let component = LaunchConfigurationListEditor::new(&editor);

    // Act
    let lines = component.lines(24, 12);
    let input_line_text = lines[2].to_string();

    // Assert
    assert!(lines[0].to_string().contains("Add command"));
    assert!(input_line_text.contains("cargo test|"));
    assert!(lines[4].to_string().contains("Enter: save"));
}

#[test]
fn test_command_window_start_keeps_tail_selection_visible() {
    // Arrange
    let command_count = 10;
    let selected_index = 9;
    let visible_command_count = 4;

    // Act
    let window_start = command_window_start(command_count, selected_index, visible_command_count);

    // Assert
    assert_eq!(window_start, 6);
}

#[test]
fn test_format_input_with_cursor_clamps_to_end() {
    // Arrange
    let mut input = InputState::with_text("abc".to_string());
    input.cursor = 99;

    // Act
    let rendered = format_input_with_cursor(&input);

    // Assert
    assert_eq!(rendered, "abc|");
}
