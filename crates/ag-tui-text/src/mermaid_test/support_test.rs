//! Diagram text extraction helper for the Mermaid tests.

use crate::mermaid::MermaidDiagram;

pub(super) fn diagram_text(diagram: &MermaidDiagram) -> String {
    diagram
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
