//! Frame-text proof backend.
//!
//! [`FrameTextBackend`] renders a [`ProofReport`](super::report::ProofReport)
//! as annotated plain text, producing the same output as
//! [`ProofReport::to_annotated_text()`](super::report::ProofReport::to_annotated_text)
//! but routed through the [`ProofBackend`](super::backend::ProofBackend) trait.

use super::backend::{ProofBackend, RenderContext};
use super::report::ProofError;

/// Renders a proof report as annotated plain-text frame dumps.
///
/// This is the simplest backend, writing each captured frame as a
/// bordered text block with step labels, descriptions, and assertion
/// markers. The output is identical to [`ProofReport::to_annotated_text()`].
pub struct FrameTextBackend;

impl ProofBackend for FrameTextBackend {
    /// Write the annotated text proof to the given output path.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError::Io`] if writing the file fails.
    fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError> {
        let text = context.report.to_annotated_text();
        std::fs::write(context.output, text)?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "frame_text_test.rs"]
mod tests;
