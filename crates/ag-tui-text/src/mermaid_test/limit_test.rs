use std::fmt::Write;

use super::support::diagram_text;
use crate::mermaid::{
    MAX_EDGE_COUNT, MAX_LABEL_WIDTH, MAX_NODE_COUNT, MAX_SOURCE_BYTE_COUNT, MAX_SOURCE_LINE_COUNT,
    render_mermaid,
};

#[test]
fn test_render_mermaid_rejects_double_width_edge_labels() {
    // Arrange
    let pipe_label = "flowchart TD\n    A -->|你好| B";
    let inline_label = "flowchart TD\n    A -- 你好 --> B";
    let er_label = concat!(
        "erDiagram\n",
        "    PROJECT ||--o{ SESSION : 你好\n",
        "    SESSION ||--|| WORKTREE : owns",
    );

    // Act & Assert
    assert!(render_mermaid(pipe_label).is_none());
    assert!(render_mermaid(inline_label).is_none());
    assert!(render_mermaid(er_label).is_none());
}

#[test]
fn test_render_mermaid_rejects_unsupported_diagram_types() {
    // Arrange & Act & Assert
    assert!(render_mermaid("graph RL\n    A --> B").is_none());
    assert!(render_mermaid("").is_none());
}

#[test]
fn test_render_mermaid_rejects_source_over_preview_limits() {
    // Arrange
    let mut long_source = String::from("graph TD");
    for node_index in 0..MAX_SOURCE_LINE_COUNT {
        write!(&mut long_source, "\n    N{node_index}").expect("writing to String should succeed");
    }

    let wide_source = format!("graph TD\n    A[{}]", "x".repeat(MAX_SOURCE_BYTE_COUNT));

    // Act & Assert
    assert!(render_mermaid(&long_source).is_none());
    assert!(render_mermaid(&wide_source).is_none());
}

#[test]
fn test_render_mermaid_rejects_node_and_edge_over_preview_limits() {
    // Arrange
    let mut too_many_nodes = String::from("graph TD");
    for node_index in 0..=MAX_NODE_COUNT {
        write!(&mut too_many_nodes, "\n    N{node_index}")
            .expect("writing to String should succeed");
    }

    let mut too_many_edges = String::from("graph TD");
    for _ in 0..=MAX_EDGE_COUNT {
        too_many_edges.push_str("\n    A --> B");
    }

    // Act & Assert
    assert!(render_mermaid(&too_many_nodes).is_none());
    assert!(render_mermaid(&too_many_edges).is_none());
}

#[test]
fn test_render_mermaid_rejects_wide_character_labels() {
    // Arrange
    let source = "graph TD\n    A[你好] --> B";

    // Act & Assert
    assert!(render_mermaid(source).is_none());
}

#[test]
fn test_render_mermaid_truncates_over_long_node_labels() {
    // Arrange
    let long_identifier = "N".repeat(MAX_LABEL_WIDTH + 1);
    let long_bare = format!("graph TD\n    {long_identifier} --> B");
    let long_labeled =
        "graph TD\n    A[This label is much longer than thirty-two characters] --> B";
    let wide_bare = "graph TD\n    你好 --> B";

    // Act
    let bare_diagram = render_mermaid(&long_bare).expect("long bare id should render");
    let labeled_diagram = render_mermaid(long_labeled).expect("long label should render");

    // Assert
    assert!(diagram_text(&bare_diagram).contains('…'));
    assert!(diagram_text(&labeled_diagram).contains("This label is much longer than …"));
    assert!(render_mermaid(wide_bare).is_none());
}

#[test]
fn test_render_mermaid_uses_first_node_label_line() {
    // Arrange
    let source = concat!(
        "flowchart TB\n",
        "    APP[\"App - owns orchestration:<br/>spawning, coordination, aggregation\"]\n",
        "    S1[\"session 1\"]\n",
        "    S2[\"session 2\"]\n",
        "    S3[\"session 3\"]\n",
        "    APP --> S1\n",
        "    APP --> S2\n",
        "    APP --> S3\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("node label with line break should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("App - owns orchestration:"));
    assert!(text.contains("session 1"));
    assert!(text.contains("session 2"));
    assert!(text.contains("session 3"));
    assert!(text.contains('▼'));
    assert!(!text.contains("<br/>"));
    assert!(!text.contains("spawning, coordination, aggregation"));
}
