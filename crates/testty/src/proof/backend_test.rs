use std::path::Path;

use crate::proof::backend::{ProofBackend, RenderContext};
use crate::proof::report::{ProofError, ProofReport};
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
