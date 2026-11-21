//! Mermaid graph layout tests: node placement, edge routing, and labels.

use std::fmt::Write;

use super::support::{diagram_text, rendered_text};
use crate::mermaid::{
    LAYER_GAP_COLUMNS, expand_long_edges, layout_layers, node_width, parse_graph, render_mermaid,
};

/// Chain into a three-way labelled fan-out that merges back into one node.
const FAN_OUT_AND_MERGE: &str = concat!(
    "flowchart TD\n",
    "    A[Op::Compact] --> B[abort tasks, spawn CompactTask]\n",
    "    B --> C[path]\n",
    "    C -->|TokenBudget| D[fresh context window]\n",
    "    C --> E[append CompactionTrigger]\n",
    "    C -->|unsupported| F[local summary turn]\n",
    "    D --> G[install replacement history]\n",
    "    E --> G\n",
    "    F --> G\n",
);

/// Decision flow whose `D --> G` edge spans three layers.
const LONG_EDGE_FLOW: &str = concat!(
    "flowchart TD\n",
    "    A[User starts session] --> B{Choose action}\n",
    "    B -->|Ask agent| C[Send prompt]\n",
    "    B -->|Review changes| D[Open diff view]\n",
    "    C --> E[Agent works in worktree]\n",
    "    E --> F[Run checks]\n",
    "    F --> G[Report result]\n",
    "    D --> G\n",
);

