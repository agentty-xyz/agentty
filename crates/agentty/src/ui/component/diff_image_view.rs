//! Before/after image diff preview drawn with half-block terminal cells.
//!
//! Each terminal cell shows two vertically stacked pixels: the upper pixel as
//! the foreground of `▀` and the lower pixel as its background, so cells map
//! to roughly square pixels. Pixel colors come from the image itself rather
//! than the semantic palette.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::presentation::diff_image::{DiffImage, DiffImagePair, DiffImageSide};
use crate::ui::{Component, style};

/// Upper half block whose foreground paints the upper pixel of a cell.
const UPPER_HALF_BLOCK: &str = "▀";

/// Lower half block used when only the lower pixel of a cell is opaque.
const LOWER_HALF_BLOCK: &str = "▄";

/// Alpha at or above which a sampled pixel is drawn instead of left
/// transparent.
const OPAQUE_ALPHA_THRESHOLD: u8 = 128;

/// Columns separating side-by-side images.
const SIDE_BY_SIDE_GAP: u16 = 2;

/// Renders the visible sides of one changed image, labeled with their role
/// and source dimensions.
pub struct DiffImageView<'a> {
    images: &'a DiffImagePair,
}

impl<'a> DiffImageView<'a> {
    /// Creates a view over one image pair.
    pub fn new(images: &'a DiffImagePair) -> Self {
        Self { images }
    }

    /// Returns labeled sides to draw, omitting sides absent from the change.
    fn visible_sides(&self) -> Vec<(&'static str, VisibleSide<'a>)> {
        let before = VisibleSide::from_side(&self.images.before);
        let after = VisibleSide::from_side(&self.images.after);
        match (before, after) {
            (None, None) => Vec::new(),
            (None, Some(after)) => vec![("Added", after)],
            (Some(before), None) => vec![("Deleted", before)],
            (Some(before), Some(after)) => vec![("Before", before), ("After", after)],
        }
    }

    /// Splits `area` into one labeled slot per side, choosing side-by-side or
    /// stacked placement to maximize the drawn image area.
    fn side_slots(sides: &[(&'static str, VisibleSide<'_>)], area: Rect) -> Vec<Rect> {
        if sides.len() < 2 {
            return vec![area];
        }

        let side_by_side = Self::side_by_side_slots(area);
        let stacked = Self::stacked_slots(area);
        let drawn_area = |slots: &[Rect]| -> u64 {
            sides
                .iter()
                .zip(slots)
                .map(|((_, side), slot)| match side {
                    VisibleSide::Image(image) => {
                        let (columns, rows) =
                            fitted_cell_size(image, slot.width, slot.height.saturating_sub(1));

                        u64::from(columns) * u64::from(rows)
                    }
                    VisibleSide::Unavailable(_) => 0,
                })
                .sum()
        };
        if drawn_area(&stacked) > drawn_area(&side_by_side) {
            return stacked.to_vec();
        }

        side_by_side.to_vec()
    }

    /// Returns two equal-width slots separated by [`SIDE_BY_SIDE_GAP`].
    fn side_by_side_slots(area: Rect) -> [Rect; 2] {
        let slot_width = area.width.saturating_sub(SIDE_BY_SIDE_GAP) / 2;
        let right_x = area.x + area.width - slot_width;

        [
            Rect::new(area.x, area.y, slot_width, area.height),
            Rect::new(right_x, area.y, slot_width, area.height),
        ]
    }

    /// Returns two full-width slots splitting the height.
    fn stacked_slots(area: Rect) -> [Rect; 2] {
        let top_height = area.height / 2;

        [
            Rect::new(area.x, area.y, area.width, top_height),
            Rect::new(
                area.x,
                area.y + top_height,
                area.width,
                area.height - top_height,
            ),
        ]
    }

    /// Draws one side's label row and its image or unavailable reason.
    fn render_side(frame: &mut Frame, label: &str, side: VisibleSide<'_>, slot: Rect) {
        if slot.width == 0 || slot.height == 0 {
            return;
        }

        let label_area = Rect::new(slot.x, slot.y, slot.width, 1);
        let body_area = Rect::new(slot.x, slot.y + 1, slot.width, slot.height - 1);
        let mut label_spans = vec![Span::styled(
            label.to_string(),
            Style::default().fg(style::palette::text()),
        )];
        match side {
            VisibleSide::Image(image) => {
                label_spans.push(Span::styled(
                    format!(" · {}×{}", image.original_width, image.original_height),
                    Style::default().fg(style::palette::text_muted()),
                ));
                render_image(frame.buffer_mut(), image, body_area);
            }
            VisibleSide::Unavailable(reason) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        reason.to_string(),
                        Style::default().fg(style::palette::warning()),
                    ))),
                    body_area,
                );
            }
        }
        frame.render_widget(Paragraph::new(Line::from(label_spans)), label_area);
    }
}

impl Component for DiffImageView<'_> {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let sides = self.visible_sides();
        if sides.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "Image unavailable.",
                    Style::default().fg(style::palette::warning()),
                ))),
                area,
            );

            return;
        }

        let slots = Self::side_slots(&sides, area);
        for ((label, side), slot) in sides.into_iter().zip(slots) {
            Self::render_side(frame, label, side, slot);
        }
    }
}

