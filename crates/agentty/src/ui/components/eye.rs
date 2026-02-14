use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::ui::Component;

/// Terminal character aspect ratio: each cell is roughly twice as tall as wide.
const ASPECT_RATIO: f64 = 2.0;

/// Superellipse exponent for the vertical axis, giving an almond/leaf shape.
const EYE_EXPONENT: f64 = 3.0;

/// Iris radius as a fraction of the eye's vertical radius.
const IRIS_FRACTION: f64 = 0.55;

/// Pupil radius as a fraction of the iris radius.
const PUPIL_FRACTION: f64 = 0.45;

/// How far the iris can shift towards the eye edge, as a fraction of
/// the remaining space between iris edge and eye boundary.
const TRACKING_RANGE: f64 = 0.35;

/// Maximum eye width as a fraction of the available area width.
const MAX_WIDTH_FRACTION: f64 = 0.6;

/// Maximum eye height as a fraction of the available area height.
const MAX_HEIGHT_FRACTION: f64 = 0.5;

/// ASCII density glyphs ordered from darkest to brightest, inspired by the
/// amp terminal orb style.
const GLYPHS: &[u8] = b" .:-=+*#%@";

/// Animated eye that tracks cursor position in the prompt input.
pub struct Eye {
    pupil_offset_x: f64,
}

impl Eye {
    /// Creates a new `Eye` with the given horizontal pupil offset.
    ///
    /// `pupil_offset_x` is clamped to `[-1.0, 1.0]` where `-1.0` looks
    /// fully left and `1.0` looks fully right.
    pub fn new(pupil_offset_x: f64) -> Self {
        Self {
            pupil_offset_x: pupil_offset_x.clamp(-1.0, 1.0),
        }
    }
}

impl Component for Eye {
    fn render(&self, frame: &mut Frame, area: Rect) {
        if area.width < 10 || area.height < 5 {
            return;
        }

        let eye = compute_eye_geometry(area, self.pupil_offset_x);
        draw_eye(frame.buffer_mut(), area, &eye);
    }
}

/// Precomputed geometry for a single eye.
struct EyeGeometry {
    center_x: f64,
    center_y: f64,
    half_height: f64,
    half_width: f64,
    iris_center_x: f64,
    iris_center_y: f64,
    iris_radius: f64,
    pupil_radius: f64,
}

/// Computes eye geometry centered in `area` with the iris shifted by
/// `pupil_offset_x` (in `[-1.0, 1.0]`).
fn compute_eye_geometry(area: Rect, pupil_offset_x: f64) -> EyeGeometry {
    let max_half_width = (f64::from(area.width) * MAX_WIDTH_FRACTION) / 2.0;
    let max_half_height = (f64::from(area.height) * MAX_HEIGHT_FRACTION) / 2.0;

    // Scale so the eye fits both constraints, correcting for aspect ratio.
    let half_height = max_half_height.min(max_half_width / ASPECT_RATIO);
    let half_width = half_height * ASPECT_RATIO;

    let center_x = f64::from(area.x) + f64::from(area.width) / 2.0;
    let center_y = f64::from(area.y) + f64::from(area.height) / 2.0;

    let iris_radius = half_height * IRIS_FRACTION;
    let pupil_radius = iris_radius * PUPIL_FRACTION;

    let max_shift = (half_width - iris_radius * ASPECT_RATIO) * TRACKING_RANGE;
    let iris_center_x = center_x + pupil_offset_x * max_shift;
    let iris_center_y = center_y;

    EyeGeometry {
        center_x,
        center_y,
        half_height,
        half_width,
        iris_center_x,
        iris_center_y,
        iris_radius,
        pupil_radius,
    }
}

/// Normalized 2D coordinates relative to the eye's half-axes.
struct NormalizedPoint {
    horizontal: f64,
    vertical: f64,
}

/// Returns `true` when the normalized point lies inside the superellipse
/// outline.
fn inside_eye(point: &NormalizedPoint, exponent: f64) -> bool {
    point.horizontal.abs().powf(2.0) + point.vertical.abs().powf(exponent) < 1.0
}

