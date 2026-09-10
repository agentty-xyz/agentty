use ratatui::style::Style;

use super::{ChatInput, SuggestionItem, SuggestionList};
use crate::domain::theme::ColorTheme;
use crate::test_support;
use crate::ui::render::Component;
use crate::ui::style;

/// Returns the rendered symbols for one buffer row.
fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
    let start = usize::from(row) * usize::from(width);
    let end = start + usize::from(width);

    buffer.content()[start..end]
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn test_builder_methods() {
    // Arrange
    let title = "Chat";
    let input = "Hello";
    let cursor = 5;
    let placeholder = "Start typing...";
    let suggestion_list = SuggestionList {
        items: vec![],
        selected_index: 0,
        title: "Menu".to_string(),
    };

    // Act
    let chat_input = ChatInput::new(title, input, cursor)
        .placeholder(placeholder)
        .suggestion_list(&suggestion_list);

    // Assert
    assert_eq!(chat_input.title, title);
    assert_eq!(chat_input.input, input);
    assert_eq!(chat_input.cursor, cursor);
    assert_eq!(chat_input.placeholder, placeholder);
    assert!(chat_input.suggestion_list.is_some());
    assert_eq!(
        chat_input
            .suggestion_list
            .expect("suggestion list should be set")
            .title,
        "Menu"
    );
}

#[test]
fn test_total_viewport_line_count_uses_cursor_row_when_cursor_is_below_last_display_line() {
    // Arrange
    let display_line_count = 1;
    let cursor_y = 1;

    // Act
    let total_line_count = ChatInput::total_viewport_line_count(display_line_count, cursor_y);

    // Assert
    assert_eq!(total_line_count, 2);
}

#[test]
fn test_render_uses_rounded_focused_frame_for_prompt_input() {
    // Arrange
    let width = 32;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let chat_input = ChatInput::new("Prompt", "", 0).placeholder("Type your message");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw prompt input");

    // Assert
    let top_row = buffer_row_text(terminal.backend().buffer(), 0, width);
    assert!(top_row.starts_with("╭"));
    assert!(top_row.contains(" Prompt "));
    assert!(top_row.contains("╮"));
}

#[test]
fn test_render_shows_session_status_beside_prompt_title() {
    // Arrange
    let width = 32;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let chat_input = ChatInput::new("Prompt", "", 0).status("Fast");

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw prompt input");

    // Assert
    let top_row = buffer_row_text(terminal.backend().buffer(), 0, width);
    assert!(top_row.contains(" Prompt · Fast "));
}

#[test]
fn test_render_inactive_uses_dimmed_border_style() {
    // Arrange
    let width = 32;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let chat_input = ChatInput::new("Prompt", "", 0)
        .placeholder("Type your message")
        .active(false);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw inactive prompt input");

    // Assert — border is rendered with the muted BORDER color, not ACCENT.
    let buffer = terminal.backend().buffer();
    let top_left_cell = &buffer.content()[0];
    assert_eq!(top_left_cell.fg, style::palette::border());
}

#[test]
fn test_render_inactive_with_text_uses_dimmed_border() {
    // Arrange — inactive input with text still uses the muted border.
    let width = 32;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let chat_input = ChatInput::new("Prompt", "hello", 5).active(false);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw inactive prompt input with text");

    // Assert — border still uses muted BORDER color.
    let buffer = terminal.backend().buffer();
    let top_left_cell = &buffer.content()[0];
    assert_eq!(top_left_cell.fg, style::palette::border());
}

#[test]
fn test_render_green_theme_uses_session_list_text_color_for_input_text() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Green);
    let width = 48;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let chat_input = ChatInput::new("Prompt", "typed-green", "typed-green".chars().count());

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw prompt input");

    // Assert
    let typed_cell =
        test_support::rendered_text_start_cell(terminal.backend().buffer(), "typed-green")
            .expect("typed input should render");
    assert_eq!(typed_cell.fg, style::palette::text());
}

#[test]
fn test_render_reapplies_configured_clear_style() {
    // Arrange
    let width = 48;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let clear_style = Style::default()
        .fg(style::palette::text())
        .bg(style::palette::surface_overlay());
    let chat_input = ChatInput::new("Prompt", "typed", 5).clear_style(clear_style);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw prompt input");

    // Assert
    let buffer = terminal.backend().buffer();
    let blank_cell = &buffer[(1, 2)];
    assert_eq!(blank_cell.fg, style::palette::text());
    assert_eq!(blank_cell.bg, style::palette::surface_overlay());
}

#[test]
fn test_render_uses_matching_rounded_dropdown_frame() {
    // Arrange
    let width = 40;
    let backend = ratatui::backend::TestBackend::new(width, 8);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let suggestion_list = SuggestionList {
        items: vec![SuggestionItem {
            badge: Some("cmd".to_string()),
            detail: Some("Choose a model".to_string()),
            label: "/model".to_string(),
            metadata: Some("Enter".to_string()),
        }],
        selected_index: 0,
        title: "Prompt Suggestion".to_string(),
    };
    let chat_input = ChatInput::new("Prompt", "/", 1).suggestion_list(&suggestion_list);

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw prompt input with dropdown");

    // Assert
    let top_row = buffer_row_text(terminal.backend().buffer(), 0, width);
    assert!(top_row.starts_with("╭"));
    assert!(top_row.contains(" Prompt Suggestion "));
    assert!(top_row.contains("╮"));
}

#[test]
fn test_render_keeps_raw_at_lookup_text_visible_in_input() {
    // Arrange
    let width = 48;
    let backend = ratatui::backend::TestBackend::new(width, 5);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let chat_input = ChatInput::new("Prompt", "@src/main.rs", "@src/main.rs".chars().count());

    // Act
    terminal
        .draw(|frame| {
            let area = frame.area();
            chat_input.render(frame, area);
        })
        .expect("failed to draw prompt input with at-lookup");

    // Assert
    let visible_text = (0..5)
        .map(|row| buffer_row_text(terminal.backend().buffer(), row, width))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible_text.contains("@src/main.rs"));
}
