use super::support::diagram_text;
use crate::mermaid::render_mermaid;

#[test]
fn test_render_mermaid_draws_er_diagram_with_cardinality_markers() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    CUSTOMER ||--o{ ORDER : places\n",
        "    CUSTOMER ||--|| ACCOUNT : owns\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("er diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("CUSTOMER"));
    assert!(text.contains("ORDER"));
    assert!(text.contains("ACCOUNT"));
    assert!(text.contains("places"));
    assert!(text.contains('1'));
    assert!(text.contains('*'));
    assert!(!text.contains('▼'));
}

#[test]
fn test_render_mermaid_er_omits_attribute_blocks() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    CUSTOMER {\n",
        "        string name\n",
        "    }\n",
        "    CUSTOMER ||--o{ ORDER : places\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("er diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("CUSTOMER"));
    assert!(text.contains("ORDER"));
    assert!(!text.contains("string"));
}

#[test]
fn test_render_mermaid_er_supports_hyphenated_entities_and_bare_links() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    ORDER ||--|{ LINE-ITEM : contains\n",
        "    LINE-ITEM }o..o| DISCOUNT\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("er diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("LINE-ITEM"));
    assert!(text.contains("DISCOUNT"));
    assert!(text.contains('+'));
    assert!(text.contains('?'));
}

#[test]
fn test_render_mermaid_er_rejects_unknown_relationship_operators() {
    // Arrange & Act & Assert
    assert!(render_mermaid("erDiagram\n    A |x--o{ B : bad").is_none());
    assert!(render_mermaid("erDiagram\n    A ||==o{ B : bad").is_none());
    assert!(render_mermaid("erDiagram").is_none());
}
