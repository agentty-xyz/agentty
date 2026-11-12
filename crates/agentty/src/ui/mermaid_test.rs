use super::render_mermaid;
use crate::domain::theme::ColorTheme;
use crate::ui::style;

#[test]
fn test_render_mermaid_paints_diagram_with_active_theme_text_color() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Green);
    let source = "graph TD\n    Start --> Finish";

    // Act
    let diagram = render_mermaid(source).expect("flowchart should render");

    // Assert
    let rendered_text = diagram
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered_text.contains("Start"));
    assert!(rendered_text.contains("Finish"));
    assert!(
        diagram
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| !span.content.trim().is_empty())
            .all(|span| span.style.fg == Some(style::palette::text()))
    );
}

#[test]
fn test_render_mermaid_rejects_unsupported_source() {
    // Arrange
    let source = "not a mermaid diagram";

    // Act
    let diagram = render_mermaid(source);

    // Assert
    assert!(diagram.is_none());
}
