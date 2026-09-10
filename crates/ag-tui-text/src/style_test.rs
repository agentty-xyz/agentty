use ratatui::style::Color;

use crate::style::TextPalette;

#[test]
fn test_default_palette_uses_current_theme_table_surface() {
    // Arrange
    let palette = TextPalette::DEFAULT;

    // Act
    let surface_elevated = palette.surface_elevated;

    // Assert
    assert_eq!(surface_elevated, Color::Black);
}
