use super::support::diagram_text;
use crate::mermaid::{
    Canvas, MermaidEdge, draw_feedback_edge_label, layout_layers, parse_graph, render_mermaid,
};

#[test]
fn test_render_mermaid_draws_left_right_feedback_cycle() {
    // Arrange
    let source = concat!(
        "flowchart LR\n",
        "    A[\"App\"] -- \"commands:<br/>prompt · interrupt · permission answer\" --> \
         H[\"ag-harness\"]\n",
        "    H -- \"typed events:<br/>deltas · tool calls · diffs · usage\" --> A\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("two-node feedback graph should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("App"));
    assert!(text.contains("ag-harness"));
    assert!(text.contains("commands:"));
    assert!(text.contains("typed events:"));
    assert!(text.contains('▶'));
    assert!(text.contains('◀'));
    assert!(!text.contains("flowchart LR"));
    assert!(!text.contains("<br/>"));
}

#[test]
fn test_render_mermaid_draws_multi_node_feedback_cycles() {
    // Arrange
    let source = concat!(
        "flowchart LR\n",
        "    U[User and TUI] --> C[Orchestrator controller]\n",
        "    M[Agent model] --> P[Typed command response]\n",
        "    P --> C\n",
        "    C --> S[ag-session service]\n",
        "    S --> A[Agentty host adapter]\n",
        "    A --> W[Session workers]\n",
        "    W --> E[Session events]\n",
        "    E --> C\n",
        "    C --> M\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("cyclic flowchart should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Orchestrator controller"));
    assert!(text.contains("Typed command response"));
    assert!(text.contains("Session events"));
    assert!(text.contains("Session events ───▶ Orchestrator controller"));
    assert!(text.contains("Orchestrator controller ───▶ Agent model"));
    assert!(!text.contains("flowchart LR"));
    assert!(diagram.width < 80);
}

#[test]
fn test_render_mermaid_keeps_independent_feedback_cycles_separate() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A[First start] --> B[First middle]\n",
        "    B --> C[First end]\n",
        "    C --> A\n",
        "    X[Second start] --> Y[Second middle]\n",
        "    Y --> Z[Second end]\n",
        "    Z --> X\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("independent cycles should render");
    let feedback_lines = diagram
        .lines
        .iter()
        .map(ToString::to_string)
        .filter(|line| line.contains("───▶"))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        feedback_lines,
        ["First end ───▶ First start", "Second end ───▶ Second start",]
    );
}

#[test]
fn test_render_mermaid_draws_labeled_top_down_feedback_edge() {
    // Arrange
    let source = "flowchart TD\n    A[Start] --> B[Work]\n    B <-->|retry| A";

    // Act
    let diagram = render_mermaid(source).expect("labeled cycle should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("retry"));
    assert!(text.contains('◀'));
    assert!(text.contains('▶'));
}

#[test]
fn test_render_mermaid_keeps_self_link_fallback() {
    // Arrange, Act, Assert
    assert!(render_mermaid("flowchart LR\n    A --> A").is_none());
}

#[test]
fn test_render_mermaid_uses_invisible_feedback_for_layout_only() {
    // Arrange
    let source = "flowchart TD\n    A[Start] --> B[Finish]\n    B ~~~ A";

    // Act
    let diagram = render_mermaid(source).expect("invisible feedback should not reject diagram");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert_eq!(text.matches('▼').count(), 1);
    assert!(!text.contains('◀'));
}

#[test]
fn test_render_mermaid_treats_reciprocal_source_arrow_as_cycle() {
    // Arrange
    let top_down = "flowchart TD\n    A --> B\n    A <-- B";
    let left_right = "flowchart LR\n    A --> B\n    A <-- B";

    // Act
    let top_down_diagram = render_mermaid(top_down).expect("top-down feedback loop should render");
    let top_down_text = diagram_text(&top_down_diagram);
    let left_right_diagram =
        render_mermaid(left_right).expect("two-node feedback loop should render");
    let left_right_text = diagram_text(&left_right_diagram);

    // Assert
    assert!(top_down_text.contains("B ───▶ A"));
    assert!(left_right_text.contains('▶'));
    assert!(left_right_text.contains('◀'));
}

#[test]
fn test_render_mermaid_renders_small_cycles() {
    // Arrange
    let cyclic = "graph TD\n    A --> B\n    B --> A";
    let three_node_cycle = "graph LR\n    A --> B\n    B --> C\n    C --> A";

    // Act
    let top_down_diagram = render_mermaid(cyclic).expect("top-down cycle should render");
    let left_right_diagram =
        render_mermaid(three_node_cycle).expect("left-right cycle should render");

    // Assert
    assert!(diagram_text(&top_down_diagram).contains("B ───▶ A"));
    assert!(diagram_text(&left_right_diagram).contains("C ───▶ A"));
}

#[test]
fn test_render_mermaid_draws_two_node_feedback_arrow_variants() {
    // Arrange
    let cases = [
        ("flowchart LR\n    A --> B\n    B ~~~ A", 1, 0),
        ("flowchart LR\n    A <--> B\n    B --- A", 1, 1),
        ("flowchart LR\n    A --> B\n    B <--> A", 2, 1),
    ];

    for (source, expected_right_arrows, expected_left_arrows) in cases {
        // Act
        let diagram = render_mermaid(source).expect("two-node feedback graph should render");
        let text = diagram_text(&diagram);

        // Assert
        assert_eq!(text.matches('▶').count(), expected_right_arrows, "{source}");
        assert_eq!(text.matches('◀').count(), expected_left_arrows, "{source}");
    }
}

#[test]
fn test_draw_feedback_edge_label_skips_label_wider_than_run() {
    // Arrange
    let mut canvas = Canvas::new(6, 1);
    let edge = MermaidEdge {
        from_index: 0,
        has_arrow: true,
        has_source_arrow: false,
        is_visible: true,
        label: Some("long".to_string()),
        source_marker: None,
        target_marker: None,
        to_index: 1,
    };

    // Act
    draw_feedback_edge_label(&mut canvas, &edge, 0, 3, 0);
    let diagram = canvas.into_diagram();

    // Assert
    assert_eq!(diagram.width, 0);
}

#[test]
fn test_layout_layers_rejects_cycles_and_layer_skipping_edges() {
    // Arrange
    let sources = [
        "graph TD\n    A --> B\n    B --> A",
        "graph TD\n    A --> A",
        "graph TD\n    A --> B --> C\n    A --> C",
    ];

    for source in sources {
        let graph = parse_graph(source).expect("graph should parse");

        // Act
        let layout = layout_layers(&graph);

        // Assert
        assert!(layout.is_none(), "{source:?}");
    }
}