/// Returns the widest layer of a top-down graph packed with the full gap
/// between all of its nodes.
fn widest_packed_layer(source: &str) -> usize {
    let graph = expand_long_edges(parse_graph(source).expect("graph should parse"))
        .expect("long edges should expand");
    let node_layers = layout_layers(&graph).expect("graph should be layered");
    let layer_count = node_layers.iter().max().map_or(0, |layer| layer + 1);

    (0..layer_count)
        .map(|layer| {
            let widths: Vec<usize> = graph
                .nodes
                .iter()
                .zip(&node_layers)
                .filter(|(_, node_layer)| **node_layer == layer)
                .map(|(node, _)| node_width(node))
                .collect();

            widths.iter().sum::<usize>() + LAYER_GAP_COLUMNS * widths.len().saturating_sub(1)
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn test_render_mermaid_draws_labelled_fan_out_that_merges_back() {
    // Arrange
    let expected = [
        "                                 ┌─────────────┐",
        "                                 │ Op::Compact │",
        "                                 └──────┬──────┘",
        "                                        │",
        "                                        ▼",
        "                        ┌────────────────────────────────┐",
        "                        │ abort tasks, spawn CompactTask │",
        "                        └───────────────┬────────────────┘",
        "                                        │",
        "                                        ▼",
        "                                     ┌──────┐",
        "                                     │ path │",
        "                                     └┬─┬─┬─┘",
        "           ┌───────TokenBudget────────┘ │ └───────unsupported───────┐",
        "           ▼                            ▼                           ▼",
        "┌──────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐",
        "│ fresh context window │   │ append CompactionTrigger │   │ local summary turn │",
        "└──────────┬───────────┘   └────────────┬─────────────┘   └─────────┬──────────┘",
        "           └──────────────────┐         │         ┌─────────────────┘",
        "                              ▼         ▼         ▼",
        "                         ┌─────────────────────────────┐",
        "                         │ install replacement history │",
        "                         └─────────────────────────────┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(FAN_OUT_AND_MERGE);

    // Assert
    assert_eq!(text, expected);
    assert!(!text.contains('┼'));
}

#[test]
fn test_render_mermaid_keeps_chain_of_mixed_box_widths_in_one_column() {
    // Arrange
    let source = concat!(
        "graph TD\n",
        "    A[Op::Compact] --> B[abort tasks, spawn CompactTask] --> C[path] --> D[done!]",
    );

    // Act
    let text = rendered_text(source);
    let connector_columns: Vec<usize> = text
        .lines()
        .filter(|line| !line.contains(['┌', '└']) && !line.contains(char::is_alphabetic))
        .map(|line| {
            assert!(matches!(line.trim(), "│" | "▼"), "{line:?}");

            line.chars().count() - 1
        })
        .collect();

    // Assert
    assert_eq!(connector_columns, [16; 6]);
    assert_eq!(text.matches('┬').count(), 3);
}

#[test]
fn test_render_mermaid_reorders_nodes_to_remove_crossing() {
    // Arrange
    let source = "graph TD\n    A --> X\n    A --> Y\n    B --> X";
    let expected = [
        "┌───┐   ┌───┐",
        "│ A │   │ B │",
        "└┬─┬┘   └──┬┘",
        " │ └─────┐ │",
        " ▼       ▼ ▼",
        "┌───┐   ┌───┐",
        "│ Y │   │ X │",
        "└───┘   └───┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_draws_unavoidable_crossing_in_one_cell() {
    // Arrange
    let source = "graph TD\n    A --> C\n    A --> D\n    B --> C\n    B --> D";
    let expected = [
        "┌───┐   ┌───┐",
        "│ A │   │ B │",
        "└┬─┬┘   └┬─┬┘",
        " │┌┼─────┘ │",
        " ││└─────┐ │",
        " ▼▼      ▼ ▼",
        "┌───┐   ┌───┐",
        "│ C │   │ D │",
        "└───┘   └───┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_keeps_long_edge_in_one_column() {
    // Arrange
    let expected = [
        "     ┌─────────────────────┐",
        "     │ User starts session │",
        "     └──────────┬──────────┘",
        "                │",
        "                ▼",
        "        ┌───────────────┐",
        "        │ Choose action │",
        "        └───┬───────┬───┘",
        "  Ask agent │       │ Review changes",
        "            ▼       ▼",
        "┌─────────────┐   ┌────────────────┐",
        "│ Send prompt │   │ Open diff view │",
        "└──────┬──────┘   └───────────┬────┘",
        "       │                      │",
        "       ▼                      │",
        "┌─────────────────────────┐   │",
        "│ Agent works in worktree │   │",
        "└────────────┬────────────┘   │",
        "             │                │",
        "             ▼                │",
        "       ┌────────────┐         │",
        "       │ Run checks │         │",
        "       └─────────┬──┘         │",
        "                 │       ┌────┘",
        "                 ▼       ▼",
        "             ┌───────────────┐",
        "             │ Report result │",
        "             └───────────────┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(LONG_EDGE_FLOW);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_runs_parallel_long_edges_side_by_side() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A[Parse and bound graph] --> B[Split feedback edges]\n",
        "    B --> C[Longest-path layers]\n",
        "    C --> D[New - order layers]\n",
        "    A --> D\n",
        "    A --> E[Paint]\n",
        "    D --> E\n",
        "    B --> E\n",
    );
    let expected = [
        "        ┌───────────────────────┐",
        "        │ Parse and bound graph │",
        "        └───┬───────────────┬─┬─┘",
        "            │               │ │",
        "            ▼               │ │",
        " ┌──────────────────────┐   │ │",
        " │ Split feedback edges │   │ │",
        " └─────────┬─────┬──────┘   │ │",
        "           │     └────────┐ │ │",
        "           ▼              │ │ │",
        "┌─────────────────────┐   │ │ │",
        "│ Longest-path layers │   │ │ │",
        "└─────┬───────────────┘   │ │ │",
        "      │         ┌─────────┼─┘ │",
        "      ▼         ▼         │   │",
        " ┌────────────────────┐   │   │",
        " │ New - order layers │   │   │",
        " └─────────┬──────────┘   │   │",
        "           └────────────┐ │ ┌─┘",
        "                        ▼ ▼ ▼",
        "                      ┌───────┐",
        "                      │ Paint │",
        "                      └───────┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_branches_shared_port_edges_off_one_track() {
    // Arrange
    let fan_out = "graph TD\n    A -->|a| B\n    A -->|b| C\n    A -->|c| D\n    A -->|d| E";
    let fan_in = "graph TD\n    B --> D\n    C --> D\n    A --> D\n    E --> D";
    let expected_fan_out = [
        "            ┌───┐",
        "            │ A │",
        "            └─┬─┘",
        "  ┌───────┬───┴───┬───────┐",
        "  ▼ a     ▼ b     ▼ c     ▼ d",
        "┌───┐   ┌───┐   ┌───┐   ┌───┐",
        "│ B │   │ C │   │ D │   │ E │",
        "└───┘   └───┘   └───┘   └───┘",
    ]
    .join("\n");
    let expected_fan_in = [
        "┌───┐   ┌───┐   ┌───┐   ┌───┐",
        "│ B │   │ C │   │ A │   │ E │",
        "└─┬─┘   └─┬─┘   └─┬─┘   └─┬─┘",
        "  └───────┴───┬───┴───────┘",
        "              ▼",
        "            ┌───┐",
        "            │ D │",
        "            └───┘",
    ]
    .join("\n");

    // Act
    let fan_out_text = rendered_text(fan_out);
    let fan_in_text = rendered_text(fan_in);

    // Assert
    assert_eq!(fan_out_text, expected_fan_out);
    assert_eq!(fan_in_text, expected_fan_in);
}

#[test]
fn test_render_mermaid_er_fan_out_keeps_every_marker() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    CUSTOMER ||--o{ ORDER : places\n",
        "    CUSTOMER ||--|| ACCOUNT : owns\n",
    );
    let expected = [
        "     ┌──────────┐",
        "     │ CUSTOMER │",
        "     └──┬───┬───┘",
        "        1   1",
        "    ┌───┘   └────┐",
        "    * places     1 owns",
        "┌───┴───┐   ┌────┴────┐",
        "│ ORDER │   │ ACCOUNT │",
        "└───────┘   └─────────┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_attaches_arrows_and_open_links_to_borders() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A{{User starts session}} --> B{Choose action}\n",
        "    B -->|Route request| C[Send prompt] & D[Open diff view]\n",
        "    C <--> E[Agent works in worktree]\n",
        "    E --> F[Run checks]\n",
        "    F <-- G[Report result]\n",
        "    D --> G\n",
    );
    let expected = [
        "              ┌─────────────────────┐",
        "              │ User starts session │",
        "              └──────────┬──────────┘",
        "                         │",
        "                         ▼",
        "                 ┌───────────────┐",
        "                 │ Choose action │",
        "                 └───┬───────┬───┘",
        "             ┌───────┘       └───────┐",
        "             ▼ Route request         ▼",
        "      ┌─────────────┐        ┌────────────────┐",
        "      │ Send prompt │        │ Open diff view │",
        "      └─────────────┘        └────────┬───────┘",
        "             ▲                        │",
        "             ▼                        ▼",
        "┌─────────────────────────┐   ┌───────────────┐",
        "│ Agent works in worktree │   │ Report result │",
        "└─────────────────────┬───┘   └───────┬───────┘",
        "                      │     ┌─────────┘",
        "                      ▼     ▼",
        "                   ┌────────────┐",
        "                   │ Run checks │",
        "                   └────────────┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_routes_left_right_fan_out_and_labels() {
    // Arrange
    let fan_out = "graph LR\n    A --> B\n    A --> C\n    A --> D\n    A --> E";
    let labelled_fan_in = "graph LR\n    A -->|x| C\n    B -->|y| C";
    let labelled_long_edge =
        "graph LR\n    A[Start here] -->|go| B[Middle] -->|done| C[End]\n    A -->|skip| C";
    let expected_fan_out = [
        "         ┌───┐",
        "      ┌─▶│ B │",
        "      │  └───┘",
        "      │",
        "      │  ┌───┐",
        "      ├─▶│ C │",
        "┌───┐ │  └───┘",
        "│ A ├─┤",
        "└───┘ │  ┌───┐",
        "      ├─▶│ D │",
        "      │  └───┘",
        "      │",
        "      │  ┌───┐",
        "      └─▶│ E │",
        "         └───┘",
    ]
    .join("\n");
    let expected_labelled_fan_in = [
        "┌───┐",
        "│ A ├─x─┐",
        "└───┘   │  ┌───┐",
        "        ├─▶│ C │",
        "┌───┐   │  └───┘",
        "│ B ├─y─┘",
        "└───┘",
    ]
    .join("\n");
    let expected_labelled_long_edge = [
        "                       ┌────────┐",
        "┌────────────┐ ┌──go──▶│ Middle ├─done─┐  ┌─────┐",
        "│ Start here ├─┤       └────────┘      ├─▶│ End │",
        "└────────────┘ │                       │  └─────┘",
        "               └─skip──────────────────┘",
    ]
    .join("\n");

    // Act
    let fan_out_text = rendered_text(fan_out);
    let labelled_fan_in_text = rendered_text(labelled_fan_in);
    let labelled_long_edge_text = rendered_text(labelled_long_edge);

    // Assert
    assert_eq!(fan_out_text, expected_fan_out);
    assert_eq!(labelled_fan_in_text, expected_labelled_fan_in);
    assert_eq!(labelled_long_edge_text, expected_labelled_long_edge);
}

#[test]
fn test_render_mermaid_keeps_swapped_left_right_edges_on_shared_ports() {
    // Arrange
    let source = "graph LR\n    A --> C\n    A --> D\n    B --> C\n    B --> D";
    let expected = [
        "┌───┐     ┌───┐",
        "│ A ├─┬┬─▶│ C │",
        "└───┘ ││  └───┘",
        "      ││",
        "┌───┐ ││  ┌───┐",
        "│ B ├─┴┴─▶│ D │",
        "└───┘     └───┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_lays_out_unconnected_nodes() {
    // Arrange
    let top_down = "graph TD\n    A[Alpha]\n    B[Bravo]\n    C";
    let left_right = "graph LR\n    A[Alpha]\n    B[Bravo]\n    C";
    let expected_top_down = [
        "┌───────┐   ┌───────┐   ┌───┐",
        "│ Alpha │   │ Bravo │   │ C │",
        "└───────┘   └───────┘   └───┘",
    ]
    .join("\n");
    let expected_left_right = [
        "┌───────┐",
        "│ Alpha │",
        "└───────┘",
        "",
        "┌───────┐",
        "│ Bravo │",
        "└───────┘",
        "",
        "┌───────┐",
        "│   C   │",
        "└───────┘",
    ]
    .join("\n");

    // Act
    let top_down_text = rendered_text(top_down);
    let left_right_text = rendered_text(left_right);

    // Assert
    assert_eq!(top_down_text, expected_top_down);
    assert_eq!(left_right_text, expected_left_right);
}

#[test]
fn test_render_mermaid_leaves_invisible_link_without_port() {
    // Arrange
    let source = "graph TD\n    A[Source] ~~~ B[Target]\n    A --> C";
    let expected = [
        "     ┌────────┐",
        "     │ Source │",
        "     └───┬────┘",
        "         └─────┐",
        "               ▼",
        "┌────────┐   ┌───┐",
        "│ Target │   │ C │",
        "└────────┘   └───┘",
    ]
    .join("\n");

    // Act
    let text = rendered_text(source);

    // Assert
    assert_eq!(text, expected);
}

#[test]
fn test_render_mermaid_is_deterministic() {
    // Arrange
    let sources = [FAN_OUT_AND_MERGE, LONG_EDGE_FLOW];

    for source in sources {
        // Act
        let first = render_mermaid(source).expect("diagram should render");
        let second = render_mermaid(source).expect("diagram should render again");

        // Assert
        assert_eq!(first.lines, second.lines);
        assert_eq!(first.width, second.width);
    }
}

#[test]
fn test_render_mermaid_stays_within_widest_packed_layer() {
    // Arrange
    let sources = [
        FAN_OUT_AND_MERGE,
        LONG_EDGE_FLOW,
        "graph TD\n    A[Start] --> B[Finish]",
        "graph TD\n    A --> B\n    A --> C\n    B --> D\n    C --> D",
        "graph TD\n    A --> B --> C\n    A --> C\n    A --> D\n    D --> C",
        "graph TD\n    A[Alpha stage] -->|a label that is far too long| B",
        concat!(
            "graph TD\n",
            "    T[Turn command] --> C[Auto-commit]\n",
            "    C --> P[Auto-push]\n",
            "    C --> R[Rebase]\n",
            "    P -.race.-> R\n",
        ),
    ];

    for source in sources {
        // Act
        let diagram = render_mermaid(source).expect("diagram should render");

        // Assert
        assert!(diagram.width <= widest_packed_layer(source), "{source}");
    }
}

#[test]
fn test_render_mermaid_renders_graph_at_node_and_edge_limits() {
    // Arrange
    let mut source = String::from("graph TD");
    for layer in 0..3 {
        for column in 0..4 {
            let from = layer * 4 + column;
            write!(&mut source, "\n    N{from} --> N{}", from + 4)
                .expect("writing to String should succeed");
        }
        for column in 0..3 {
            let from = layer * 4 + column;
            write!(&mut source, "\n    N{from} --> N{}", from + 5)
                .expect("writing to String should succeed");
        }
    }
    source.push_str("\n    N1 --> N4\n    N2 --> N5\n    N3 --> N6");

    // Act
    let diagram = render_mermaid(&source).expect("graph at the limits should render");
    let text = diagram_text(&diagram);

    // Assert
    assert_eq!(source.matches("-->").count(), 24);
    for node_index in 0..16 {
        assert!(text.contains(&format!("N{node_index} ")), "N{node_index}");
    }
}

#[test]
fn test_render_mermaid_omits_label_without_free_place() {
    // Arrange
    let source = "graph TD\n    A[Alpha stage] -->|a label that is far too long| B";

    // Act
    let text = rendered_text(source);

    // Assert
    assert!(!text.contains("label"));
    assert!(text.contains('▼'));
}
