//! Screenshot strip proof backend.
//!
//! [`ScreenshotStripBackend`] renders each captured frame using the native
//! renderer and composes them vertically into a single tall PNG image with
//! step labels between each frame.

use image::{Rgba, RgbaImage, imageops};

use super::backend::{ProofBackend, RenderContext};
use super::report::{ProofCapture, ProofError, ProofReport};
use crate::frame::TerminalFrame;
use crate::renderer;

/// Height of the label header between frames in pixels.
const LABEL_HEIGHT: u32 = 32;

/// Background color for label headers.
const LABEL_BG: Rgba<u8> = Rgba([50, 50, 50, 255]);

/// Text color for label headers.
const LABEL_FG: Rgba<u8> = Rgba([220, 220, 220, 255]);

/// Padding between frames in pixels.
const FRAME_PADDING: u32 = 4;

/// Renders a proof report as a vertical PNG strip of screenshots.
///
/// Each capture is rendered using the native bitmap font renderer,
/// with step labels drawn between frames for context.
pub struct ScreenshotStripBackend;

impl ProofBackend for ScreenshotStripBackend {
    /// Render the proof report as a vertical strip PNG.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError::Io`] if writing the image file fails.
    fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError> {
        let report = context.report;

        let rendered_frames = render_all_frames(report);
        let strip = compose_strip(&rendered_frames, report);
        strip.save(context.output).map_err(ProofError::format)?;

        Ok(())
    }
}

/// Render all captured frames to images using the native renderer.
fn render_all_frames(report: &ProofReport) -> Vec<(RgbaImage, &ProofCapture)> {
    report
        .captures
        .iter()
        .map(|capture| {
            let frame = TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes);
            let image = renderer::render_to_image(&frame);

            (image, capture)
        })
        .collect()
}

/// Compose rendered frames into a single vertical strip with labels.
fn compose_strip(frames: &[(RgbaImage, &ProofCapture)], report: &ProofReport) -> RgbaImage {
    if frames.is_empty() {
        return RgbaImage::new(1, 1);
    }

    // Calculate total dimensions.
    let max_width = frames
        .iter()
        .map(|(image, _)| image.width())
        .max()
        .unwrap_or(1);

    let total_height: u32 = frames
        .iter()
        .enumerate()
        .map(|(index, (image, _))| {
            let label_h = LABEL_HEIGHT;
            let padding = if index + 1 < frames.len() {
                FRAME_PADDING
            } else {
                0
            };

            label_h + image.height() + padding
        })
        .sum();

    let mut strip = RgbaImage::from_pixel(max_width, total_height, Rgba([30, 30, 30, 255]));
    let mut current_y = 0u32;

    for (index, (frame_image, capture)) in frames.iter().enumerate() {
        // Draw label header.
        draw_label_header(
            &mut strip,
            current_y,
            max_width,
            index + 1,
            &capture.label,
            &capture.description,
        );
        current_y += LABEL_HEIGHT;

        // Copy frame image.
        imageops::replace(&mut strip, frame_image, 0, i64::from(current_y));
        current_y += frame_image.height();

        // Add padding between frames.
        if index + 1 < report.captures.len() {
            current_y += FRAME_PADDING;
        }
    }

    strip
}

/// Draw a label header with step number, label, and description.
fn draw_label_header(
    image: &mut RgbaImage,
    start_y: u32,
    width: u32,
    step_number: usize,
    label: &str,
    description: &str,
) {
    // Fill background.
    renderer::fill_rect(image, 0, start_y, width, LABEL_HEIGHT, LABEL_BG);

    // Render label text using the bitmap font.
    let text = format!("Step {step_number}: [{label}] {description}");
    draw_text_line(image, 4, start_y + 8, &text, LABEL_FG);
}

/// Draw a line of text using the bitmap font from the renderer module.
///
/// Each character is 8 pixels wide and rendered from the embedded font.
fn draw_text_line(image: &mut RgbaImage, start_x: u32, start_y: u32, text: &str, color: Rgba<u8>) {
    let mut cursor_x = start_x;

    for character in text.chars() {
        if cursor_x + 8 > image.width() {
            break;
        }

        draw_bitmap_char(image, cursor_x, start_y, character, color);
        cursor_x += 8;
    }
}

/// Draw a single character from the embedded ASCII bitmap font.
fn draw_bitmap_char(
    image: &mut RgbaImage,
    pixel_x: u32,
    pixel_y: u32,
    character: char,
    color: Rgba<u8>,
) {
    let code = character as u32;

    // Only render ASCII printable characters.
    if !(0x20..=0x7E).contains(&code) {
        return;
    }

    let glyph_index = (code - 0x20) as usize;

    renderer::draw_glyph(
        image,
        pixel_x,
        pixel_y,
        &renderer::ASCII_GLYPHS[glyph_index],
        color,
    );
}

#[cfg(test)]
#[path = "strip_test.rs"]
mod tests;
