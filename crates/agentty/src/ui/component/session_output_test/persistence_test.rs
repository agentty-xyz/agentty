use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::icon::TACHYON_LOADER_WIDTH;
use crate::ui::style;

#[test]
fn test_tachyon_loader_effect_emphasizes_loader_cells() {
    // Arrange
    let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
    let mut buffer = Buffer::empty(area);
    for column in 0..TACHYON_LOADER_WIDTH {
        buffer[(column, 0)]
            .set_symbol("▌")
            .set_fg(style::palette::text_muted());
    }

    // Act
    let mut loader_effect = TachyonLoaderEffect::new();
    loader_effect.apply(&mut buffer, area, 4);

    // Assert
    let foreground_colors = (0..TACHYON_LOADER_WIDTH)
        .map(|column| buffer[(column, 0)].fg)
        .collect::<Vec<_>>();
    assert!(foreground_colors.contains(&style::palette::warning()));
    assert!(foreground_colors.contains(&style::palette::warning_soft()));
}
