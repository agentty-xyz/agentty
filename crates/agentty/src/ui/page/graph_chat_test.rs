//! Graph chat sidebar sizing, node status, dependency rail, and overflow
//! rendering unit tests.

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use ag_protocol::{
    GraphProposalOutcome, GraphProposalResponse, ProtocolRequestProfile, QuestionItem,
};
use ag_session::{
    ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate,
    GraphContext, GraphDependency, GraphExecutionRef, GraphNodeProgress, GraphNodeStatus,
    GraphProposalApproval, GraphProposalResolution, GraphProposalVersionOutcome, GraphResult,
    GraphResultRef, GraphResultState, StartExecutionGraphRequest, test_support,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

use super::{areas, graph_status, render, status_glyph, status_label_width, summary};
use crate::domain::agent::ReasoningLevel;
use crate::presentation::app_mode::AppMode;
use crate::presentation::graph_chat::{GraphChatSnapshot, NodeStatus};
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
    assert!(summary_text(None).contains("Describe your task"));
    assert!(summary_text(Some(&snapshot)).contains("Generating preview"));
    let generating = render_rows(Some(&snapshot), 56, 24);
    assert!(generating[0].contains("Execution graph · v1"));
    assert!(!generating[0].contains("done"));
    assert!(render_rows(None, 56, 24)[0].contains("Execution graph──"));
    snapshot.error = Some("offline".into());
    snapshot.proposal.version.outcome = GraphProposalVersionOutcome::Failed("invalid graph".into());
    assert!(summary_text(Some(&snapshot)).contains("Status unavailable (last snapshot): offline"));
    assert!(summary_text(Some(&snapshot)).contains("Generation failed: invalid graph"));
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
    assert!(summary_text(Some(&snapshot)).contains("Cancelled. No graph was launched."));
}

#[test]
fn every_graph_status_has_a_glyph_label_and_semantic_color() {
    // Arrange
    let statuses = [
        (
            ExecutionGraphStatus::Running,
            "◐",
            "Running",
            palette::info(),
        ),
        (
            ExecutionGraphStatus::Blocked,
            "●",
            "Blocked",
            palette::warning(),
        ),
        (
            ExecutionGraphStatus::Paused,
            "‖",
            "Paused",
            palette::text_muted(),
        ),
        (
            ExecutionGraphStatus::Completed,
            "✓",
            "Completed",
            palette::success(),
        ),
        (
            ExecutionGraphStatus::Failed,
            "✗",
            "Failed",
            palette::danger(),
        ),
        (
            ExecutionGraphStatus::Cancelled,
            "⊘",
            "Canceled",
            palette::text_subtle(),
        ),
    ];

    for (status, glyph, label, color) in statuses {
        // Act / Assert
        assert_eq!(graph_status(status, 0), (glyph, label, color));
    }
}

#[test]
fn every_node_status_has_a_styled_label_and_matching_width() {
    // Arrange
    let statuses = [
        (
            NodeStatus::Live(GraphNodeStatus::Pending),
            "○ Waiting",
            palette::text_muted(),
        ),
        (
            NodeStatus::Live(GraphNodeStatus::Queued),
            "◐ Working",
            palette::info(),
        ),
        (
            NodeStatus::Live(GraphNodeStatus::Running),
            "◐ Working",
            palette::info(),
        ),
        (
            NodeStatus::Live(GraphNodeStatus::AwaitingAnswers),
            "? Needs your input",
            palette::question(),
        ),
        (
            NodeStatus::AwaitingApproval,
            "● Awaiting your approval",
            palette::warning(),
        ),
        (
            NodeStatus::Live(GraphNodeStatus::Succeeded),
            "✓ Done",
            palette::success(),
        ),
        (
            NodeStatus::Live(GraphNodeStatus::Failed),
            "✗ Failed",
            palette::danger(),
        ),
        (
            NodeStatus::Live(GraphNodeStatus::Cancelled),
            "⊘ Canceled",
            palette::text_subtle(),
        ),
    ];

    for (status, expected_text, expected_color) in statuses {
        // Act
        let (glyph, color) = status_glyph(status, 0);
        let width = status_label_width(status);

        // Assert
        assert_eq!(format!("{glyph} {}", status.label()), expected_text);
        assert_eq!(width, expected_text.width());
        assert_eq!(color, expected_color);
    }
}

