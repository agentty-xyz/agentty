use super::support::diagram_text;
use crate::mermaid::{MAX_LABEL_WIDTH, render_mermaid};
use crate::style;

#[test]
fn test_render_mermaid_uses_text_color_for_structure() {
    // Arrange
    let source = "graph TD\n    A[Start] --> B[Finish]";

    // Act
    let diagram = render_mermaid(source).expect("chain should render");
    let structure_span = diagram
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains('┌'))
        .expect("structure span should render");

    // Assert
    assert_eq!(structure_span.style.fg, Some(style::palette::text()));
}

#[test]
fn test_render_mermaid_draws_top_down_chain() {
    // Arrange
    let source = "graph TD\n    A[Start] --> B[Finish]";

    // Act
    let diagram = render_mermaid(source).expect("chain should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains('┌'));
    assert!(text.contains('▼'));
    assert!(diagram.width > 0);
}

#[test]
fn test_render_mermaid_draws_branching_diamond() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A --> C\n    B --> D\n    C --> D";

    // Act
    let diagram = render_mermaid(source).expect("diamond should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('B'));
    assert!(text.contains('C'));
    assert_eq!(text.matches('▼').count(), 3);
}

#[test]
fn test_render_mermaid_draws_top_down_long_edge() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A[User starts session] --> B{Choose action}\n",
        "    B -->|Ask agent| C[Send prompt]\n",
        "    B -->|Review changes| D[Open diff view]\n",
        "    C --> E[Agent works in worktree]\n",
        "    E --> F[Run checks]\n",
        "    F --> G[Report result]\n",
        "    D --> G\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("long-edge flowchart should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("User starts session"));
    assert!(text.contains("Open diff view"));
    assert!(text.contains("Report result"));
    assert!(text.contains('▼'));
}

#[test]
fn test_render_mermaid_draws_left_right_direction() {
    // Arrange
    let source = "flowchart LR\n    A[In] --> B[Out]";

    // Act
    let diagram = render_mermaid(source).expect("LR chain should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('▶'));
    let first_box_line = text
        .lines()
        .find(|line| line.contains("In"))
        .expect("label row");
    assert!(first_box_line.contains("Out"));
}

#[test]
fn test_render_mermaid_writes_edge_label_on_track() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A -->|yes| C";

    // Act
    let diagram = render_mermaid(source).expect("labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("yes"));
}

#[test]
fn test_render_mermaid_supports_rounded_and_chained_statements() {
    // Arrange
    let source = "graph TD; A(Begin) --> B{Choice}; B --> C((End))";

    // Act
    let diagram = render_mermaid(source).expect("chained statements should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('╭'));
    assert!(text.contains("Begin"));
    assert!(text.contains("Choice"));
    assert!(text.contains("End"));
}

#[test]
fn test_render_mermaid_maps_extended_node_shapes() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A([Stadium]) --> B[[Subroutine]]\n",
        "    B --> C[(Cylinder)]\n",
        "    C --> D{{Hexagon}}\n",
        "    D --> E(((Core)))\n",
        "    E --> F>Flag]",
    );

    // Act
    let diagram = render_mermaid(source).expect("extended shapes should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Stadium"));
    assert!(text.contains("Subroutine"));
    assert!(text.contains("Cylinder"));
    assert!(text.contains("Hexagon"));
    assert!(text.contains("Core"));
    assert!(text.contains("Flag"));
    assert!(!text.contains('['));
}

