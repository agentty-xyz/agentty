use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::ui::{Component, style};

/// Pixel map of a Baikal seal resting on an ice floe, facing left.
///
/// Each entry is one terminal row holding an upper and a lower row of square
/// pixels. `.` is empty, `B` the body, `H` the muzzle, whiskers, and eye
/// catchlights, and `I` the ice. The eyes and nose are empty pixels.
///
/// Lit pixels sharing a cell share one tone, so the cell needs a foreground
/// color only. The right whiskers are the exception: they lie over the body,
/// so each of their cells pairs a highlight pixel with a body pixel. Only
/// color tells those two pixels apart, so the right whiskers merge into the
/// body when the terminal renders without colors.
const NERPA_PIXELS: [[&str; 2]; 7] = [
    [
        "................................",
        "......BBBBBBBB..................",
    ],
    [
        "....BBBBBBBBBBBB................",
        "...BBBBBBBBBBBBBB............BB.",
    ],
    [
        "..BBB.HBBBBB.HBBBBBBBB......BBB.",
        "..BBB..BBBBB..BBBBBBBBBBB..BBB..",
    ],
    [
        "..BBBHHHHHHHHBBBBBBBBBBBBBBBB...",
        "..BBBHHH..HHHBBBBBBBBBBBBBBBB...",
    ],
    [
        "HHHBHHHHHHHHHHBHHHBBBBBBBBBBBB..",
        "...BHHHHHHHHHHBBBBBBBBBBBBB.BBB.",
    ],
    [
        "HHH.BBBBBBBBBBBHHHBBBBBBBB...BB.",
        "....BBBBBBBBBBBBBBBBBBBBB.......",
    ],
    [
        "IIIIIIIIIIIIIIIIIIIIIIIIIIIIIIII",
        "...IIIIIII....IIIIIIII...IIIII..",
    ],
];

/// Glyph for a cell whose upper and lower pixels are both lit.
const FULL_BLOCK: char = '█';

/// Glyph for a cell whose upper pixel alone is lit.
const UPPER_HALF_BLOCK: char = '▀';

/// Glyph for a cell whose lower pixel alone is lit.
const LOWER_HALF_BLOCK: char = '▄';

/// Columns kept free between the art and the right edge of its area.
const RIGHT_PADDING: u16 = 1;

/// Full sheen cycle: the art rests, then one light band sweeps across it.
const SHEEN_PERIOD_MS: u128 = 5_000;

/// Portion of [`SHEEN_PERIOD_MS`], from the cycle start, with no sheen.
///
/// Starting each cycle at rest keeps captures taken at whole-period
/// timestamps, such as pinned-clock feature recordings, on the plain art.
const SHEEN_REST_MS: u128 = 3_500;

/// Half-width, in diagonal cells, of the sweeping light band.
const SHEEN_RADIUS: usize = 2;

/// Each terminal row shifts the band this many columns, which makes it
/// travel at about 45 degrees because cells are twice as tall as wide.
const SHEEN_ROW_SLANT: usize = 2;

/// Diagonal distance, in cells, the band center travels during one sweep.
///
/// Diagonals are offset by [`SHEEN_RADIUS`], so the band starts just before
/// the first cell and leaves the last one as the sweep ends.
const SHEEN_TRAVEL: usize =
    (NERPA_PIXELS[0][0].len() - 1) + (NERPA_PIXELS.len() - 1) * SHEEN_ROW_SLANT + 2 * SHEEN_RADIUS;

/// Pixel-art nerpa that greets a session before its first message.
///
/// Half-block glyphs give every terminal cell two square pixels, each filled
/// with a flat theme tone. Every few seconds a diagonal sheen sweeps across
/// the seal and brightens the cells it passes by one tone. The frame derives
/// only from `unix_millis`, so equal timestamps always paint identical
/// frames, and the sheen changes colors only, never glyphs.
///
/// A terminal without colors gets the same silhouette minus the right
/// whiskers, which only a second tone can draw over the body.
///
/// The art anchors to the bottom-right corner of the provided area and is
/// skipped when the area is too small or already holds any content, such as
/// a saved-prompt notice, so it only decorates an empty transcript.
pub struct NerpaGreeting {
    color_support: style::ColorSupport,
    unix_millis: u128,
}

