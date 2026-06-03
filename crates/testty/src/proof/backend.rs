//! Proof backend trait for swappable proof output formats.
//!
//! All proof rendering flows through [`ProofBackend`], allowing new visual
//! formats (text, strip, GIF, HTML) to be added without modifying
//! [`ProofReport`](super::report::ProofReport) or scenario code.

use std::path::Path;

use super::report::{ProofError, ProofReport};

/// Inputs passed to a [`ProofBackend`] when it renders a report.
///
/// Bundling the render inputs in a single struct lets new inputs (an output
/// directory, a theme, run metadata) be added later as additional fields
/// without changing the [`ProofBackend::render`] signature, so external
/// backend implementations stay source-compatible across non-breaking testty
/// releases. The struct is `#[non_exhaustive]`: backends read its fields
/// (`report`, `output`) but never construct it with a struct literal; build
/// one through [`RenderContext::new`].
#[non_exhaustive]
pub struct RenderContext<'a> {
    /// The proof report to render.
    pub report: &'a ProofReport,
    /// Destination path for the rendered output.
    pub output: &'a Path,
}

impl<'a> RenderContext<'a> {
    /// Create a render context from a report and its destination path.
    pub fn new(report: &'a ProofReport, output: &'a Path) -> Self {
        Self { report, output }
    }
}

/// Trait for rendering a [`ProofReport`] to a file.
///
/// Each implementation produces a different visual format. Render inputs are
/// passed by reference through a [`RenderContext`] so multiple backends can
/// render the same data and so new inputs can be added without breaking the
/// method signature for external implementors.
pub trait ProofBackend {
    /// Render the report described by `context` and write the output.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or I/O fails.
    fn render(&self, context: &RenderContext<'_>) -> Result<(), ProofError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal backend implementation for testing the trait contract.
    struct StubBackend {
        should_fail: bool,
    }

    impl ProofBackend for StubBackend {
        fn render(&self, _context: &RenderContext<'_>) -> Result<(), ProofError> {
            if self.should_fail {
                return Err(ProofError::Format("stub failure".to_string()));
            }

            Ok(())
        }
    }

    #[test]
    fn render_context_exposes_report_and_output() {
        // Arrange
        let report = ProofReport::new("ctx_scenario");
        let path = Path::new("/tmp/ctx.txt");

        // Act
        let context = RenderContext::new(&report, path);

        // Assert
        assert_eq!(context.report.scenario_name, "ctx_scenario");
        assert_eq!(context.output, path);
    }

    #[test]
    fn stub_backend_succeeds() {
        // Arrange
        let backend = StubBackend { should_fail: false };
        let report = ProofReport::new("test");

        // Act
        let result = backend.render(&RenderContext::new(&report, Path::new("/tmp/test.txt")));

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn stub_backend_returns_error() {
        // Arrange
        let backend = StubBackend { should_fail: true };
        let report = ProofReport::new("test");

        // Act
        let result = backend.render(&RenderContext::new(&report, Path::new("/tmp/test.txt")));

        // Assert
        assert!(result.is_err());
    }
}
