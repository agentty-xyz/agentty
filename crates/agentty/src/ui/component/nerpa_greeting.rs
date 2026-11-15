//! Animated pixel-art Baikal seal greeting that decorates an empty session
//! transcript.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::ui::{Component, style};

/// Pixel map of a Baikal seal resting on an ice floe, facing left, without
/// its hind flippers.
///
/// Each entry is one terminal row holding an upper and a lower row of square
/// pixels. `.` is empty, `B` the body, `H` the muzzle, whiskers, and eye
/// catchlights, and `I` the ice. The eyes and nose are empty pixels.
///
/// The rows covered by [`TAIL_POSES`] stay empty from [`TAIL_COLUMN`] on, so
/// the current tail pose alone draws the hind flippers there.
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
        "...BBBBBBBBBBBBBB...............",
    ],
    [
        "..BBB.HBBBBB.HBBBBBBBB..........",
        "..BBB..BBBBB..BBBBBBBBBBB.......",
    ],
    [
        "..BBBHHHHHHHHBBBBBBBBBBBBB......",
        "..BBBHHH..HHHBBBBBBBBBBBBB......",
    ],
    [
        "HHHBHHHHHHHHHHBHHHBBBBBBBB......",
        "...BHHHHHHHHHHBBBBBBBBBBBB......",
    ],
    [
        "HHH.BBBBBBBBBBBHHHBBBBBBBB......",
        "....BBBBBBBBBBBBBBBBBBBBB.......",
    ],
    [
        "IIIIIIIIIIIIIIIIIIIIIIIIIIIIIIII",
        "...IIIIIII....IIIIIIII...IIIII..",
    ],
];

/// Width of the art in terminal columns.
const ART_WIDTH: usize = NERPA_PIXELS[0][0].len();

/// First art column drawn by [`TAIL_POSES`].
const TAIL_COLUMN: usize = 26;

/// Hind-flipper poses, in the [`NERPA_PIXELS`] pixel format, covering the
/// art columns from [`TAIL_COLUMN`] in its top rows.
///
/// Each line of a pose is one terminal row: its upper and then its lower
/// pixel row, separated by whitespace.
///
/// Like the seal on the documentation site, the poses turn the flippers
/// about the tail joint: at rest, tip slightly up, tip further up, and tip
/// slightly down.
const TAIL_POSES: [&str; 4] = [
    "
    ...... ......
    ...... ...BB.
    ..BBB. .BBB..
    BBB... BBB...
    BBBB.. B.BBB.
    ...BB. ......
    ",
    "
    ...... ......
    ...BB. ...BB.
    ..BB.. .BB...
    BBB... BBBB..
    BBBBB. B.BBB.
    ...... ......
    ",
    "
    ...... ...BB.
    ...BB. ..BB..
    ..BB.. .BB...
    BBBB.. BBBBB.
    BBBBB. B.....
    ...... ......
    ",
    "
    ...... ......
    ...... ......
    ..BBB. .BBBB.
    BBBB.. BBB...
    BBB... B.BB..
    ...BB. ...BB.
    ",
];

/// Index of the resting pose in [`TAIL_POSES`].
const TAIL_REST_POSE: usize = 0;

/// [`TAIL_POSES`] indices one wag plays in order, matching the seal on the
/// documentation site.
const TAIL_WAG: [usize; 10] = [1, 2, 1, 0, 3, 0, 1, 2, 1, 0];

/// Time each [`TAIL_WAG`] pose stays on screen.
const TAIL_STEP_MS: u128 = 120;

/// Full tail cycle: the art rests, then the hind flippers wag once.
const TAIL_PERIOD_MS: u128 = 6_000;

/// Portion of [`TAIL_PERIOD_MS`], from the cycle start, before the wag.
///
/// The wag fills the rest of the cycle, one [`TAIL_STEP_MS`] per
/// [`TAIL_WAG`] pose. Starting each cycle at rest keeps captures taken at
/// whole-period timestamps, such as pinned-clock feature recordings, on the
/// resting art.
const TAIL_REST_MS: u128 = 4_800;

/// Glyph for a cell whose upper and lower pixels are both lit.
const FULL_BLOCK: char = '█';

/// Glyph for a cell whose upper pixel alone is lit.
const UPPER_HALF_BLOCK: char = '▀';

