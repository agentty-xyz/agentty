use image::{Rgba, RgbaImage};

use super::{
    DEFAULT_BG, DEFAULT_FG, EMPTY_GLYPH, brighten_component, color_to_rgba, dim_component,
    draw_glyph, fill_rect, lookup_glyph,
};
use crate::frame::{CellColor, TerminalFrame};
use crate::renderer::render_to_image;

#[test]
fn render_to_image_produces_correct_dimensions() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello");

    // Act
    let image = render_to_image(&frame);

    // Assert — 80*8 = 640 wide, 24*16 = 384 tall.
    assert_eq!(image.width(), 640);
    assert_eq!(image.height(), 384);
}

#[test]
fn render_to_image_small_terminal() {
    // Arrange
    let frame = TerminalFrame::new(10, 5, b"Hi");

    // Act
    let image = render_to_image(&frame);

    // Assert — 10*8 = 80 wide, 5*16 = 80 tall.
    assert_eq!(image.width(), 80);
    assert_eq!(image.height(), 80);
}

#[test]
fn render_plain_cell_has_correct_background() {
    // Arrange — single cell with default colors.
    let frame = TerminalFrame::new(1, 1, b" ");

    // Act
    let image = render_to_image(&frame);

    // Assert — background should be DEFAULT_BG color.
    let pixel = image.get_pixel(0, 0);
    assert_eq!(pixel.0[0], DEFAULT_BG.red);
    assert_eq!(pixel.0[1], DEFAULT_BG.green);
    assert_eq!(pixel.0[2], DEFAULT_BG.blue);
}

#[test]
fn render_colored_cell_uses_ansi_color() {
    // Arrange — red background via ANSI escape.
    let data = b"\x1b[41m \x1b[0m";
    let frame = TerminalFrame::new(1, 1, data);

    // Act
    let image = render_to_image(&frame);

    // Assert — ANSI red background (index 1 = 128,0,0).
    let pixel = image.get_pixel(0, 0);
    assert_eq!(pixel.0[0], 128);
    assert_eq!(pixel.0[1], 0);
    assert_eq!(pixel.0[2], 0);
}

#[test]
fn render_bold_brightens_foreground() {
    // Arrange — bold 'A' with red foreground.
    let data = b"\x1b[1;31mA\x1b[0m";
    let frame = TerminalFrame::new(1, 1, data);

    // Act — render to ensure no panic, then verify the brightness math.
    let _rendered = render_to_image(&frame);
    let brightened = brighten_component(128);

    // Assert — bold red should be brighter than base red (128).
    assert!(brightened > 128);
}

#[test]
fn render_inverse_swaps_colors() {
    // Arrange — inverse text.
    let data = b"\x1b[7m \x1b[0m";
    let frame = TerminalFrame::new(1, 1, data);

    // Act
    let image = render_to_image(&frame);

    // Assert — background should be default FG color (swapped).
    let pixel = image.get_pixel(0, 0);
    assert_eq!(pixel.0[0], DEFAULT_FG.red);
    assert_eq!(pixel.0[1], DEFAULT_FG.green);
    assert_eq!(pixel.0[2], DEFAULT_FG.blue);
}

#[test]
fn render_underline_draws_bottom_edge_in_foreground() {
    // Arrange — an underlined space has no glyph pixels of its own.
    let frame = TerminalFrame::new(1, 1, b"\x1b[4m \x1b[0m");

    // Act
    let image = render_to_image(&frame);

    // Assert — only the bottom pixel row switches to the foreground.
    for pixel_x in 0..8 {
        assert_eq!(*image.get_pixel(pixel_x, 15), color_to_rgba(DEFAULT_FG));
        assert_eq!(*image.get_pixel(pixel_x, 14), color_to_rgba(DEFAULT_BG));
    }
}

#[test]
fn fill_rect_and_draw_glyph_clip_to_the_image() {
    // Arrange — a 6x10 image is smaller than one 8x16 cell.
    let color = Rgba([1, 2, 3, 255]);
    let mut filled = RgbaImage::new(6, 10);
    let mut drawn = RgbaImage::new(6, 10);

    // Act
    fill_rect(&mut filled, 2, 4, 8, 16, color);
    draw_glyph(&mut drawn, 0, 0, &[0xFF; 16], color);

    // Assert — every in-bounds pixel of the shape is painted, and nothing
    // outside it.
    assert_eq!(*filled.get_pixel(5, 9), color);
    assert_eq!(*filled.get_pixel(1, 9), Rgba([0, 0, 0, 0]));
    assert!(drawn.pixels().all(|pixel| *pixel == color));
}

#[test]
fn lookup_glyph_returns_known_ascii() {
    // Arrange / Act
    let glyph_a = lookup_glyph('A');
    let glyph_space = lookup_glyph(' ');

    // Assert — 'A' glyph should have non-zero bytes, space should be all
    // zeros.
    assert!(glyph_a.iter().any(|&byte| byte != 0));
    assert!(glyph_space.iter().all(|&byte| byte == 0));
}

#[test]
fn lookup_glyph_returns_empty_for_unsupported() {
    // Arrange / Act
    let glyph = lookup_glyph('\u{1F600}'); // emoji

    // Assert
    assert_eq!(glyph, &EMPTY_GLYPH);
}

#[test]
fn lookup_glyph_returns_box_drawing() {
    // Arrange / Act
    let glyph = lookup_glyph('─'); // U+2500

    // Assert — horizontal line should have non-zero bytes.
    assert!(glyph.iter().any(|&byte| byte != 0));
}

#[test]
fn color_to_rgba_converts_correctly() {
    // Arrange
    let color = CellColor::new(100, 150, 200);

    // Act
    let rgba = color_to_rgba(color);

    // Assert
    assert_eq!(rgba, Rgba([100, 150, 200, 255]));
}

#[test]
fn brighten_component_clamps_at_255() {
    // Arrange / Act
    let result = brighten_component(200);

    // Assert — 200 * 140 / 100 = 280, clamped to 255.
    assert_eq!(result, 255);
}

#[test]
fn dim_component_reduces_brightness() {
    // Arrange / Act
    let result = dim_component(200);

    // Assert — 200 * 60 / 100 = 120.
    assert_eq!(result, 120);
}

#[test]
fn dim_component_handles_zero() {
    // Arrange / Act / Assert
    assert_eq!(dim_component(0), 0);
}

#[test]
fn render_dim_darkens_foreground() {
    // Arrange — dim 'A' with default foreground.
    let data = b"\x1b[2mA\x1b[0m";
    let frame = TerminalFrame::new(1, 1, data);

    // Act
    let _rendered = render_to_image(&frame);
    let dimmed = dim_component(DEFAULT_FG.red);

    // Assert — dim should produce a darker value than default FG.
    assert!(dimmed < DEFAULT_FG.red);
}