#[test]
fn short_sidebar_hides_models_with_objectives_and_keeps_the_rail() {
    // Arrange
    let mut snapshot = dependency_snapshot();
    snapshot.progress = None;
    let row = |left: &str, right: &str| spread(left, right, 58);

    // Act
    let without_objectives = inner_rows(Some(&snapshot), 60, 16);

    // Assert
    assert_eq!(
        without_objectives,
        [
            "Awaiting your approval",
            "",
            row("○ inspect", "Waiting").as_str(),
            "┊ after inspect",
            "┊",
            row("○ followup", "Waiting").as_str(),
            "┊ needs summary accepted from inspect",
            "┊ needs notes",
            "┊",
            row("○ report", "Waiting").as_str(),
            "",
            row("○ audit", "Waiting").as_str(),
            "",
            "w: details and actions",
        ]
    );
}

#[test]
fn sidebar_draws_dependency_rail_models_objectives_and_live_node_status() {
    // Arrange
    let mut snapshot = dependency_snapshot();
    // A task conversation runs at the rendered session effort (`low`), while
    // fresh utility nodes keep the utility runner's fixed effort (`high`).
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &mut snapshot.proposal.version.outcome
    {
        launch.graph.nodes[0].execution.context = GraphContext::TaskConversation {
            task_id: "task".into(),
        };
    }
    let progress = snapshot.progress.take();
    let row = |left: &str, right: &str| spread(left, right, 58);

    // Act
    let preview = inner_rows(Some(&snapshot), 60, 24);
    snapshot.proposal.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: snapshot.proposal.limits.clone(),
        version: 1,
    });
    let approved = summary_text(Some(&snapshot));
    snapshot.progress = progress;
    let buffer = render_buffer(Some(&snapshot), 60, 26);
    let running = buffer_inner_rows(&buffer);

    // Assert
    assert_eq!(
        preview,
        [
            "Awaiting your approval",
            "",
            row("○ inspect", "Waiting").as_str(),
            "┊ gpt-6.1-sol [low]",
            "┊ Inspect the requested change",
            "┊ after inspect",
            "┊",
            row("○ followup", "Waiting").as_str(),
            "┊ gpt-6.1-sol [high]",
            "┊ Inspect the requested change",
            "┊ needs summary accepted from inspect",
            "┊ needs notes",
            "┊",
            row("○ report", "Waiting").as_str(),
            "  gpt-6.1-sol [high]",
            "  Inspect the requested change",
            "",
            row("○ audit", "Waiting").as_str(),
            "  gpt-6.1-sol [high]",
            "  Inspect the requested change",
            "",
            "w: details and actions",
        ]
    );
    assert!(approved.contains("Approved; loading execution status"));
    assert_eq!(
        running[..21],
        [
            "◐ Running",
            bar(29, 29).as_str(),
            "",
            row("✓ inspect", "Done").as_str(),
            "│ gpt-6.1-sol [low]",
            "│ Inspect the requested change",
            "│ after inspect",
            "│",
            row("◐ followup", "Working").as_str(),
            "┊ gpt-6.1-sol [high]",
            "┊ Inspect the requested change",
            "│ needs summary accepted from inspect",
            "┊ needs notes",
            "┊",
            row("○ report", "Waiting").as_str(),
            "  gpt-6.1-sol [high]",
            "  Inspect the requested change",
            "",
            row("✗ audit", "Failed").as_str(),
            "  gpt-6.1-sol [high]",
            "  Inspect the requested change",
        ]
    );
    let title = (0..60)
        .map(|column| buffer[(column, 0)].symbol())
        .collect::<String>();
    assert!(title.starts_with("┌Execution graph · v1"), "{title}");
    assert!(title.ends_with("1/4 done┐"), "{title}");
    assert_eq!(buffer[(1, 5)].fg, palette::success());
    assert_eq!(buffer[(3, 5)].fg, palette::text_muted());
    assert!(!buffer[(3, 5)].modifier.contains(Modifier::ITALIC));
    assert_eq!(buffer[(1, 6)].fg, palette::success());
    assert_eq!(buffer[(1, 11)].fg, palette::text_muted());
    assert_eq!(buffer[(1, 4)].fg, palette::success());
    assert_eq!(buffer[(3, 7)].fg, palette::success());
    assert_eq!(buffer[(1, 8)].fg, palette::success());
    assert_eq!(buffer[(1, 14)].fg, palette::text_muted());
    assert_eq!(buffer[(1, 9)].fg, palette::info());
    assert!(!buffer[(3, 4)].modifier.contains(Modifier::BOLD));
    assert!(buffer[(3, 9)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(1, 24)].fg, palette::accent());
}

