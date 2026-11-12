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
fn test_apply_stateless_late_frame_emphasizes_last_cell_and_skips_blanks() {
    // Arrange
    let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH + 1, 1);
    let mut buffer = Buffer::empty(area);
    for column in 0..TACHYON_LOADER_WIDTH {
        buffer[(column, 0)].set_symbol("▌");
    }

    // Act
    TachyonLoaderEffect::apply_stateless(&mut buffer, area, 7);

    // Assert
    assert_eq!(active_loader_column(&buffer), Some(2));
    assert_eq!(
        buffer[(TACHYON_LOADER_WIDTH, 0)].fg,
        ratatui::style::Color::Reset
    );
}

#[test]
fn test_apply_advances_by_frame_deltas_and_restarts_each_cycle() {
    // Arrange
    let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
    let mut buffer = Buffer::empty(area);
    for column in 0..TACHYON_LOADER_WIDTH {
        buffer[(column, 0)].set_symbol("▌");
    }
    let mut effect = TachyonLoaderEffect::new();

    // Act
    let active_columns = [0, 0, 7, 10]
        .into_iter()
        .map(|spinner_frame| {
            effect.apply(&mut buffer, area, spinner_frame);

            active_loader_column(&buffer)
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(active_columns, vec![Some(0), Some(0), Some(2), Some(0)]);
}

#[test]
fn test_apply_to_last_glyph_returns_none_without_a_loader() {
    // Arrange
    let narrow_area = Rect::new(0, 0, TACHYON_LOADER_WIDTH - 1, 1);
    let blank_area = Rect::new(0, 0, 8, 2);
    let mut narrow_buffer = Buffer::empty(narrow_area);
    let mut blank_buffer = Buffer::empty(blank_area);

    // Act
    let narrow_result =
        TachyonLoaderEffect::apply_to_last_glyph(&mut narrow_buffer, narrow_area, 0);
    let blank_result = TachyonLoaderEffect::apply_to_last_glyph(&mut blank_buffer, blank_area, 0);

    // Assert
    assert_eq!(narrow_result, None);
    assert_eq!(blank_result, None);
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

/// Returns the loader column currently painted with the active warning color.
fn active_loader_column(buffer: &Buffer) -> Option<u16> {
    (0..TACHYON_LOADER_WIDTH).find(|column| buffer[(*column, 0)].fg == style::palette::warning())
}