impl NerpaGreeting {
    /// Creates a colored greeting that paints the animation frame for
    /// `unix_millis`.
    pub fn new(unix_millis: u128) -> Self {
        Self {
            color_support: style::ColorSupport::Enabled,
            unix_millis,
        }
    }

    /// Sets whether the terminal renders colors.
    ///
    /// Without colors a two-tone cell cannot show its second tone, so it is
    /// painted as one solid block instead.
    #[must_use]
    pub fn color_support(mut self, color_support: style::ColorSupport) -> Self {
        self.color_support = color_support;

        self
    }

    /// Returns whether the sheen is sweeping at `unix_millis`.
    ///
    /// Frames only change while the sheen sweeps, so callers can redraw at
    /// frame rate during the sweep and fall back to a slower cadence while
    /// the art rests.
    pub fn is_sweeping(unix_millis: u128) -> bool {
        Self::sheen_center(unix_millis).is_some()
    }

    /// Renders the nerpa like [`Component::render`] and returns whether the
    /// painted art can animate.
    ///
    /// The sheen changes colors only, so the art animates only when it fits a
    /// blank `area` and the terminal renders colors. Callers use the result
    /// to skip frame-rate redraws while the art is hidden or colorless.
    pub fn render_animated(&self, f: &mut Frame, area: Rect) -> bool {
        self.paint(f.buffer_mut(), area) && self.color_support == style::ColorSupport::Enabled
    }

    /// Paints the nerpa into `buffer` when it fits inside a blank `area`,
    /// returning whether it was painted.
    fn paint(&self, buffer: &mut Buffer, area: Rect) -> bool {
        let Some(art_area) = Self::art_area(area) else {
            return false;
        };
        if !Self::is_blank(buffer, area) {
            return false;
        }

        let sheen_center = Self::sheen_center(self.unix_millis);
        for ((row, [upper_pixels, lower_pixels]), row_y) in
            NERPA_PIXELS.iter().enumerate().zip(art_area.y..)
        {
            let pixel_pairs = upper_pixels.bytes().zip(lower_pixels.bytes());
            for ((column, (upper_pixel, lower_pixel)), column_x) in
                pixel_pairs.enumerate().zip(art_area.x..)
            {
                let Some((glyph, foreground, background)) =
                    Self::cell_art(upper_pixel, lower_pixel, self.color_support)
                else {
                    continue;
                };

                let diagonal = column + row * SHEEN_ROW_SLANT + SHEEN_RADIUS;
                let cell = &mut buffer[(column_x, row_y)];
                cell.set_char(glyph)
                    .set_fg(Self::lit_tone(foreground, diagonal, sheen_center).color());
                if let Some(background) = background {
                    cell.set_bg(Self::lit_tone(background, diagonal, sheen_center).color());
                }
            }
        }

        true
    }

    /// Returns the bottom-right rectangle occupied by the art, or `None` when
    /// it does not fit inside `area`.
    fn art_area(area: Rect) -> Option<Rect> {
        let width = u16::try_from(NERPA_PIXELS[0][0].len()).ok()?;
        let height = u16::try_from(NERPA_PIXELS.len()).ok()?;
        let required_width = width.checked_add(RIGHT_PADDING)?;
        if area.width < required_width || area.height < height {
            return None;
        }

        Some(Rect::new(
            area.right() - required_width,
            area.bottom() - height,
            width,
            height,
        ))
    }

    /// Returns whether every cell in `area` is still an empty space.
    fn is_blank(buffer: &Buffer, area: Rect) -> bool {
        area.positions()
            .all(|position| buffer[position].symbol() == " ")
    }