#[test]
fn active_work_rotates_glyphs_and_sweeps_the_progress_bar() {
    // Arrange
    let mut snapshot = dependency_snapshot();
    snapshot.proposal.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: snapshot.proposal.limits.clone(),
        version: 1,
    });
    let highlight_column =
        |buffer: &Buffer| (1..59u16).find(|column| buffer[(*column, 2)].fg == palette::text());

    // Act
    let first = render_frame(Some(&snapshot), 60, 20, 0);
    let second = render_frame(Some(&snapshot), 60, 20, 2);
    let wrapped = render_frame(Some(&snapshot), 60, 20, 30);
    let mut settled = snapshot.clone();
    snapshot.proposal.version.outcome = GraphProposalVersionOutcome::Generating;
    let generating = summary(Some(&snapshot), 2);
    let progress = settled.progress.as_mut().expect("progress");
    progress.status = ExecutionGraphStatus::Completed;
    progress.nodes[1].status = GraphNodeStatus::Succeeded;
    let settled = render_frame(Some(&settled), 60, 20, 2);

    // Assert
    assert_eq!(first[(1, 1)].symbol(), "◐");
    assert_eq!(first[(1, 7)].symbol(), "◐");
    assert_eq!(second[(1, 1)].symbol(), "◓");
    assert_eq!(second[(1, 7)].symbol(), "◓");
    assert_eq!(first[(14, 2)].fg, palette::success());
    assert_eq!(first[(16, 2)].fg, palette::info());
    assert_eq!(first[(30, 2)].fg, palette::text_subtle());
    assert_eq!(highlight_column(&first), Some(15));
    assert_eq!(highlight_column(&second), Some(16));
    assert_eq!(highlight_column(&wrapped), Some(15));
    assert_eq!(
        generating.last().map(|(text, _)| text.as_str()),
        Some("◓ Generating preview...")
    );
    assert_eq!(buffer_inner_rows(&settled)[1], bar(29, 29));
    assert_eq!(highlight_column(&settled), None);
    assert_eq!(settled[(29, 2)].fg, palette::success());
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

    let row = |left: &str, right: &str| spread(left, right, 26);

    // Act
    let rows = inner_rows(Some(&snapshot), 28, 14);

    // Assert
    assert_eq!(
        rows,
        [
            "◐ Running",
            "Awaiting your approval",
            row("✗ node-12", "Failed").as_str(),
            "? nod...  Needs your input",
            row("○ node-13", "Waiting").as_str(),
            row("◐ node-11", "Working").as_str(),
            row("◐ node-14", "Working").as_str(),
            row("○ node-8", "Waiting").as_str(),
            row("○ node-10", "Waiting").as_str(),
            row("✓ node-0", "Done").as_str(),
            "+8 more nodes",
            "w: details and actions",
        ]
    );
}

#[test]
fn awaited_output_shows_approval_on_its_producer_and_prioritizes_it() {
    // Arrange
    let mut snapshot = awaiting_snapshot();
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &mut snapshot.proposal.version.outcome
    {
        launch.graph.nodes[0].execution.context = GraphContext::TaskConversation {
            task_id: "task".into(),
        };
    }

    // Act
    let full = inner_rows(Some(&snapshot), 72, 22);
    let compact = inner_rows(Some(&snapshot), 72, 8);

    // Assert
    let awaiting = spread("● inspect", "Awaiting your approval", 70);
    assert_eq!(
        full[..5],
        [
            "● Blocked",
            "inspect awaits your approval; reply in chat to accept or revise it",
            bar(35, 35).as_str(),
            "",
            awaiting.as_str(),
        ]
    );
    assert_eq!(compact[2], awaiting);
}

#[test]
fn paused_graph_keeps_its_awaited_output_and_offers_to_resume_by_reply() {
    // Arrange
    let mut snapshot = awaiting_snapshot();
    snapshot.progress.as_mut().expect("progress").status = ExecutionGraphStatus::Paused;

    // Act
    let rows = inner_rows(Some(&snapshot), 90, 22);

    // Assert
    assert_eq!(
        rows[..2],
        [
            "‖ Paused",
            "inspect awaits your approval; reply in chat to accept it and resume the graph",
        ]
    );
    assert_eq!(rows[4], spread("● inspect", "Awaiting your approval", 88));
}

