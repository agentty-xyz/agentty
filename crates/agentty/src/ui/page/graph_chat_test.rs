use std::collections::BTreeMap;

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse, QuestionItem};
use ag_session::{
    ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate,
    GraphDependency, GraphNodeProgress, GraphNodeStatus, GraphProposalApproval,
    GraphProposalResolution, GraphProposalVersionOutcome, StartExecutionGraphRequest, test_support,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use super::{areas, render, status_label_width, status_spans, summary};
use crate::presentation::app_mode::AppMode;
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::ui::style::palette;

#[test]
fn sidebar_never_exceeds_thirty_five_percent_and_normal_chat_keeps_full_width() {
    // Arrange
    let mode = AppMode::List;
    for width in [0, 1, 2, 40, 79, 80, 120, 161, u16::MAX] {
        let area = Rect::new(0, 0, width, 30);

        // Act
        let (chat, sidebar) = areas(area, &mode, true);
        let sidebar = sidebar.expect("sidebar");

        // Assert
        assert_eq!(u32::from(sidebar.width), u32::from(width) * 35 / 100);
        assert_eq!(chat.width + sidebar.width, width);
        assert_eq!(sidebar.x, chat.right());
        assert_eq!(areas(area, &mode, false), (area, None));
    }
}

#[test]
fn sidebar_shows_proposal_lifecycle_and_stale_status() {
    // Arrange
    let mut snapshot = GraphChatSnapshot {
        error: None,
        progress: None,
        proposal: test_support::graph_proposal_state("proposal", "session"),
    };

    // Act / Assert
    assert!(summary(None).contains("Describe your task"));
    assert!(summary(Some(&snapshot)).contains("Generating preview"));
    snapshot.error = Some("offline".into());
    snapshot.proposal.version.outcome = GraphProposalVersionOutcome::Failed("invalid graph".into());
    assert!(summary(Some(&snapshot)).contains("Status unavailable (last snapshot): offline"));
    assert!(summary(Some(&snapshot)).contains("Generation failed: invalid graph"));
    snapshot.proposal.version.outcome = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Clarification {
                questions: vec![QuestionItem::new("Which target?")],
            },
        },
    };
    let sidebar = render_rows(Some(&snapshot), 56, 24).join("\n");
    assert!(sidebar.contains("Needs your input"));
    assert!(!sidebar.contains("Which target?"));
    snapshot.proposal.version.is_cancelled = true;
    assert!(summary(Some(&snapshot)).contains("Cancelled. No graph was launched."));
}

#[test]
fn every_node_status_has_a_styled_label_and_matching_width() {
    // Arrange
    let statuses = [
        (GraphNodeStatus::Pending, "○ Waiting", palette::text_muted()),
        (GraphNodeStatus::Queued, "◐ Working", palette::info()),
        (GraphNodeStatus::Running, "◐ Working", palette::info()),
        (
            GraphNodeStatus::AwaitingAnswers,
            "? Needs your input",
            palette::question(),
        ),
        (GraphNodeStatus::Succeeded, "✓ Done", palette::success()),
        (GraphNodeStatus::Failed, "✗ Failed", palette::danger()),
        (
            GraphNodeStatus::Cancelled,
            "⊘ Canceled",
            palette::text_subtle(),
        ),
    ];

    for (status, expected_text, expected_color) in statuses {
        // Act
        let spans = status_spans(status);
        let width = status_label_width(status);

        // Assert
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(text, expected_text);
        assert_eq!(width, expected_text.width());
        assert!(
            spans
                .iter()
                .all(|span| span.style.fg == Some(expected_color))
        );
    }
}

#[test]
fn sidebar_draws_dependency_connectors_and_live_node_status() {
    // Arrange
    let mut snapshot = dependency_snapshot();
    let progress = snapshot.progress.take();

    // Act
    let preview = inner_rows(Some(&snapshot), 60, 20);
    snapshot.proposal.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: snapshot.proposal.limits.clone(),
        version: 1,
    });
    let approved = summary(Some(&snapshot));
    snapshot.progress = progress;
    let buffer = render_buffer(Some(&snapshot), 60, 20);
    let running = buffer_inner_rows(&buffer);

    // Assert
    assert_eq!(
        preview[..15],
        [
            "Proposal v1",
            "Awaiting your approval",
            "",
            "inspect   ○ Waiting",
            "  │",
            "  ▼",
            "followup  ○ Waiting",
            "  │ from inspect · accepted summary",
            "  │ notes",
            "  ▼",
            "report    ○ Waiting",
            "",
            "audit     ○ Waiting",
            "",
            "w: details and actions",
        ]
    );
    assert!(approved.contains("Approved; loading execution status"));
    assert_eq!(
        running[..15],
        [
            "Proposal v1",
            "Status: Running",
            "",
            "inspect   ✓ Done",
            "  │ ✓",
            "  ▼",
            "followup  ◐ Working",
            "  │ from inspect · accepted summary ✓",
            "  │ notes",
            "  ▼",
            "report    ○ Waiting",
            "",
            "audit     ✗ Failed",
            "",
            "w: details and actions",
        ]
    );
    assert_eq!(buffer[(3, 5)].fg, palette::success());
    assert_eq!(buffer[(3, 10)].fg, palette::text_muted());
}