/// Draws the eye into `buffer` within `area`.
fn draw_eye(buffer: &mut Buffer, area: Rect, eye: &EyeGeometry) {
    for row in area.y..area.y + area.height {
        for col in area.x..area.x + area.width {
            if let Some(cell_style) = compute_cell(eye, col, row) {
                let cell = &mut buffer[(col, row)];
                cell.set_char(char::from(cell_style.glyph));
                cell.set_fg(cell_style.color);
            }
        }
    }
}

/// Visual properties for a single cell in the eye.
struct CellStyle {
    color: Color,
    glyph: u8,
}

/// Maps a brightness `[0.0, 1.0]` to an ASCII density glyph.
fn brightness_to_glyph(brightness: f64) -> u8 {
    let index = (brightness * (GLYPHS.len() - 1) as f64)
        .round()
        .clamp(0.0, (GLYPHS.len() - 1) as f64);

    #[expect(clippy::cast_possible_truncation)]
    let idx = index as usize;

    GLYPHS[idx]
}

/// Computes the glyph and color for a cell, or `None` if outside the eye.
fn compute_cell(eye: &EyeGeometry, col: u16, row: u16) -> Option<CellStyle> {
    let pixel_x = f64::from(col) + 0.5;
    let pixel_y = f64::from(row) + 0.5;

    let normalized = NormalizedPoint {
        horizontal: (pixel_x - eye.center_x) / eye.half_width,
        vertical: (pixel_y - eye.center_y) / eye.half_height,
    };

    if !inside_eye(&normalized, EYE_EXPONENT) {
        return None;
    }

    // How deep inside the eye shape (0 = edge, 1 = center).
    let eye_depth =
        1.0 - (normalized.horizontal.powi(2) + normalized.vertical.abs().powf(EYE_EXPONENT));

    // Distance from iris center (in cell-space, corrected for aspect ratio).
    let offset_horizontal = (pixel_x - eye.iris_center_x) / ASPECT_RATIO;
    let offset_vertical = pixel_y - eye.iris_center_y;
    let iris_dist =
        (offset_horizontal * offset_horizontal + offset_vertical * offset_vertical).sqrt();

    if iris_dist < eye.pupil_radius {
        return Some(pupil_style(eye, offset_horizontal, offset_vertical));
    }

    if iris_dist < eye.iris_radius {
        return Some(iris_style(iris_dist, eye.iris_radius));
    }

    Some(sclera_style(eye_depth))
}

/// Computes style for cells within the pupil (dark center with highlight).
fn pupil_style(eye: &EyeGeometry, offset_horizontal: f64, offset_vertical: f64) -> CellStyle {
    // Highlight: small bright dot in upper-left quadrant of the pupil.
    let shine_x = offset_horizontal + eye.pupil_radius * 0.3;
    let shine_y = offset_vertical + eye.pupil_radius * 0.3;
    let shine_dist = (shine_x * shine_x + shine_y * shine_y).sqrt();

    if shine_dist < eye.pupil_radius * 0.3 {
        return CellStyle {
            color: Color::White,
            glyph: b'@',
        };
    }

    CellStyle {
        color: Color::DarkGray,
        glyph: b'.',
    }
}

/// Computes style for cells within the iris (colored gradient).
fn iris_style(distance: f64, radius: f64) -> CellStyle {
    let ratio = distance / radius;
    let brightness = 1.0 - ratio * 0.4;

    CellStyle {
        color: iris_color(ratio),
        glyph: brightness_to_glyph(brightness),
    }
}

/// Computes style for cells in the sclera (white area with depth-based
/// density).
fn sclera_style(eye_depth: f64) -> CellStyle {
    // Brighter near the center, fading at edges for a rounded look.
    let brightness = eye_depth.clamp(0.0, 1.0);

    CellStyle {
        color: Color::White,
        glyph: brightness_to_glyph(brightness),
    }
}