#[test]
fn test_render_mermaid_expands_ampersand_groups() {
    // Arrange
    let source = "flowchart TD\n    A --> B & C\n    B & C --> D";

    // Act
    let diagram = render_mermaid(source).expect("ampersand groups should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('B'));
    assert!(text.contains('C'));
    assert_eq!(text.matches('▼').count(), 3);
}

#[test]
fn test_render_mermaid_accepts_extended_arrow_variants() {
    // Arrange
    let long_arrow = "flowchart TD\n    A ----> B";
    let source_arrow = "flowchart TD\n    A <-- B";
    let bidirectional = "flowchart TD\n    A <--> B";
    let circle_ends = "flowchart TD\n    A o--o B";
    let cross_ends = "flowchart TD\n    A x--x B";
    let long_arrow_label = "flowchart TD\n    A[Alpha stage] ---->|later| B[Beta stage]";

    // Act
    let long_arrow_diagram = render_mermaid(long_arrow).expect("long arrow should render");
    let source_arrow_diagram = render_mermaid(source_arrow).expect("source arrow should render");
    let bidirectional_diagram =
        render_mermaid(bidirectional).expect("bidirectional arrow should render");
    let labeled_diagram =
        render_mermaid(long_arrow_label).expect("labeled long arrow should render");

    // Assert
    assert!(diagram_text(&long_arrow_diagram).contains('▼'));
    let source_arrow_text = diagram_text(&source_arrow_diagram);
    assert!(source_arrow_text.contains('▼'));
    assert!(!source_arrow_text.contains('▲'));
    let source_position = source_arrow_text.find('B').expect("B should render");
    let target_position = source_arrow_text.find('A').expect("A should render");
    assert!(source_position < target_position);
    let bidirectional_text = diagram_text(&bidirectional_diagram);
    assert!(bidirectional_text.contains('▲'));
    assert!(bidirectional_text.contains('▼'));
    assert!(render_mermaid(circle_ends).is_some());
    assert!(render_mermaid(cross_ends).is_some());
    assert!(diagram_text(&labeled_diagram).contains("later"));
}

#[test]
fn test_render_mermaid_fans_source_arrow_chain_out_of_shared_source() {
    // Arrange
    let source = "flowchart TD\n    A <-- B --> C";

    // Act
    let diagram = render_mermaid(source).expect("source arrow chain should render");
    let text = diagram_text(&diagram);

    // Assert
    let source_position = text.find('B').expect("B should render");
    let first_target_position = text.find('A').expect("A should render");
    let second_target_position = text.find('C').expect("C should render");
    assert!(source_position < first_target_position);
    assert!(source_position < second_target_position);
    assert_eq!(text.matches('▼').count(), 2);
    assert!(!text.contains('▲'));
}

#[test]
fn test_render_mermaid_hides_invisible_layout_link() {
    // Arrange
    let source = "flowchart TD\n    A[Source] ~~~ B[Target]";

    // Act
    let diagram = render_mermaid(source).expect("invisible layout link should render");
    let lines: Vec<String> = diagram.lines.iter().map(ToString::to_string).collect();
    let source_row = lines
        .iter()
        .position(|line| line.contains("Source"))
        .expect("source node should render");
    let target_row = lines
        .iter()
        .position(|line| line.contains("Target"))
        .expect("target node should render");

    // Assert
    assert!(source_row + 2 < target_row - 1);
    assert!(
        lines[source_row + 2..target_row - 1]
            .iter()
            .all(|line| line.trim().is_empty())
    );
}

#[test]
fn test_render_mermaid_keeps_line_operator_before_labeled_arrow_chain() {
    // Arrange
    let source = "flowchart TD\n    A --- B --> C";

    // Act
    let diagram = render_mermaid(source).expect("mixed chain should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('B'));
    assert!(text.contains('C'));
    assert_eq!(text.matches('▼').count(), 1);
}

#[test]
fn test_render_mermaid_renders_unspaced_inline_edge_label() {
    // Arrange
    let source = "flowchart TD\n    A[Alpha stage]--send-->B[Beta stage]";

    // Act
    let diagram = render_mermaid(source).expect("unspaced inline label should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("send"));
    assert!(text.contains('▼'));
}

#[test]
fn test_render_mermaid_flattens_subgraph_statements() {
    // Arrange
    let source = concat!(
        "graph TD\n",
        "    subgraph Group\n",
        "    direction LR\n",
        "    A --> B\n",
        "    end\n",
        "    B --> C",
    );

    // Act
    let diagram = render_mermaid(source).expect("flattened subgraph should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('C'));
    assert!(!text.contains("Group"));
}

#[test]
fn test_render_mermaid_skips_styling_statements() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    classDef terminal stroke-width: 1.5px;\n",
        "    A:::terminal --> B\n",
        "    style A fill:#f9f\n",
        "    linkStyle 0 stroke:#f00\n",
        "    class B terminal\n",
        "    click A href \"https://example.com\"",
    );

    // Act
    let diagram = render_mermaid(source).expect("styled flowchart should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('B'));
    assert!(!text.contains("terminal"));
}

#[test]
fn test_render_mermaid_accepts_long_bare_identifier_with_short_label() {
    // Arrange
    let long_identifier = "N".repeat(MAX_LABEL_WIDTH + 1);
    let source = format!("graph TD\n    {long_identifier}[Short] --> B");

    // Act
    let diagram = render_mermaid(&source).expect("labeled node should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Short"));
    assert!(!text.contains(&long_identifier));
}

#[test]
fn test_render_mermaid_skips_comments_and_inline_label_form() {
    // Arrange
    let source = "graph LR\n    %% comment line\n    A -- ok --> B";

    // Act
    let diagram = render_mermaid(source).expect("inline label form should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('▶'));
    assert!(!text.contains("comment"));
}

#[test]
fn test_render_mermaid_renders_dotted_edge_with_embedded_label() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A -.yes.-> C";

    // Act
    let diagram = render_mermaid(source).expect("dotted labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("yes"));
    assert_eq!(text.matches('▼').count(), 2);
}

#[test]
fn test_render_mermaid_renders_spaced_dotted_edge_label_without_arrow() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A -. off .- C";

    // Act
    let diagram = render_mermaid(source).expect("dotted open labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("off"));
    assert_eq!(text.matches('▼').count(), 1);
}

#[test]
fn test_render_mermaid_renders_thick_edge_with_embedded_label() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A ==big==> C";

    // Act
    let diagram = render_mermaid(source).expect("thick labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("big"));
    assert_eq!(text.matches('▼').count(), 2);
}

#[test]
fn test_render_mermaid_keeps_plain_dotted_and_thick_arrows() {
    // Arrange
    let source = "graph TD\n    A -.-> B\n    A ==>|yes| C";

    // Act
    let diagram = render_mermaid(source).expect("plain dotted and thick arrows should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("yes"));
    assert_eq!(text.matches('▼').count(), 2);
}

#[test]
fn test_render_mermaid_renders_graph_mixing_solid_and_dotted_labeled_edges() {
    // Arrange
    let source = concat!(
        "graph TD\n",
        "    T[Turn command] --> C[Auto-commit]\n",
        "    C --> P[Auto-push]\n",
        "    C --> R[Rebase]\n",
        "    P -.race.-> R\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("mixed edge graph should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Turn command"));
    assert!(text.contains("Auto-push"));
    assert!(text.contains("Rebase"));
    assert!(text.contains('▼'));
}

#[test]
fn test_render_mermaid_renders_plain_dotted_and_thick_open_links() {
    // Arrange
    let source = "graph TD\n    A -.- B\n    B === C";

    // Act
    let diagram = render_mermaid(source).expect("open dotted and thick links should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('C'));
    assert!(!text.contains('▼'));
}