#[test]
fn overflowing_sidebar_prioritizes_intervention_and_reserves_controls() {
    // Arrange
    let mut snapshot = graph_snapshot(16);
    let progress = snapshot.progress.as_mut().expect("progress");
    progress.nodes[15].status = GraphNodeStatus::AwaitingAnswers;
    progress.nodes[14].status = GraphNodeStatus::Running;
    progress.nodes[13].status = GraphNodeStatus::Pending;
    progress.nodes[13]
        .blocked
        .push(GraphBlockedReason::ResumeRequired);
    progress.nodes[12].status = GraphNodeStatus::Failed;
    progress.nodes[11].status = GraphNodeStatus::Queued;
    progress.nodes[10].status = GraphNodeStatus::Pending;
    progress.nodes[9].status = GraphNodeStatus::Cancelled;
    progress.nodes.remove(8);

    // Act
    let rows = inner_rows(Some(&snapshot), 28, 14);

    // Assert
    assert_eq!(
        rows,
        [
            "Proposal v1",
            "Status: Running",
            "node-12  ✗ Failed",
            "nod...  ? Needs your input",
            "node-13  ○ Waiting",
            "node-11  ◐ Working",
            "node-14  ◐ Working",
            "node-8   ○ Waiting",
            "node-10  ○ Waiting",
            "node-0   ✓ Done",
            "+8 more nodes",
            "w: details and actions",
        ]
    );
}

#[test]
fn long_node_names_wrap_status_or_shorten_without_displacing_controls() {
    // Arrange
    let mut snapshot = graph_snapshot(2);
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &mut snapshot.proposal.version.outcome
    {
        for (index, node) in launch.graph.nodes.iter_mut().enumerate() {
            node.id = format!("{}{index}", "界".repeat(100));
        }
    }
    if let Some(progress) = &mut snapshot.progress {
        progress.nodes.clear();
    }

    // Act
    let full = inner_rows(Some(&snapshot), 28, 13);
    let compact = inner_rows(Some(&snapshot), 28, 8);

    // Assert
    assert!(full[4].starts_with('界'));
    assert_eq!(full[5], "  ○ Waiting");
    assert_eq!(full[6], "");
    assert_eq!(full[8], "  ○ Waiting");
    assert_eq!(full[10], "w: details and actions");
    assert!(compact[2].starts_with('界'));
    assert!(compact[2].ends_with("○ Waiting"));
    assert!(compact[3].ends_with("○ Waiting"));
    assert_eq!(compact[4], "All nodes (compact)");
    assert_eq!(compact[5], "w: details and actions");
}

#[test]
fn sidebar_keeps_full_details_when_they_fit_and_handles_non_node_overflow() {
    // Arrange
    let mut snapshot = graph_snapshot(1);

    // Act / Assert
    let text = inner_rows(Some(&snapshot), 80, 24).join("\n");
    assert!(text.contains("node-0  ✓ Done"));
    assert!(text.contains("w: details and actions"));
    assert!(!text.contains("compact"));
    snapshot.proposal.version.is_cancelled = true;
    snapshot.error = Some("long error ".repeat(50));
    let rows = inner_rows(Some(&snapshot), 28, 12);
    assert_eq!(rows[8], "More details");
    assert_eq!(rows[9], "w: details and actions");
    assert!(!rows.join("\n").contains("node-0"));
    snapshot.proposal.version.outcome = GraphProposalVersionOutcome::Generating;
    let rows = inner_rows(Some(&snapshot), 28, 12);
    assert_eq!(rows[8], "More details");
    let rows = inner_rows(None, 28, 6);
    assert_eq!(rows[2], "More details");
    assert_eq!(rows[3], "w: details and actions");
    assert!(
        !inner_rows(None, 80, 24)
            .join("\n")
            .contains("w: details and actions")
    );
}

