pub use ag_tui_text::mermaid::MermaidDiagram;

use crate::ui::style;

/// Renders one ```` ```mermaid ```` source block using Agentty's active UI
/// theme.
pub fn render_mermaid(source: &str) -> Option<MermaidDiagram> {
    ag_tui_text::mermaid::render_mermaid_with_settings(source, style::text_render_settings())
}
