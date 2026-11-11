use ratatui::style::Color;
use ratatui::text::Span;

use super::{
    USER_PROMPT_PREFIX, user_prompt_content_style, user_prompt_markdown_line,
    user_prompt_prefix_style,
};
use crate::domain::theme::ColorTheme;
use crate::ui::style;

#[test]
fn test_user_prompt_styles_use_prompt_surface_background() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);

    // Act
    let prefix_style = user_prompt_prefix_style();
    let content_style = user_prompt_content_style();

    // Assert
    assert_eq!(prefix_style.bg, Some(style::palette::surface_prompt()));
    assert_eq!(content_style.bg, Some(style::palette::surface_prompt()));
}

#[test]
fn test_user_prompt_markdown_line_adds_no_padding_when_content_fills_width() {
    // Arrange
    let rendered_spans = vec![Span::raw("fits")];
    let prefix_style = user_prompt_prefix_style();

    // Act
    let line = user_prompt_markdown_line(rendered_spans, USER_PROMPT_PREFIX, prefix_style, 7);

    // Assert
    assert_eq!(line.to_string(), " › fits");
    assert_eq!(line.spans.len(), 2);
    assert_eq!(line.spans[1].style, user_prompt_content_style());
}

#[test]
fn test_current_theme_prompt_surface_is_terminal_scheme_independent() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);

    // Act
    let prompt_surface = style::palette::surface_prompt();

    // Assert
    assert!(
        matches!(prompt_surface, Color::Rgb(..)),
        "prompt blocks must pin an RGB surface so terminal ANSI schemes cannot remap it to a \
         light color",
    );
}
