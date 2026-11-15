//! Nerpa greeting pixel art, sheen animation, colorless fallback, and
//! placement unit tests.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::{
    ART_WIDTH, NERPA_PIXELS, NerpaGreeting, TAIL_COLUMN, TAIL_PERIOD_MS, TAIL_POSES, TAIL_REST_MS,
    TAIL_STEP_MS, TAIL_WAG, Tone,
};
use crate::ui::{Component, style};

/// Timestamp at the start of a tail cycle, when the art rests.
const REST_MILLIS: u128 = 1_782_864_000_000;

/// Area one column wider and two rows taller than the art and its padding,
/// which places the art's top-left cell at column `1`, row `2`.
const ROOMY_AREA: Rect = Rect::new(0, 0, 34, 9);

/// Strip of [`ROOMY_AREA`] painted by the tail poses.
const TAIL_AREA: Rect = Rect::new(27, 2, 6, 6);

/// Collects each buffer row of `area` as untrimmed text.
fn area_text(buffer: &Buffer, area: Rect) -> Vec<String> {
    (area.top()..area.bottom())
        .map(|row_y| {
            (area.left()..area.right())
                .map(|column| buffer[(column, row_y)].symbol())
                .collect()
        })
        .collect()
}

/// Paints the colored greeting for `unix_millis` into an empty
/// [`ROOMY_AREA`] buffer.
fn painted_buffer(unix_millis: u128) -> Buffer {
    let mut buffer = Buffer::empty(ROOMY_AREA);
    NerpaGreeting::new(unix_millis).paint(&mut buffer, ROOMY_AREA);

    buffer
}

/// Returns the glyph and foreground painted at one position.
fn painted(buffer: &Buffer, column: u16, row: u16) -> (&str, Color) {
    let cell = &buffer[(column, row)];

    (cell.symbol(), cell.fg)
}

#[test]
fn test_pixels_form_a_rectangular_grid_of_known_tones() {
    // Arrange
    let tail_width = ART_WIDTH - TAIL_COLUMN;
    let tail_pixels = || TAIL_POSES.iter().flat_map(|pose| pose.split_whitespace());
    let tail_pixel_rows = usize::from(TAIL_AREA.height) * 2;

    // Act
    let is_rectangular = NERPA_PIXELS
        .iter()
        .flatten()
        .all(|pixels| pixels.len() == ART_WIDTH);
    let tails_are_rectangular = tail_pixels().all(|pixels| pixels.len() == tail_width)
        && TAIL_POSES
            .iter()
            .all(|pose| pose.split_whitespace().count() == tail_pixel_rows);
    let only_known_pixels = NERPA_PIXELS
        .iter()
        .flatten()
        .flat_map(|pixels| pixels.bytes())
        .all(|pixel| pixel == b'.' || Tone::from_pixel(pixel).is_some());
    let tails_hold_only_body = tail_pixels()
        .flat_map(|pixels| pixels.bytes())
        .all(|pixel| pixel == b'.' || pixel == b'B');

    // Assert
    assert!(is_rectangular);
    assert!(tails_are_rectangular);
    assert!(only_known_pixels);
    assert!(tails_hold_only_body);
}

#[test]
fn test_body_leaves_the_tail_columns_to_the_tail_poses() {
    // Arrange
    let tail_rows = &NERPA_PIXELS[..usize::from(TAIL_AREA.height)];

    // Act
    let tail_columns_are_empty = tail_rows
        .iter()
        .flatten()
        .all(|pixels| pixels[TAIL_COLUMN..].bytes().all(|pixel| pixel == b'.'));

    // Assert
    assert!(tail_columns_are_empty);
}

#[test]
fn test_only_right_whisker_cells_mix_two_tones() {
    // Arrange
    let pixel_pairs = NERPA_PIXELS
        .iter()
        .flat_map(|[upper_pixels, lower_pixels]| upper_pixels.bytes().zip(lower_pixels.bytes()));

    // Act
    let mixed_cells: Vec<(u8, u8)> = pixel_pairs
        .filter(|(upper_pixel, lower_pixel)| {
            *upper_pixel != b'.' && *lower_pixel != b'.' && upper_pixel != lower_pixel
        })
        .collect();

    // Assert
    assert_eq!(mixed_cells, [(b'H', b'B'); 6]);
}