    /// Returns the diagonal position of the sheen band at `unix_millis`, or
    /// `None` while the cycle rests.
    ///
    /// During the sweep the center moves linearly from `0` to
    /// [`SHEEN_TRAVEL`] diagonal cells.
    fn sheen_center(unix_millis: u128) -> Option<usize> {
        let sweep_elapsed_ms = (unix_millis % SHEEN_PERIOD_MS).checked_sub(SHEEN_REST_MS)?;
        let sweep_ms = SHEEN_PERIOD_MS - SHEEN_REST_MS;
        let travel = u128::try_from(SHEEN_TRAVEL).ok()?;

        usize::try_from(sweep_elapsed_ms * travel / sweep_ms).ok()
    }

    /// Returns the glyph, foreground tone, and optional background tone of
    /// the cell made of `upper_pixel` and `lower_pixel`, or `None` when both
    /// pixels are empty.
    ///
    /// Only a cell whose two lit pixels differ in tone needs a background:
    /// its upper pixel is the foreground of an upper half block and its
    /// lower pixel shows through as the background. Without colors the
    /// background pixel would vanish, so the cell becomes a full block in the
    /// lower pixel's tone.
    fn cell_art(
        upper_pixel: u8,
        lower_pixel: u8,
        color_support: style::ColorSupport,
    ) -> Option<(char, Tone, Option<Tone>)> {
        match (Tone::from_pixel(upper_pixel), Tone::from_pixel(lower_pixel)) {
            (None, None) => None,
            (Some(upper_tone), Some(lower_tone)) if upper_tone == lower_tone => {
                Some((FULL_BLOCK, upper_tone, None))
            }
            (Some(upper_tone), Some(lower_tone))
                if color_support == style::ColorSupport::Enabled =>
            {
                Some((UPPER_HALF_BLOCK, upper_tone, Some(lower_tone)))
            }
            (Some(_), Some(lower_tone)) => Some((FULL_BLOCK, lower_tone, None)),
            (Some(tone), None) => Some((UPPER_HALF_BLOCK, tone, None)),
            (None, Some(tone)) => Some((LOWER_HALF_BLOCK, tone, None)),
        }
    }

    /// Returns `tone` brightened by one step when the sheen band covers a
    /// cell on `diagonal`.
    fn lit_tone(tone: Tone, diagonal: usize, sheen_center: Option<usize>) -> Tone {
        match sheen_center {
            Some(sheen_center) if diagonal.abs_diff(sheen_center) < SHEEN_RADIUS => tone.lit(),
            _ => tone,
        }
    }
}

impl Component for NerpaGreeting {
    fn render(&self, f: &mut Frame, area: Rect) {
        self.render_animated(f, area);
    }
}

/// Flat theme tone of one lit nerpa pixel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tone {
    /// Seal body.
    Body,
    /// Muzzle, whiskers, and eye catchlights.
    Highlight,
    /// Ice floe under the seal.
    Ice,
}

impl Tone {
    /// Parses one [`NERPA_PIXELS`] character, returning `None` for an empty
    /// pixel.
    fn from_pixel(pixel: u8) -> Option<Self> {
        match pixel {
            b'B' => Some(Self::Body),
            b'H' => Some(Self::Highlight),
            b'I' => Some(Self::Ice),
            _ => None,
        }
    }

    /// Returns the next brighter tone, used for cells under the sheen band.
    fn lit(self) -> Self {
        match self {
            Self::Ice => Self::Body,
            Self::Body | Self::Highlight => Self::Highlight,
        }
    }

    /// Maps the tone onto the active theme's semantic palette.
    fn color(self) -> Color {
        match self {
            Self::Body => style::palette::text_muted(),
            Self::Highlight => style::palette::text(),
            Self::Ice => style::palette::accent_soft(),
        }
    }
}

#[cfg(test)]
#[path = "nerpa_greeting_test.rs"]
mod tests;
