//! Half-block image diff fitting, sampling, layout, and painting tests.

use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::{DiffImageView, cell_paint, fitted_cell_size, sampled_pixel, source_span};
use crate::presentation::diff_image::{DiffImage, DiffImagePair, DiffImageSide};
use crate::ui::Component;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// Builds an image from row-major RGBA pixels.
fn image(width: u32, height: u32, pixels: Vec<[u8; 4]>) -> DiffImage {
    DiffImage {
        height,
        original_height: height * 10,
        original_width: width * 10,
        pixels: Arc::from(pixels),
        width,
    }
}

/// Builds a solid image of one color.
fn solid_image(width: u32, height: u32, color: [u8; 4]) -> DiffImage {
    let pixel_count = usize::try_from(width * height).expect("fixture size fits usize");

    image(width, height, vec![color; pixel_count])
}

/// Renders one image pair into a buffer of `width` by `height` cells.
fn render_pair(images: &DiffImagePair, width: u16, height: u16) -> Buffer {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("test terminal should build");
    terminal
        .draw(|frame| DiffImageView::new(images).render(frame, frame.area()))
        .expect("test frame should draw");

    terminal.backend().buffer().clone()
}

/// Collects each buffer row as text.
fn buffer_rows(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;

    (area.top()..area.bottom())
        .map(|row| {
            (area.left()..area.right())
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}

/// Counts half-block cells painted within one column range.
fn half_block_count(buffer: &Buffer, columns: std::ops::Range<u16>) -> usize {
    let area = buffer.area;

    (area.top()..area.bottom())
        .flat_map(|row| columns.clone().map(move |column| (column, row)))
        .filter(|position| matches!(buffer[*position].symbol(), "▀" | "▄"))
        .count()
}

#[test]
fn fitted_cell_size_preserves_aspect_ratio_within_bounds() {
    // Arrange
    let wide = solid_image(40, 10, RED);
    let tall = solid_image(10, 40, RED);
    let tiny = solid_image(2, 2, RED);

    // Act
    let wide_fit = fitted_cell_size(&wide, 20, 20);
    let tall_fit = fitted_cell_size(&tall, 20, 10);
    let upscaled_fit = fitted_cell_size(&tiny, 8, 8);
    let empty_fit = fitted_cell_size(&wide, 0, 10);

    // Assert
    assert_eq!(wide_fit, (20, 3));
    assert_eq!(tall_fit, (5, 10));
    assert_eq!(upscaled_fit, (8, 4));
    assert_eq!(empty_fit, (0, 0));
}

#[test]
fn source_span_covers_at_least_one_source_pixel() {
    // Arrange, Act
    let downscaled = source_span(1, 2, 10);
    let upscaled = source_span(3, 8, 2);

    // Assert
    assert_eq!(downscaled, (5, 10));
    assert_eq!(upscaled, (0, 1));
}

#[test]
fn sampled_pixel_averages_opaque_pixels_and_drops_transparent_ones() {
    // Arrange
    let mixed = image(2, 1, vec![RED, BLUE]);
    let mostly_clear = image(2, 1, vec![RED, [0, 0, 255, 1]]);
    let clear = image(2, 1, vec![CLEAR, [255, 255, 255, 100]]);

    // Act
    let averaged = sampled_pixel(&mixed, 0, 0, 1, 1);
    let alpha_weighted = sampled_pixel(&mostly_clear, 0, 0, 1, 1);
    let transparent = sampled_pixel(&clear, 0, 0, 1, 1);

    // Assert
    assert_eq!(averaged, Some(Color::Rgb(127, 0, 127)));
    assert_eq!(alpha_weighted, Some(Color::Rgb(254, 0, 0)));
    assert_eq!(transparent, None);
}

#[test]
fn cell_paint_selects_glyph_and_colors_for_each_pixel_pair() {
    // Arrange
    let red = Color::Rgb(255, 0, 0);
    let blue = Color::Rgb(0, 0, 255);

    // Act
    let both = cell_paint(Some(red), Some(blue));
    let upper = cell_paint(Some(red), None);
    let lower = cell_paint(None, Some(blue));
    let neither = cell_paint(None, None);

    // Assert
    let (both_symbol, both_style) = both.expect("opaque pair should paint");
    assert_eq!(
        (both_symbol, both_style.fg, both_style.bg),
        ("▀", Some(red), Some(blue))
    );
    let (upper_symbol, upper_style) = upper.expect("upper pixel should paint");
    assert_eq!(
        (upper_symbol, upper_style.fg, upper_style.bg),
        ("▀", Some(red), None)
    );
    let (lower_symbol, lower_style) = lower.expect("lower pixel should paint");
    assert_eq!(
        (lower_symbol, lower_style.fg, lower_style.bg),
        ("▄", Some(blue), None)
    );
    assert_eq!(neither, None);
}

#[test]
fn render_places_tall_images_side_by_side_with_labels() {
    // Arrange
    let images = DiffImagePair {
        after: DiffImageSide::Image(solid_image(2, 4, BLUE)),
        before: DiffImageSide::Image(solid_image(2, 4, RED)),
    };

    // Act
    let buffer = render_pair(&images, 42, 6);

    // Assert
    let rows = buffer_rows(&buffer);
    assert!(rows[0].starts_with("Before · 20×40"));
    assert!(rows[0].contains("After · 20×40"));
    assert!(half_block_count(&buffer, 0..20) > 0);
    assert!(half_block_count(&buffer, 22..42) > 0);
    let left_cell = (0..20)
        .flat_map(|column| (1..6).map(move |row| (column, row)))
        .find(|position| buffer[*position].symbol() == "▀")
        .expect("before image should paint");
    assert_eq!(buffer[left_cell].fg, Color::Rgb(255, 0, 0));
    let right_cell = (22..42)
        .flat_map(|column| (1..6).map(move |row| (column, row)))
        .find(|position| buffer[*position].symbol() == "▀")
        .expect("after image should paint");
    assert_eq!(buffer[right_cell].bg, Color::Rgb(0, 0, 255));
}

#[test]
fn render_stacks_wide_images_when_that_draws_more_cells() {
    // Arrange
    let images = DiffImagePair {
        after: DiffImageSide::Image(solid_image(8, 1, BLUE)),
        before: DiffImageSide::Image(solid_image(8, 1, RED)),
    };

    // Act
    let buffer = render_pair(&images, 16, 10);

    // Assert
    let rows = buffer_rows(&buffer);
    assert!(rows[0].starts_with("Before"));
    assert!(rows[5].starts_with("After"));
    assert_eq!(half_block_count(&buffer, 0..16), 32);
}

#[test]
fn render_shows_single_side_for_added_and_deleted_images() {
    // Arrange
    let added = DiffImagePair {
        after: DiffImageSide::Image(solid_image(1, 1, BLUE)),
        before: DiffImageSide::Absent,
    };
    let deleted = DiffImagePair {
        after: DiffImageSide::Absent,
        before: DiffImageSide::Image(image(1, 2, vec![CLEAR, RED])),
    };

    // Act
    let added_buffer = render_pair(&added, 16, 4);
    let deleted_buffer = render_pair(&deleted, 16, 4);

    // Assert
    assert!(buffer_rows(&added_buffer)[0].starts_with("Added · 10×10"));
    assert!(buffer_rows(&deleted_buffer)[0].starts_with("Deleted · 10×20"));
    assert!(half_block_count(&deleted_buffer, 0..16) > 0);
    assert!(
        buffer_rows(&deleted_buffer)
            .iter()
            .any(|row| row.contains('▄'))
    );
}

#[test]
fn render_reports_unavailable_and_missing_sides() {
    // Arrange
    let unavailable = DiffImagePair {
        after: DiffImageSide::Image(solid_image(1, 1, BLUE)),
        before: DiffImageSide::Unavailable("Image too large".to_string()),
    };
    let missing = DiffImagePair {
        after: DiffImageSide::Absent,
        before: DiffImageSide::Absent,
    };
    let transparent = DiffImagePair {
        after: DiffImageSide::Image(solid_image(2, 2, CLEAR)),
        before: DiffImageSide::Absent,
    };

    // Act
    let unavailable_buffer = render_pair(&unavailable, 40, 4);
    let missing_buffer = render_pair(&missing, 24, 2);
    let transparent_buffer = render_pair(&transparent, 8, 4);
    let collapsed_buffer = render_pair(&unavailable, 40, 0);

    // Assert
    let unavailable_rows = buffer_rows(&unavailable_buffer);
    assert!(unavailable_rows[0].starts_with("Before"));
    assert!(unavailable_rows[1].starts_with("Image too large"));
    assert!(buffer_rows(&missing_buffer)[0].starts_with("Image unavailable."));
    assert_eq!(half_block_count(&transparent_buffer, 0..8), 0);
    assert_eq!(collapsed_buffer.area, Rect::new(0, 0, 40, 0));
}