#[test]
fn test_render_anchors_resting_art_to_bottom_right_corner() {
    // Arrange
    let mut terminal = Terminal::new(TestBackend::new(34, 9)).expect("terminal");
    let body = style::palette::text_muted();
    let highlight = style::palette::text();
    let ice = style::palette::accent_soft();
    let right_whisker_cells = [(16, 6), (17, 6), (18, 6), (16, 7), (17, 7), (18, 7)];

    // Act
    terminal
        .draw(|frame| NerpaGreeting::new(REST_MILLIS).render(frame, ROOMY_AREA))
        .expect("draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let rows = area_text(buffer, ROOMY_AREA);
    assert!(rows[..2].iter().all(|row| row.trim().is_empty()));
    assert_eq!(rows[2].trim(), "▄▄▄▄▄▄▄▄");
    assert!(rows[3].contains("▄████████████▄"));
    assert_eq!(painted(buffer, 7, 2), ("▄", body));
    assert_eq!(painted(buffer, 4, 3), ("▄", body));
    assert_eq!(painted(buffer, 5, 3), ("█", body));
    assert_eq!(painted(buffer, 6, 4), (" ", Color::Reset));
    assert_eq!(painted(buffer, 7, 4), ("▀", highlight));
    assert_eq!(painted(buffer, 31, 4), ("▀", body));
    assert_eq!(painted(buffer, 6, 5), ("█", highlight));
    assert_eq!(painted(buffer, 9, 5), ("▀", highlight));
    assert_eq!(painted(buffer, 1, 6), ("▀", highlight));
    assert_eq!(painted(buffer, 15, 6), ("█", body));
    assert_eq!(painted(buffer, 19, 6), ("█", body));
    assert_eq!(painted(buffer, 31, 6), ("▄", body));
    assert_eq!(painted(buffer, 1, 7), ("▀", highlight));
    assert_eq!(painted(buffer, 4, 7), (" ", Color::Reset));
    assert_eq!(painted(buffer, 5, 7), ("█", body));
    assert_eq!(painted(buffer, 26, 7), ("▀", body));
    assert_eq!(painted(buffer, 1, 8), ("▀", ice));
    assert_eq!(painted(buffer, 4, 8), ("█", ice));
    assert_eq!(painted(buffer, 32, 8), ("▀", ice));
    assert_eq!(painted(buffer, 33, 8), (" ", Color::Reset));
    for position in ROOMY_AREA.positions() {
        let cell = &buffer[position];
        if right_whisker_cells.contains(&(position.x, position.y)) {
            assert_eq!((cell.symbol(), cell.fg, cell.bg), ("▀", highlight, body));
        } else {
            assert_eq!(cell.bg, Color::Reset);
        }
    }
}

#[test]
fn test_render_without_colors_merges_right_whiskers_into_body() {
    // Arrange
    let mut colored_buffer = Buffer::empty(ROOMY_AREA);
    let mut colorless_buffer = Buffer::empty(ROOMY_AREA);
    let body = style::palette::text_muted();
    let highlight = style::palette::text();
    let right_whisker_cells = [(16, 6), (17, 6), (18, 6), (16, 7), (17, 7), (18, 7)];

    // Act
    NerpaGreeting::new(REST_MILLIS).paint(&mut colored_buffer, ROOMY_AREA);
    NerpaGreeting::new(REST_MILLIS)
        .color_support(style::ColorSupport::Disabled)
        .paint(&mut colorless_buffer, ROOMY_AREA);

    // Assert
    assert_eq!(painted(&colorless_buffer, 1, 6), ("▀", highlight));
    for position in ROOMY_AREA.positions() {
        let colored_cell = &colored_buffer[position];
        let colorless_cell = &colorless_buffer[position];
        if right_whisker_cells.contains(&(position.x, position.y)) {
            assert_eq!(
                (
                    colorless_cell.symbol(),
                    colorless_cell.fg,
                    colorless_cell.bg
                ),
                ("█", body, Color::Reset)
            );
        } else {
            assert_eq!(colorless_cell, colored_cell);
        }
    }
}

#[test]
fn test_wag_fills_the_cycle_after_the_rest() {
    // Arrange
    let wag_ms = TAIL_STEP_MS * u128::try_from(TAIL_WAG.len()).expect("wag length");

    // Act
    let cycle_ms = TAIL_REST_MS + wag_ms;

    // Assert
    assert_eq!(cycle_ms, TAIL_PERIOD_MS);
}

#[test]
fn test_tail_rests_then_wags_through_the_pose_sequence() {
    // Arrange
    let wag_start = REST_MILLIS + TAIL_REST_MS;

    // Act
    let resting = NerpaGreeting::wag_pose(wag_start - 1);
    let wag_poses: Vec<Option<usize>> = (0..TAIL_PERIOD_MS - TAIL_REST_MS)
        .step_by(usize::try_from(TAIL_STEP_MS).expect("step"))
        .map(|offset| NerpaGreeting::wag_pose(wag_start + offset))
        .collect();
    let wag_end = NerpaGreeting::wag_pose(REST_MILLIS + TAIL_PERIOD_MS - 1);
    let next_rest = NerpaGreeting::wag_pose(REST_MILLIS + TAIL_PERIOD_MS);

    // Assert
    assert_eq!(resting, None);
    assert_eq!(wag_poses, TAIL_WAG.map(Some));
    assert_eq!(wag_end, Some(0));
    assert_eq!(next_rest, None);
    assert!(!NerpaGreeting::is_wagging(REST_MILLIS));
    assert!(NerpaGreeting::is_wagging(wag_start));
}

#[test]
fn test_render_mid_wag_moves_only_the_hind_flippers() {
    // Arrange
    let wag_start = REST_MILLIS + TAIL_REST_MS;
    let resting_buffer = painted_buffer(REST_MILLIS);

    // Act
    let tip_up_buffer = painted_buffer(wag_start);
    let raised_buffer = painted_buffer(wag_start + TAIL_STEP_MS);
    let lowered_buffer = painted_buffer(wag_start + 4 * TAIL_STEP_MS);

    // Assert
    assert_eq!(
        area_text(&resting_buffer, TAIL_AREA),
        ["      ", "   ▄▄ ", " ▄██▀ ", "███   ", "█▀██▄ ", "   ▀▀ "]
    );
    assert_eq!(
        area_text(&tip_up_buffer, TAIL_AREA),
        ["      ", "   ██ ", " ▄█▀  ", "███▄  ", "█▀███ ", "      "]
    );
    assert_eq!(
        area_text(&raised_buffer, TAIL_AREA),
        ["   ▄▄ ", "  ▄█▀ ", " ▄█▀  ", "████▄ ", "█▀▀▀▀ ", "      "]
    );
    assert_eq!(
        area_text(&lowered_buffer, TAIL_AREA),
        ["      ", "      ", " ▄███ ", "███▀  ", "█▀█▄  ", "   ██ "]
    );
    for wag_buffer in [&tip_up_buffer, &raised_buffer, &lowered_buffer] {
        for position in ROOMY_AREA.positions() {
            if !TAIL_AREA.contains(position) {
                assert_eq!(wag_buffer[position], resting_buffer[position]);
            }
        }
    }
    assert_eq!(
        painted(&raised_buffer, 30, 2),
        ("▄", style::palette::text_muted())
    );
}

#[test]
fn test_tone_color_maps_onto_theme_palette() {
    // Arrange
    let tones = [Tone::Ice, Tone::Body, Tone::Highlight];

    // Act
    let colors = tones.map(Tone::color);

    // Assert
    assert_eq!(
        colors,
        [
            style::palette::accent_soft(),
            style::palette::text_muted(),
            style::palette::text(),
        ]
    );
}

#[test]
fn test_paint_skips_area_that_is_too_small() {
    // Arrange
    let narrow_area = Rect::new(0, 0, 32, 7);
    let short_area = Rect::new(0, 0, 33, 6);
    let mut narrow_buffer = Buffer::empty(narrow_area);
    let mut short_buffer = Buffer::empty(short_area);

    // Act
    NerpaGreeting::new(REST_MILLIS).paint(&mut narrow_buffer, narrow_area);
    NerpaGreeting::new(REST_MILLIS).paint(&mut short_buffer, short_area);

    // Assert
    assert_eq!(narrow_buffer, Buffer::empty(narrow_area));
    assert_eq!(short_buffer, Buffer::empty(short_area));
}

#[test]
fn test_paint_skips_when_area_holds_content_outside_art() {
    // Arrange
    let mut buffer = Buffer::empty(ROOMY_AREA);
    buffer[(0, 0)].set_char('x');
    let expected_buffer = buffer.clone();

    // Act
    NerpaGreeting::new(REST_MILLIS).paint(&mut buffer, ROOMY_AREA);

    // Assert
    assert_eq!(buffer, expected_buffer);
}
