//! GIF proof backend for animated proof output.
//!
//! [`GifBackend`] renders each captured frame using the native renderer
//! and encodes them as an animated GIF with configurable inter-frame
//! delays, suitable for PR comments and documentation.

use std::fs::File;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Frame, RgbaImage};

use super::backend::{ProofBackend, RenderContext};
use super::report::ProofError;
use crate::frame::TerminalFrame;
use crate::renderer;

/// Default delay between frames in milliseconds.
const DEFAULT_FRAME_DELAY_MS: u32 = 1500;

/// Minimum allowed frame delay in milliseconds.
const MIN_FRAME_DELAY_MS: u32 = 200;

/// Maximum allowed frame delay in milliseconds.
const MAX_FRAME_DELAY_MS: u32 = 5000;

/// Renders a proof report as an animated GIF.
///
/// Each captured frame is rendered via the native bitmap font renderer
/// and encoded as a GIF frame with configurable timing.
pub struct GifBackend {
    /// Delay between frames in milliseconds.
    frame_delay_ms: u32,
}

impl GifBackend {
    /// Create a GIF backend with a custom frame delay.
    ///
    /// The delay is clamped to the range
    /// [`MIN_FRAME_DELAY_MS`]–[`MAX_FRAME_DELAY_MS`].
    pub fn with_delay_ms(delay_ms: u32) -> Self {
        Self {
            frame_delay_ms: delay_ms.clamp(MIN_FRAME_DELAY_MS, MAX_FRAME_DELAY_MS),
        }
    }

    /// Return the configured frame delay in milliseconds.
    pub fn frame_delay_ms(&self) -> u32 {
        self.frame_delay_ms
    }
}

impl Default for GifBackend {
    /// Create a GIF backend with the default frame delay.
    fn default() -> Self {
        Self {
            frame_delay_ms: DEFAULT_FRAME_DELAY_MS,
        }
    }
}

impl ProofBackend for GifBackend {
    /// Render the proof report as an animated GIF.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or encoding fails.
    fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError> {
        let report = context.report;
        let output = context.output;

        if report.captures.is_empty() {
            return Err(ProofError::Format(
                "cannot create GIF from empty report".to_string(),
            ));
        }

        let file = File::create(output)?;
        let mut encoder = GifEncoder::new_with_speed(file, 10);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|err| ProofError::Format(err.to_string()))?;

        // GIF delay is in units of 10ms.
        let delay_hundredths = self.frame_delay_ms / 10;

        for capture in &report.captures {
            let terminal_frame =
                TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes);
            let image = renderer::render_to_image(&terminal_frame);
            let gif_frame = build_gif_frame(image, delay_hundredths);
            encoder
                .encode_frame(gif_frame)
                .map_err(|err| ProofError::Format(err.to_string()))?;
        }

        Ok(())
    }
}

/// Build a GIF frame from an RGBA image with the specified delay.
fn build_gif_frame(image: RgbaImage, delay_hundredths: u32) -> Frame {
    let delay = image::Delay::from_saturating_duration(std::time::Duration::from_millis(
        u64::from(delay_hundredths) * 10,
    ));

    Frame::from_parts(image, 0, 0, delay)
}

#[cfg(test)]
#[path = "gif_test.rs"]
mod tests;