/// Glyph for a cell whose lower pixel alone is lit.
const LOWER_HALF_BLOCK: char = '▄';

/// Columns kept free between the art and the right edge of its area.
const RIGHT_PADDING: u16 = 1;

/// Pixel-art nerpa that greets a session before its first message.
///
/// Half-block glyphs give every terminal cell two square pixels, each filled
/// with a flat theme tone. Every few seconds the seal wags its hind flippers
/// through a short sequence of poses. The frame derives only from
/// `unix_millis`, so equal timestamps always paint identical frames.
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

    /// Returns whether the tail wags at `unix_millis`.
    ///
    /// Frames only change while the tail wags, so callers can redraw at frame
    /// rate during the wag and fall back to a slower cadence while the art
    /// rests.
    pub fn is_wagging(unix_millis: u128) -> bool {
        Self::wag_pose(unix_millis).is_some()
    }

    /// Renders the nerpa like [`Component::render`] and returns whether the
    /// painted art can animate.
    ///
    /// The wag changes glyphs, so the art animates whenever it fits a blank
    /// `area`, with or without colors. Callers use the result to skip
    /// frame-rate redraws while the art is hidden.
    pub fn render_animated(&self, f: &mut Frame, area: Rect) -> bool {
        self.paint(f.buffer_mut(), area)
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

        let tail_pose = TAIL_POSES[Self::wag_pose(self.unix_millis).unwrap_or(TAIL_REST_POSE)];
        for ((row, [upper_pixels, lower_pixels]), row_y) in
            NERPA_PIXELS.iter().enumerate().zip(art_area.y..)
        {
            let tail_row = Self::tail_row(tail_pose, row);
            for (column, column_x) in (0..ART_WIDTH).zip(art_area.x..) {
                let upper_pixel =
                    Self::pixel(upper_pixels, tail_row.map(|(upper, _)| upper), column);
                let lower_pixel =
                    Self::pixel(lower_pixels, tail_row.map(|(_, lower)| lower), column);
                let Some((glyph, foreground, background)) =
                    Self::cell_art(upper_pixel, lower_pixel, self.color_support)
                else {
                    continue;
                };

                let cell = &mut buffer[(column_x, row_y)];
                cell.set_char(glyph).set_fg(foreground.color());
                if let Some(background) = background {
                    cell.set_bg(background.color());
                }
            }
        }

        true
    }

    /// Returns the bottom-right rectangle occupied by the art, or `None` when
    /// it does not fit inside `area`.
    fn art_area(area: Rect) -> Option<Rect> {
        let width = u16::try_from(ART_WIDTH).ok()?;
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

    /// Returns the [`TAIL_POSES`] index shown at `unix_millis`, or `None`
    /// while the cycle rests.
    fn wag_pose(unix_millis: u128) -> Option<usize> {
        let wag_elapsed_ms = (unix_millis % TAIL_PERIOD_MS).checked_sub(TAIL_REST_MS)?;
        let wag_step = usize::try_from(wag_elapsed_ms / TAIL_STEP_MS).ok()?;

        TAIL_WAG.get(wag_step).copied()
    }

    /// Returns the upper and lower pixel rows of terminal `row` in one
    /// [`TAIL_POSES`] entry, or `None` below the rows the pose covers.
    fn tail_row(tail_pose: &str, row: usize) -> Option<(&str, &str)> {
        let mut pixel_rows = tail_pose.split_whitespace().skip(row * 2);

        Some((pixel_rows.next()?, pixel_rows.next()?))
    }

    /// Returns the pixel at `column` of one art pixel row, read from
    /// `tail_pixels` for the columns the tail pose covers.
    fn pixel(body_pixels: &str, tail_pixels: Option<&str>, column: usize) -> u8 {
        match (tail_pixels, column.checked_sub(TAIL_COLUMN)) {
            (Some(tail_pixels), Some(tail_column)) => tail_pixels.as_bytes()[tail_column],
            _ => body_pixels.as_bytes()[column],
        }
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
    /// Parses one [`NERPA_PIXELS`] or [`TAIL_POSES`] character, returning
    /// `None` for an empty pixel.
    fn from_pixel(pixel: u8) -> Option<Self> {
        match pixel {
            b'B' => Some(Self::Body),
            b'H' => Some(Self::Highlight),
            b'I' => Some(Self::Ice),
            _ => None,
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
