use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::TachyonLoaderEffect;
use crate::ui::icon::TACHYON_LOADER_WIDTH;
use crate::ui::style;

#[test]
fn test_apply_stateless_emphasizes_loader_cells() {
    // Arrange
    let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
    let mut buffer = Buffer::empty(area);
    for column in 0..TACHYON_LOADER_WIDTH {
        buffer[(column, 0)].set_symbol("▌");
    }

    // Act
    TachyonLoaderEffect::apply_stateless(&mut buffer, area, 4);

    // Assert
    let foreground_colors = (0..TACHYON_LOADER_WIDTH)
        .map(|column| buffer[(column, 0)].fg)
        .collect::<Vec<_>>();
    assert!(foreground_colors.contains(&style::palette::warning()));
    assert!(foreground_colors.contains(&style::palette::warning_soft()));
}

#[test]
fn test_apply_to_last_glyph_targets_bottom_most_loader() {
    // Arrange
    let area = Rect::new(0, 0, 8, 3);
    let mut buffer = Buffer::empty(area);
    for column in 0..TACHYON_LOADER_WIDTH {
        buffer[(column, 0)].set_symbol("▌");
        buffer[(column + 3, 2)].set_symbol("▌");
    }

    // Act
    let loader_area = TachyonLoaderEffect::apply_to_last_glyph(&mut buffer, area, 0);

    // Assert
    assert_eq!(loader_area, Some(Rect::new(3, 2, TACHYON_LOADER_WIDTH, 1)));
    assert_eq!(buffer[(0, 0)].fg, ratatui::style::Color::Reset);
    assert_eq!(buffer[(3, 2)].fg, style::palette::warning());
}