#[test]
fn awaited_output_without_a_task_conversation_offers_only_acceptance() {
    // Arrange
    let snapshot = awaiting_snapshot();

    // Act
    let text = summary_text(Some(&snapshot));

    // Assert
    assert!(
        text.ends_with("\ninspect awaits your approval; reply in chat to accept it"),
        "{text}"
    );
}

#[test]
fn awaited_output_a_started_dependent_consumed_offers_only_acceptance() {
    // Arrange
    let mut snapshot = awaiting_snapshot();
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &mut snapshot.proposal.version.outcome
    {
        launch.graph.nodes[0].execution.context = GraphContext::TaskConversation {
            task_id: "task".into(),
        };
    }
    snapshot.progress.as_mut().expect("progress").nodes[1].executions =
        vec![GraphExecutionRef::TaskAttempt {
            attempt_id: "followup-attempt".into(),
            operation_id: "operation".into(),
            task_id: "followup-task".into(),
        }];

    // Act
    let text = summary_text(Some(&snapshot));

    // Assert
    assert!(
        text.ends_with("\ninspect awaits your approval; reply in chat to accept it"),
        "{text}"
    );
}

/// Blocked dependency graph whose `report` node waits for acceptance of the
/// fresh `inspect` node's validated `summary` output.
fn awaiting_snapshot() -> GraphChatSnapshot {
    let mut snapshot = dependency_snapshot();
    snapshot.proposal.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: snapshot.proposal.limits.clone(),
        version: 1,
    });
    let progress = snapshot.progress.as_mut().expect("progress");
    progress.status = ExecutionGraphStatus::Blocked;
    progress.nodes[0].results = vec![GraphResult {
        reference: GraphResultRef {
            execution: GraphExecutionRef::TaskAttempt {
                attempt_id: "attempt".into(),
                operation_id: "operation".into(),
                task_id: "task".into(),
            },
            graph_id: "graph".into(),
            graph_revision: NonZeroU64::MIN,
            id: "result".into(),
            node_id: "inspect".into(),
            output_name: "summary".into(),
            schema: ProtocolRequestProfile::SessionTurn,
            version: NonZeroU64::MIN,
        },
        state: GraphResultState::Validated,
    }];
    progress.nodes[2]
        .blocked
        .push(GraphBlockedReason::Dependency {
            gate: GraphCompletionGate::ResultAccepted {
                output_name: "summary".into(),
            },
            node_id: "inspect".into(),
        });

    snapshot
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
    let full = inner_rows(Some(&snapshot), 28, 17);
    let compact = inner_rows(Some(&snapshot), 28, 8);

    // Assert
    let wrapped_status = spread("", "Waiting", 26);
    assert_eq!(full[2], bar(0, 26));
    for name_row in [4, 9] {
        assert!(full[name_row].starts_with("○ 界"), "{full:#?}");
        assert_eq!(full[name_row + 1], wrapped_status);
        assert_eq!(full[name_row + 2], "  gpt-6.1-sol [high]");
        assert!(full[name_row + 3].starts_with("  Inspect"));
    }
    assert_eq!(full[8], "");
    assert_eq!(full[14], "w: details and actions");
    for row in &compact[2..4] {
        assert!(row.starts_with("○ 界"), "{compact:#?}");
        assert!(row.ends_with("  Waiting"));
    }
    assert_eq!(compact[4], "All nodes (compact)");
    assert_eq!(compact[5], "w: details and actions");
}

