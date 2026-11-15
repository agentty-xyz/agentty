use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::{
    NERPA_PIXELS, NerpaGreeting, SHEEN_PERIOD_MS, SHEEN_RADIUS, SHEEN_REST_MS, SHEEN_TRAVEL, Tone,
};
use crate::ui::{Component, style};

/// Timestamp at the start of a sheen cycle, when the art rests.
const REST_MILLIS: u128 = 1_782_864_000_000;

/// Area one column wider and two rows taller than the art and its padding,
/// which places the art's top-left cell at column `1`, row `2`.
const ROOMY_AREA: Rect = Rect::new(0, 0, 34, 9);

/// Collects each buffer row of `area` as trimmed-right text.
fn area_rows(buffer: &Buffer, area: Rect) -> Vec<String> {
    (area.top()..area.bottom())
        .map(|row_y| {
            (area.left()..area.right())
                .map(|column| buffer[(column, row_y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Returns the glyph and foreground painted at one position.
fn painted(buffer: &Buffer, column: u16, row: u16) -> (&str, Color) {
    let cell = &buffer[(column, row)];

    (cell.symbol(), cell.fg)
}

#[test]
fn test_pixels_form_a_rectangular_grid_of_known_tones() {
    // Arrange
    let width = NERPA_PIXELS[0][0].len();

    // Act
    let is_rectangular = NERPA_PIXELS
        .iter()
        .flatten()
        .all(|pixels| pixels.len() == width);
    let only_known_pixels = NERPA_PIXELS
        .iter()
        .flatten()
        .flat_map(|pixels| pixels.bytes())
        .all(|pixel| pixel == b'.' || Tone::from_pixel(pixel).is_some());

    // Assert
    assert!(is_rectangular);
    assert!(only_known_pixels);
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
    let rows = area_rows(buffer, ROOMY_AREA);
    assert!(rows[..2].iter().all(String::is_empty));
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
fn test_sheen_rests_then_sweeps_across_every_diagonal() {
    // Arrange
    let sweep_start = REST_MILLIS + SHEEN_REST_MS;
    let sweep_end = REST_MILLIS + SHEEN_PERIOD_MS - 1;

    // Act
    let resting = NerpaGreeting::sheen_center(REST_MILLIS + SHEEN_REST_MS - 1);
    let entering = NerpaGreeting::sheen_center(sweep_start);
    let leaving = NerpaGreeting::sheen_center(sweep_end);

    // Assert
    assert_eq!(resting, None);
    assert_eq!(entering, Some(0));
    assert_eq!(leaving, Some(SHEEN_TRAVEL - 1));
}

#[test]
fn test_lit_tone_brightens_only_cells_inside_the_band() {
    // Arrange
    let center = Some(20);

    // Act
    let at_center = NerpaGreeting::lit_tone(Tone::Body, 20, center);
    let inside_edge = NerpaGreeting::lit_tone(Tone::Body, 20 + SHEEN_RADIUS - 1, center);
    let outside_edge = NerpaGreeting::lit_tone(Tone::Body, 20 + SHEEN_RADIUS, center);
    let resting = NerpaGreeting::lit_tone(Tone::Body, 20, None);

    // Assert
    assert_eq!(at_center, Tone::Highlight);
    assert_eq!(inside_edge, Tone::Highlight);
    assert_eq!(outside_edge, Tone::Body);
    assert_eq!(resting, Tone::Body);
}

#[test]
fn test_tone_lit_steps_one_tone_brighter() {
    // Arrange
    let tones = [Tone::Ice, Tone::Body, Tone::Highlight];

    // Act
    let lit_tones = tones.map(Tone::lit);

    // Assert
    assert_eq!(lit_tones, [Tone::Body, Tone::Highlight, Tone::Highlight]);
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
fn test_render_mid_sweep_recolors_cells_under_the_band_without_changing_glyphs() {
    // Arrange
    let resting = NerpaGreeting::new(REST_MILLIS);
    let sweeping = NerpaGreeting::new(REST_MILLIS + SHEEN_REST_MS + 750);
    let mut resting_buffer = Buffer::empty(ROOMY_AREA);
    let mut sweeping_buffer = Buffer::empty(ROOMY_AREA);
    let body = style::palette::text_muted();
    let highlight = style::palette::text();
    let ice = style::palette::accent_soft();

    // Act
    resting.paint(&mut resting_buffer, ROOMY_AREA);
    sweeping.paint(&mut sweeping_buffer, ROOMY_AREA);

    // Assert
    assert_eq!(
        area_rows(&resting_buffer, ROOMY_AREA),
        area_rows(&sweeping_buffer, ROOMY_AREA)
    );
    assert_eq!(painted(&resting_buffer, 17, 5), ("█", body));
    assert_eq!(painted(&sweeping_buffer, 17, 5), ("█", highlight));
    assert_eq!(painted(&sweeping_buffer, 18, 5), ("█", body));
    assert_eq!(painted(&resting_buffer, 11, 7), ("█", body));
    assert_eq!(painted(&sweeping_buffer, 11, 7), ("█", highlight));
    assert_eq!(painted(&resting_buffer, 9, 8), ("█", ice));
    assert_eq!(painted(&sweeping_buffer, 9, 8), ("█", body));
}

#[test]
fn test_render_sheen_brightens_body_under_right_whisker() {
    // Arrange
    let sweeping = NerpaGreeting::new(REST_MILLIS + SHEEN_REST_MS + 830);
    let mut buffer = Buffer::empty(ROOMY_AREA);
    let body = style::palette::text_muted();
    let highlight = style::palette::text();

    // Act
    sweeping.paint(&mut buffer, ROOMY_AREA);

    // Assert
    let lit_cell = &buffer[(17, 6)];
    let unlit_cell = &buffer[(17, 7)];
    assert_eq!(
        (lit_cell.symbol(), lit_cell.fg, lit_cell.bg),
        ("▀", highlight, highlight)
    );
    assert_eq!(
        (unlit_cell.symbol(), unlit_cell.fg, unlit_cell.bg),
        ("▀", highlight, body)
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
