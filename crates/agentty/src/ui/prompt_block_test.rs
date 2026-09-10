use ratatui::style::Color;

use super::{user_prompt_content_style, user_prompt_prefix_style};
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