/// Linearly interpolates a color channel from `base` by
/// `delta * ratio_percent / 100`.
fn lerp_channel(base: u8, delta: i16, ratio_percent: u8) -> u8 {
    let value = i16::from(base) + delta * i16::from(ratio_percent) / 100;

    // After clamping to [0, 255], conversion to u8 is infallible.
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

/// Converts a ratio `[0.0, 1.0]` to a percentage `[0, 100]` as `u8`.
fn ratio_to_percent(ratio: f64) -> u8 {
    let scaled = (ratio * 100.0).round().clamp(0.0, 100.0);

    // Clamped to [0, 100]: fits in both i32 and u8 without loss.
    #[expect(clippy::cast_possible_truncation)]
    let integer = scaled as i32;

    u8::try_from(integer).unwrap_or(100)
}

/// Returns a blue-cyan gradient color for the iris based on distance ratio.
fn iris_color(ratio: f64) -> Color {
    let pct = ratio_to_percent(ratio);
    let red = lerp_channel(30, 40, pct);
    let green = lerp_channel(100, 80, pct);
    let blue = lerp_channel(200, -50, pct);

    Color::Rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inside_eye_center() {
        // Arrange
        let point = NormalizedPoint {
            horizontal: 0.0,
            vertical: 0.0,
        };

        // Act & Assert
        assert!(inside_eye(&point, EYE_EXPONENT));
    }

    #[test]
    fn test_inside_eye_boundary() {
        // Arrange
        let at_right = NormalizedPoint {
            horizontal: 1.0,
            vertical: 0.0,
        };
        let at_top = NormalizedPoint {
            horizontal: 0.0,
            vertical: 1.0,
        };

        // Act & Assert — just outside the boundary
        assert!(!inside_eye(&at_right, EYE_EXPONENT));
        assert!(!inside_eye(&at_top, EYE_EXPONENT));
    }

    #[test]
    fn test_inside_eye_within() {
        // Arrange
        let point = NormalizedPoint {
            horizontal: 0.5,
            vertical: 0.5,
        };

        // Act & Assert
        assert!(inside_eye(&point, EYE_EXPONENT));
    }

    #[test]
    fn test_inside_eye_outside() {
        // Arrange
        let point = NormalizedPoint {
            horizontal: 0.9,
            vertical: 0.9,
        };

        // Act & Assert
        assert!(!inside_eye(&point, EYE_EXPONENT));
    }

    #[test]
    fn test_brightness_to_glyph_dark() {
        // Arrange & Act & Assert — 0.0 maps to space (darkest)
        assert_eq!(brightness_to_glyph(0.0), b' ');
    }

    #[test]
    fn test_brightness_to_glyph_bright() {
        // Arrange & Act & Assert — 1.0 maps to '@' (brightest)
        assert_eq!(brightness_to_glyph(1.0), b'@');
    }

    #[test]
    fn test_brightness_to_glyph_mid() {
        // Arrange & Act
        let glyph = brightness_to_glyph(0.5);

        // Assert — should be somewhere in the middle of the density ramp
        assert!(GLYPHS.contains(&glyph));
        assert_ne!(glyph, b' ');
        assert_ne!(glyph, b'@');
    }

    #[test]
    fn test_iris_color_center() {
        // Arrange & Act
        let color = iris_color(0.0);

        // Assert
        assert_eq!(color, Color::Rgb(30, 100, 200));
    }

    #[test]
    fn test_iris_color_edge() {
        // Arrange & Act
        let color = iris_color(1.0);

        // Assert
        assert_eq!(color, Color::Rgb(70, 180, 150));
    }

    #[test]
    fn test_lerp_channel_zero_ratio() {
        // Arrange & Act & Assert — returns base when ratio is 0
        assert_eq!(lerp_channel(30, 20, 0), 30);
    }

    #[test]
    fn test_lerp_channel_full_ratio() {
        // Arrange & Act & Assert — returns base + delta when ratio is 100
        assert_eq!(lerp_channel(30, 20, 100), 50);
    }

    #[test]
    fn test_lerp_channel_negative_delta() {
        // Arrange & Act & Assert
        assert_eq!(lerp_channel(180, -40, 100), 140);
    }

    #[test]
    fn test_lerp_channel_clamps_to_zero() {
        // Arrange & Act & Assert — base 10 with delta -100 at full ratio
        assert_eq!(lerp_channel(10, -100, 100), 0);
    }

    #[test]
    fn test_ratio_to_percent_zero() {
        // Arrange & Act & Assert
        assert_eq!(ratio_to_percent(0.0), 0);
    }

    #[test]
    fn test_ratio_to_percent_one() {
        // Arrange & Act & Assert
        assert_eq!(ratio_to_percent(1.0), 100);
    }

    #[test]
    fn test_ratio_to_percent_half() {
        // Arrange & Act & Assert
        assert_eq!(ratio_to_percent(0.5), 50);
    }

    #[test]
    fn test_compute_eye_geometry_centered() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let eye = compute_eye_geometry(area, 0.0);

        // Assert
        assert!((eye.center_x - 40.0).abs() < 0.01);
        assert!((eye.center_y - 12.0).abs() < 0.01);
        assert!(eye.iris_radius > 0.0);
        assert!(eye.pupil_radius > 0.0);
        assert!(eye.pupil_radius < eye.iris_radius);
    }

    #[test]
    fn test_compute_eye_geometry_offset_right() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let eye = compute_eye_geometry(area, 1.0);

        // Assert — iris shifted right of center
        assert!(eye.iris_center_x > eye.center_x);
    }

    #[test]
    fn test_compute_eye_geometry_offset_left() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);

        // Act
        let eye = compute_eye_geometry(area, -1.0);

        // Assert — iris shifted left of center
        assert!(eye.iris_center_x < eye.center_x);
    }

    #[test]
    fn test_eye_new_clamps_offset() {
        // Arrange & Act
        let eye_left = Eye::new(-5.0);
        let eye_right = Eye::new(5.0);

        // Assert
        assert!((eye_left.pupil_offset_x - (-1.0)).abs() < f64::EPSILON);
        assert!((eye_right.pupil_offset_x - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_cell_outside_eye() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);
        let eye = compute_eye_geometry(area, 0.0);

        // Act — corner of the area is outside the eye
        let result = compute_cell(&eye, 0, 0);

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_compute_cell_sclera() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);
        let eye = compute_eye_geometry(area, 0.0);

        // Act — far from iris center but inside eye (near the edge)
        let col = f64_to_u16(eye.center_x + eye.half_width * 0.8);
        let row = f64_to_u16(eye.center_y);
        let result = compute_cell(&eye, col, row);

        // Assert — sclera cells are white
        assert!(result.is_some());
        assert_eq!(result.unwrap().color, Color::White);
    }

    #[test]
    fn test_sclera_style_deep() {
        // Arrange & Act
        let style = sclera_style(0.9);

        // Assert — deep inside the eye should be bright
        assert_eq!(style.color, Color::White);
        assert_ne!(style.glyph, b' ');
    }

    #[test]
    fn test_sclera_style_edge() {
        // Arrange & Act
        let style = sclera_style(0.05);

        // Assert — near the edge should be dim
        assert_eq!(style.color, Color::White);
    }

    #[test]
    fn test_pupil_style_center() {
        // Arrange
        let area = Rect::new(0, 0, 80, 24);
        let eye = compute_eye_geometry(area, 0.0);

        // Act
        let style = pupil_style(&eye, 0.0, 0.0);

        // Assert — center of pupil is dark
        assert_eq!(style.color, Color::DarkGray);
    }

    /// Test helper: converts f64 to u16, clamping to u16 range.
    fn f64_to_u16(value: f64) -> u16 {
        u16::try_from(value.round().clamp(0.0, f64::from(u16::MAX)) as i32).unwrap_or(0)
    }
}