#[test]
fn long_objectives_wrap_to_a_capped_width_and_shrink_before_hiding() {
    // Arrange
    let mut snapshot = graph_snapshot(2);
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &mut snapshot.proposal.version.outcome
    {
        for node in &mut launch.graph.nodes {
            node.objective = ["alpha"; 30].join(" ");
        }
        launch.graph.nodes[1].dependencies = vec![dependency(
            "node-0",
            GraphCompletionGate::ResultAccepted {
                output_name: "plan".into(),
            },
        )];
    }
    let progress = snapshot.progress.as_mut().expect("progress");
    progress.nodes[0].status = GraphNodeStatus::Running;
    progress.nodes[1].status = GraphNodeStatus::Pending;
    snapshot.proposal.version.approval = Some(GraphProposalApproval {
        graph_id: "graph".into(),
        limits: snapshot.proposal.limits.clone(),
        version: 1,
    });
    let wrapped_row = "alpha alpha alpha alpha";

    // Act
    let tall = render_buffer(Some(&snapshot), 30, 25);
    let wrapped = buffer_inner_rows(&tall);
    let one_row = inner_rows(Some(&snapshot), 30, 15);
    let hidden = inner_rows(Some(&snapshot), 30, 11);

    // Assert
    assert_eq!(wrapped[3], spread("◐ node-0", "Working", 28));
    let rail_row = format!("┊ {wrapped_row}");
    assert_eq!(
        wrapped[4..13],
        [
            "┊ gpt-6.1-sol [high]",
            rail_row.as_str(),
            rail_row.as_str(),
            rail_row.as_str(),
            rail_row.as_str(),
            rail_row.as_str(),
            "┊ alpha...",
            "┊ needs plan accepted",
            "┊",
        ],
        "{wrapped:#?}"
    );
    assert_eq!(wrapped[13], spread("○ node-1", "Waiting", 28));
    assert_eq!(wrapped[14], "  gpt-6.1-sol [high]");
    assert_eq!(wrapped[15], format!("  {wrapped_row}"));
    assert_eq!(wrapped[20], "  alpha...");
    assert_eq!(tall[(3, 6)].fg, palette::text_muted());
    assert!(!tall[(3, 6)].modifier.contains(Modifier::ITALIC));
    assert_eq!(tall[(3, 12)].fg, palette::accent());
    assert!(tall[(3, 12)].modifier.contains(Modifier::ITALIC));
    assert_eq!(one_row[4], "┊ gpt-6.1-sol [high]");
    assert_eq!(one_row[5], format!("┊ {wrapped_row}..."));
    assert_eq!(one_row[6], "┊ needs plan accepted");
    assert_eq!(one_row[7], "┊");
    assert_eq!(one_row[9], "  gpt-6.1-sol [high]");
    assert_eq!(one_row[10], format!("  {wrapped_row}..."));
    assert_eq!(hidden[4], "┊ needs plan accepted");
    assert_eq!(hidden[5], "┊");
    assert_eq!(hidden[8], "w: details and actions");
    assert!(!hidden.join("\n").contains("gpt-6.1-sol"), "{hidden:#?}");
}

#[test]
fn model_rows_shorten_to_fit_narrow_sidebars() {
    // Arrange
    let snapshot = graph_snapshot(1);

    // Act
    let rows = inner_rows(Some(&snapshot), 14, 30);

    // Assert
    assert!(rows.iter().any(|row| row == "  gpt-6.1..."), "{rows:#?}");
}

#[test]
fn sidebar_keeps_full_details_when_they_fit_and_handles_non_node_overflow() {
    // Arrange
    let mut snapshot = graph_snapshot(1);

    // Act / Assert
    let rows = inner_rows(Some(&snapshot), 80, 24);
    assert!(rows.contains(&spread("✓ node-0", "Done", 78)));
    assert!(
        rows.iter()
            .any(|row| row == "  Inspect the requested change")
    );
    assert_eq!(rows[21], "w: details and actions");
    assert!(!rows.join("\n").contains("compact"));
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

/// Joins summary rows into plain text for content assertions.
fn summary_text(snapshot: Option<&GraphChatSnapshot>) -> String {
    summary(snapshot, 0)
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Places `left` and `right` at opposite edges of a `width`-column row, as
/// node rows render their right-aligned status.
fn spread(left: &str, right: &str, width: usize) -> String {
    format!(
        "{left}{}{right}",
        " ".repeat(width - left.width() - right.width())
    )
}

fn render_buffer(snapshot: Option<&GraphChatSnapshot>, width: u16, height: u16) -> Buffer {
    render_frame(snapshot, width, height, 0)
}

/// Renders the sidebar at one animation `spinner_frame`.
fn render_frame(
    snapshot: Option<&GraphChatSnapshot>,
    width: u16,
    height: u16,
    spinner_frame: usize,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            render(
                frame,
                frame.area(),
                snapshot,
                ReasoningLevel::Low,
                spinner_frame,
            );
        })
        .expect("draw");

    terminal.backend().buffer().clone()
}

/// Progress bar text with `filled` finished or active cells followed by
/// `track` remaining cells.
fn bar(filled: usize, track: usize) -> String {
    format!("{}{}", "━".repeat(filled), "─".repeat(track))
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