/// One side that exists in the change, borrowed for drawing.
#[derive(Clone, Copy)]
enum VisibleSide<'a> {
    /// The decoded first frame.
    Image(&'a DiffImage),
    /// The user-facing reason the existing side cannot be shown.
    Unavailable(&'a str),
}

impl<'a> VisibleSide<'a> {
    /// Returns the drawable view of `side`, or `None` when the side is absent
    /// from the change.
    fn from_side(side: &'a DiffImageSide) -> Option<Self> {
        match side {
            DiffImageSide::Absent => None,
            DiffImageSide::Image(image) => Some(Self::Image(image)),
            DiffImageSide::Unavailable(reason) => Some(Self::Unavailable(reason)),
        }
    }
}

/// Returns the aspect-preserving cell size of `image` fitted inside
/// `columns` by `rows` cells, where each cell holds two vertical pixels.
///
/// Images scale both up and down to fill the space; each non-empty edge
/// keeps at least one cell.
fn fitted_cell_size(image: &DiffImage, columns: u16, rows: u16) -> (u16, u16) {
    let (pixel_width, pixel_height) = fitted_pixel_size(image, columns, rows);

    (pixel_width, pixel_height.div_ceil(2))
}

/// Returns the fitted size in half-block pixels.
fn fitted_pixel_size(image: &DiffImage, columns: u16, rows: u16) -> (u16, u16) {
    if columns == 0 || rows == 0 || image.width == 0 || image.height == 0 {
        return (0, 0);
    }

    let max_width = u64::from(columns);
    let max_height = u64::from(rows) * 2;
    let image_width = u64::from(image.width);
    let image_height = u64::from(image.height);
    let (width, height) = if image_height * max_width <= max_height * image_width {
        (max_width, image_height * max_width / image_width)
    } else {
        (image_width * max_height / image_height, max_height)
    };

    (
        u16::try_from(width.max(1)).unwrap_or(columns),
        u16::try_from(height.max(1)).unwrap_or(u16::MAX),
    )
}

/// Paints `image` fitted and centered inside `area` directly into `buffer`.
fn render_image(buffer: &mut Buffer, image: &DiffImage, area: Rect) {
    let (pixel_width, pixel_height) = fitted_pixel_size(image, area.width, area.height);
    let cell_rows = pixel_height.div_ceil(2);
    let origin_x = area.x + (area.width - pixel_width) / 2;
    let origin_y = area.y + (area.height - cell_rows) / 2;

    for row in 0..cell_rows {
        for column in 0..pixel_width {
            let upper_pixel = sampled_pixel(image, column, row * 2, pixel_width, pixel_height);
            let lower_pixel = (row * 2 + 1 < pixel_height)
                .then(|| sampled_pixel(image, column, row * 2 + 1, pixel_width, pixel_height))
                .flatten();
            let Some((symbol, cell_style)) = cell_paint(upper_pixel, lower_pixel) else {
                continue;
            };

            buffer[(origin_x + column, origin_y + row)]
                .set_symbol(symbol)
                .set_style(cell_style);
        }
    }
}

/// Returns the glyph and colors for one cell from its two sampled pixels,
/// or `None` when both are transparent.
fn cell_paint(
    upper_pixel: Option<Color>,
    lower_pixel: Option<Color>,
) -> Option<(&'static str, Style)> {
    match (upper_pixel, lower_pixel) {
        (Some(upper), Some(lower)) => {
            Some((UPPER_HALF_BLOCK, Style::default().fg(upper).bg(lower)))
        }
        (Some(upper), None) => Some((UPPER_HALF_BLOCK, Style::default().fg(upper))),
        (None, Some(lower)) => Some((LOWER_HALF_BLOCK, Style::default().fg(lower))),
        (None, None) => None,
    }
}

/// Box-samples the source pixels covered by one output pixel, returning
/// `None` when the averaged alpha is below [`OPAQUE_ALPHA_THRESHOLD`].
fn sampled_pixel(
    image: &DiffImage,
    output_x: u16,
    output_y: u16,
    output_width: u16,
    output_height: u16,
) -> Option<Color> {
    let (start_x, end_x) = source_span(output_x, output_width, image.width);
    let (start_y, end_y) = source_span(output_y, output_height, image.height);
    let mut channel_sums = [0_u64; 4];
    for source_y in start_y..end_y {
        for source_x in start_x..end_x {
            let [red, green, blue, alpha] = image.pixel(source_x, source_y);
            let alpha_weight = u64::from(alpha);
            channel_sums[0] += u64::from(red) * alpha_weight;
            channel_sums[1] += u64::from(green) * alpha_weight;
            channel_sums[2] += u64::from(blue) * alpha_weight;
            channel_sums[3] += alpha_weight;
        }
    }
    let sample_count = u64::from(end_x - start_x) * u64::from(end_y - start_y);
    let average_alpha = channel_sums[3] / sample_count;
    if average_alpha < u64::from(OPAQUE_ALPHA_THRESHOLD) {
        return None;
    }

    let channel = |sum: u64| u8::try_from(sum / channel_sums[3]).unwrap_or(u8::MAX);

    Some(Color::Rgb(
        channel(channel_sums[0]),
        channel(channel_sums[1]),
        channel(channel_sums[2]),
    ))
}

/// Returns the half-open source range covered by one output coordinate,
/// always spanning at least one source pixel.
fn source_span(output_index: u16, output_length: u16, source_length: u32) -> (u32, u32) {
    let output_length = u64::from(output_length);
    let source_length = u64::from(source_length);
    let start = u64::from(output_index) * source_length / output_length;
    let end = (u64::from(output_index) + 1) * source_length / output_length;
    let start = u32::try_from(start).unwrap_or(u32::MAX);
    let end = u32::try_from(end.max(u64::from(start) + 1)).unwrap_or(u32::MAX);

    (start, end)
}

#[cfg(test)]
#[path = "diff_image_view_test.rs"]
mod tests;