#[test]
fn sidebar_tolerates_resizing_to_tiny_areas() {
    // Arrange
    let snapshot = graph_snapshot(16);

    // Act / Assert
    for (width, height) in [(0, 0), (1, 1), (12, 3), (20, 5)] {
        let rows = render_rows(Some(&snapshot), width, height);
        assert_eq!(rows.len(), usize::from(height));
    }
}

fn render_buffer(snapshot: Option<&GraphChatSnapshot>, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), snapshot))
        .expect("draw");

    terminal.backend().buffer().clone()
}

fn render_rows(snapshot: Option<&GraphChatSnapshot>, width: u16, height: u16) -> Vec<String> {
    let buffer = render_buffer(snapshot, width, height);

    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}

/// Rows inside the sidebar border with trailing padding removed.
fn inner_rows(snapshot: Option<&GraphChatSnapshot>, width: u16, height: u16) -> Vec<String> {
    buffer_inner_rows(&render_buffer(snapshot, width, height))
}

fn buffer_inner_rows(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;

    (1..area.height.saturating_sub(1))
        .map(|row| {
            (1..area.width.saturating_sub(1))
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Four nodes: a chain, a join with accepted and validated result gates, and
/// a root.
fn dependency_snapshot() -> GraphChatSnapshot {
    let mut snapshot = graph_snapshot(4);
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &mut snapshot.proposal.version.outcome
    {
        for (node, id) in launch
            .graph
            .nodes
            .iter_mut()
            .zip(["inspect", "followup", "report", "audit"])
        {
            node.id = id.into();
        }
        launch.graph.nodes[1].dependencies = vec![dependency(
            "inspect",
            GraphCompletionGate::ExecutionSucceeded,
        )];
        launch.graph.nodes[2].dependencies = vec![
            dependency(
                "inspect",
                GraphCompletionGate::ResultAccepted {
                    output_name: "summary".into(),
                },
            ),
            dependency(
                "followup",
                GraphCompletionGate::ResultValidated {
                    output_name: "notes".into(),
                },
            ),
        ];
    }
    let progress = snapshot.progress.as_mut().expect("progress");
    for (node, id) in progress
        .nodes
        .iter_mut()
        .zip(["inspect", "followup", "report", "audit"])
    {
        node.node_id = id.into();
    }
    progress.nodes[1].status = GraphNodeStatus::Running;
    progress.nodes[2].status = GraphNodeStatus::Pending;
    progress.nodes[2]
        .blocked
        .push(GraphBlockedReason::Dependency {
            gate: GraphCompletionGate::ExecutionSucceeded,
            node_id: "followup".into(),
        });
    progress.nodes[3].status = GraphNodeStatus::Failed;

    snapshot
}

fn dependency(node_id: &str, gate: GraphCompletionGate) -> GraphDependency {
    GraphDependency {
        gate,
        node_id: node_id.into(),
    }
}

fn graph_snapshot(node_count: usize) -> GraphChatSnapshot {
    let mut proposal = test_support::graph_proposal_state("proposal", "session");
    let declaration = test_support::graph_proposal();
    let mut graph = GraphProposalResolution {
        agent: proposal.agent,
        answers: &proposal.version.answers,
        definitions: &proposal.definitions,
        graph_id: "graph",
        limits: &proposal.limits,
        permission: proposal.permission,
        request: &proposal.input.request,
        revision_attachments: &proposal.version.attachments,
        session_id: &proposal.input.session_id,
    }
    .resolve(&declaration)
    .expect("resolved graph");
    graph.nodes = (0..node_count)
        .map(|index| {
            let mut node = graph.nodes[0].clone();
            node.id = format!("node-{index}");
            node
        })
        .collect();
    let progress = ExecutionGraphProgress {
        graph_id: graph.id.clone(),
        revision: graph.revision,
        session_id: "session".into(),
        status: ExecutionGraphStatus::Running,
        nodes: graph
            .nodes
            .iter()
            .map(|node| GraphNodeProgress {
                blocked: Vec::new(),
                executions: Vec::new(),
                last_error: None,
                node_id: node.id.clone(),
                questions: Vec::new(),
                results: Vec::new(),
                status: GraphNodeStatus::Succeeded,
            })
            .collect(),
    };
    proposal.version.outcome = GraphProposalVersionOutcome::Generated {
        launch: Some(Box::new(StartExecutionGraphRequest {
            graph,
            provider_call_limits: BTreeMap::default(),
        })),
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Proposal {
                proposal: declaration,
            },
        },
    };

    GraphChatSnapshot {
        error: None,
        progress: Some(progress),
        proposal,
    }
}
