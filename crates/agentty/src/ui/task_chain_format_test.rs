use ag_session::BuiltinAgentDefinition;
use ratatui::text::Line;

use crate::presentation::task_chain_graph::{
    ACCEPTED_PLAN_EDGE_LABEL, TaskChainGraph, TaskChainGraphEdge, TaskChainGraphNode,
    TaskChainNodeStatus,
};
use crate::ui::style;
use crate::ui::task_chain_format::{agent_answer_header_line, graph_card_lines};

#[test]
fn agent_answer_header_names_the_role_with_accent() {
    // Arrange
    let agent = BuiltinAgentDefinition::Implementer;

    // Act
    let header = agent_answer_header_line(agent, 40);

    // Assert
    assert_eq!(header.to_string(), "◆ Implementer");
    assert_eq!(header.spans[1].style.fg, Some(style::palette::accent()));
    assert_eq!(agent_answer_header_line(agent, 6).width(), 6);
}

#[test]
fn graph_card_aligns_statuses_and_marks_the_satisfied_edge() {
    // Arrange
    let graph = graph(
        TaskChainNodeStatus::Done,
        TaskChainNodeStatus::Working,
        true,
    );

    // Act
    let lines = graph_card_lines(Some(&graph), 60);

    // Assert
    assert_eq!(
        texts(&lines),
        [
            "⬡ Execution graph",
            "  Planner      ✓ Done",
            "    │ accepted plan ✓",
            "    ▼",
            "  Implementer  ◐ Working",
        ]
    );
    assert_eq!(lines[2].spans[1].style.fg, Some(style::palette::success()));
}

#[test]
fn graph_card_moves_statuses_below_nodes_at_narrow_widths() {
    // Arrange
    let graph = graph(
        TaskChainNodeStatus::AwaitingApproval,
        TaskChainNodeStatus::Waiting,
        false,
    );

    // Act
    let lines = graph_card_lines(Some(&graph), 30);
    let tiny_lines = graph_card_lines(Some(&graph), 12);

    // Assert
    assert_eq!(
        texts(&lines),
        [
            "⬡ Execution graph",
            "  Planner",
            "    ● Awaiting your approval",
            "    │ accepted plan",
            "    ▼",
            "  Implementer",
            "    ○ Waiting",
        ]
    );
    assert_eq!(
        lines[3].spans[1].style.fg,
        Some(style::palette::text_muted())
    );
    assert_eq!(tiny_lines.len(), lines.len());
    assert!(tiny_lines.iter().all(|line| line.width() <= 12));
}

#[test]
fn graph_card_uses_distinct_glyphs_for_every_status() {
    // Arrange
    let statuses = [
        (TaskChainNodeStatus::NeedsInput, "?"),
        (TaskChainNodeStatus::Failed, "✗"),
        (TaskChainNodeStatus::Canceled, "⊘"),
    ];

    for (status, glyph) in statuses {
        // Act
        let lines = graph_card_lines(
            Some(&graph(status, TaskChainNodeStatus::Waiting, false)),
            80,
        );

        // Assert
        assert!(
            lines[1]
                .to_string()
                .ends_with(&format!("{glyph} {}", status.label())),
            "{status:?}"
        );
    }
}

#[test]
fn graph_card_shows_placeholder_until_the_snapshot_loads() {
    // Arrange, Act
    let lines = graph_card_lines(None, 40);

    // Assert
    assert_eq!(texts(&lines), ["⬡ Execution graph", "  Loading status…"]);
}

fn graph(
    planner: TaskChainNodeStatus,
    implementer: TaskChainNodeStatus,
    is_satisfied: bool,
) -> TaskChainGraph {
    TaskChainGraph {
        chain_id: "chain".into(),
        edges: vec![TaskChainGraphEdge {
            is_satisfied,
            label: ACCEPTED_PLAN_EDGE_LABEL,
        }],
        nodes: vec![
            TaskChainGraphNode {
                agent: BuiltinAgentDefinition::Planner,
                status: planner,
            },
            TaskChainGraphNode {
                agent: BuiltinAgentDefinition::Implementer,
                status: implementer,
            },
        ],
    }
}

fn texts(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(ToString::to_string).collect()
}
