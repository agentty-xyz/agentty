//! Diagram text extraction helpers for the Mermaid tests.

use crate::mermaid::{MermaidDiagram, render_mermaid};

pub(super) fn diagram_text(diagram: &MermaidDiagram) -> String {
    diagram
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders a source that must produce a diagram and returns its text.
pub(super) fn rendered_text(source: &str) -> String {
    diagram_text(&render_mermaid(source).expect("diagram should render"))
}
