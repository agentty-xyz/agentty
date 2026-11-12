//! Compact execution graph rendering beside the regular chat composer.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    GraphBlockedReason, GraphCompletionGate, GraphDependency, GraphNode, GraphNodeProgress,
    GraphNodeStatus, GraphProposalVersionOutcome,
};
use ag_tui_text::text_util::{
    truncate_spans_with_ellipsis, truncate_with_ellipsis, wrap_lines_breaking_long_words,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::presentation::app_mode::AppMode;
use crate::presentation::execution_graph::{outputs_awaiting_acceptance, takes_revision_guidance};
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::ui::style::palette;

/// Indent of edge rows and wrapped status rows below node names.
const DETAIL_INDENT: &str = "  ";

/// Gap between the node-name column and its status.
const STATUS_GAP: &str = "  ";

/// Inspector shortcut shown below the graph.
const INSPECTOR_HINT: &str = "w: details and actions";

/// Shared painting and scroll geometry. Integer division rounds the sidebar
/// down so it never exceeds 35% of the available width, even when resized.
pub(crate) fn areas(area: Rect, mode: &AppMode, has_graph: bool) -> (Rect, Option<Rect>) {
    let armed = matches!(mode, AppMode::Prompt { slash_state, .. } if slash_state.graph.is_armed());
    if !has_graph && !armed {
        return (area, None);
    }
    let width = area.width / 100 * 35 + area.width % 100 * 35 / 100;
    let [chat, graph] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);

    (chat, Some(graph))
}

/// Renders the proposal status and the node graph with vertical connectors.
/// Oversized graphs collapse to one prioritized row per node.
pub(crate) fn render(frame: &mut Frame, area: Rect, snapshot: Option<&GraphChatSnapshot>) {
    let block = Block::default()
        .title("Execution graph")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::border()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let text = summary(snapshot);
    let graph = snapshot.and_then(launched_nodes);
    let awaited = graph.map_or_default(|(snapshot, _)| awaited_producers(snapshot));
    let mut lines = wrap_lines_breaking_long_words(&text, width);
    if let Some((snapshot, nodes)) = graph {
        lines.push(Line::default());
        lines.extend(graph_lines(snapshot, nodes, &awaited, width));
    }
    if snapshot.is_some() {
        lines.push(Line::default());
        lines.push(Line::styled(INSPECTOR_HINT, muted_style()));
    }
    if lines.len() <= usize::from(inner.height) {
        frame.render_widget(Paragraph::new(lines), inner);

        return;
    }
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(inner);
    let (lines, overflow) = if let Some((snapshot, nodes)) = graph {
        compact_lines(snapshot, nodes, &awaited, &text, body)
    } else {
        (
            lines.into_iter().take(usize::from(body.height)).collect(),
            "More details".into(),
        )
    };
    frame.render_widget(Paragraph::new(lines), body);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(overflow),
            Line::styled(INSPECTOR_HINT, muted_style()),
        ]),
        footer,
    );
}

/// Proposal lifecycle rows shown above the node graph.
fn summary(snapshot: Option<&GraphChatSnapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return "Describe your task in chat to propose an execution graph.\n\nNo nodes run before \
                you approve the exact proposal version."
            .into();
    };
    let proposal = &snapshot.proposal;
    let version = &proposal.version;
    let mut text = format!("Proposal v{}", version.number);
    if let Some(error) = &snapshot.error {
        let _ = write!(text, "\nStatus unavailable (last snapshot): {error}");
    }
    if version.is_cancelled {
        text.push_str("\nCancelled. No graph was launched.");

        return text;
    }
    if let Some(progress) = &snapshot.progress {
        let _ = write!(text, "\nStatus: {:?}", progress.status);
        if let Some((producer, _)) = outputs_awaiting_acceptance(progress).first() {
            let takes_revision = launched_nodes(snapshot).is_some_and(|(_, nodes)| {
                takes_revision_guidance(nodes, progress, &producer.node_id)
            });
            let action = if takes_revision {
                "reply in chat to accept or revise it"
            } else {
                "reply in chat to accept it"
            };
            let _ = write!(
                text,
                "\n{} awaits your approval; {action}",
                producer.node_id
            );
        }
    } else if version.approval.is_some() {
        text.push_str("\nApproved; loading execution status");
    }
    match &version.outcome {
        GraphProposalVersionOutcome::Generating => text.push_str("\nGenerating preview..."),
        GraphProposalVersionOutcome::Failed(error) => {
            let _ = write!(text, "\nGeneration failed: {error}");
        }
        GraphProposalVersionOutcome::Generated { launch, response } => {
            if let GraphProposalOutcome::Clarification { .. } = &response.outcome {
                text.push_str("\nNeeds your input");
            }
            if launch.is_some() && version.approval.is_none() {
                text.push_str("\nAwaiting your approval");
            }
        }
    }

    text
}

/// Returns the resolved nodes of a proposal that is not cancelled.
fn launched_nodes(snapshot: &GraphChatSnapshot) -> Option<(&GraphChatSnapshot, &[GraphNode])> {
    match &snapshot.proposal.version.outcome {
        GraphProposalVersionOutcome::Generated {
            launch: Some(launch),
            ..
        } if !snapshot.proposal.version.is_cancelled => {
            Some((snapshot, launch.graph.nodes.as_slice()))
        }
        _ => None,
    }
}

/// Producers whose output a dependent waits to see accepted, derived once per
/// render and shared by status and priority lookups.
fn awaited_producers(snapshot: &GraphChatSnapshot) -> BTreeSet<&str> {
    snapshot
        .progress
        .iter()
        .flat_map(outputs_awaiting_acceptance)
        .map(|(producer, _)| producer.node_id.as_str())
        .collect()
}

/// Vertical node list in declaration order. Each dependency draws a `│`
/// edge, labeled with its source when it is not the preceding node, and a
/// `▼` into its dependent; unconnected roots are separated by a blank row.
fn graph_lines(
    snapshot: &GraphChatSnapshot,
    nodes: &[GraphNode],
    awaited: &BTreeSet<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let statuses = nodes
        .iter()
        .map(|node| node_status(snapshot, node, awaited))
        .collect::<Vec<_>>();
    let name_width = nodes
        .iter()
        .map(|node| node.id.width())
        .max()
        .unwrap_or_default();
    let status_width = statuses
        .iter()
        .map(|status| status_label_width(*status))
        .max()
        .unwrap_or_default();
    let fits_inline = name_width + STATUS_GAP.width() + status_width <= width;
    let mut lines = Vec::new();
    for (index, (node, status)) in nodes.iter().zip(statuses).enumerate() {
        let previous = index
            .checked_sub(1)
            .map(|previous| nodes[previous].id.as_str());
        if node.dependencies.is_empty() && index > 0 {
            lines.push(Line::default());
        }
        let mut all_satisfied = true;
        for dependency in &node.dependencies {
            let is_satisfied = is_dependency_satisfied(snapshot, node, dependency);
            all_satisfied &= is_satisfied;
            let label = edge_label(dependency, previous);
            let mut edge = if label.is_empty() {
                "│".to_string()
            } else {
                format!("│ {label}")
            };
            if is_satisfied {
                edge.push_str(" ✓");
            }
            lines.push(fitted_line(
                vec![
                    Span::raw(DETAIL_INDENT),
                    Span::styled(edge, edge_style(is_satisfied)),
                ],
                width,
            ));
        }
        if !node.dependencies.is_empty() {
            lines.push(fitted_line(
                vec![
                    Span::raw(DETAIL_INDENT),
                    Span::styled("▼", edge_style(all_satisfied)),
                ],
                width,
            ));
        }
        let name = Span::styled(node.id.clone(), name_style());
        if fits_inline {
            let padding = name_width - node.id.width();
            let mut spans = vec![
                name,
                Span::raw(format!("{}{STATUS_GAP}", " ".repeat(padding))),
            ];
            spans.extend(status_spans(status));
            lines.push(fitted_line(spans, width));
        } else {
            lines.push(fitted_line(vec![name], width));
            let mut spans = vec![Span::raw(DETAIL_INDENT)];
            spans.extend(status_spans(status));
            lines.push(fitted_line(spans, width));
        }
    }

    lines
}

/// Fits one row per node, keeping intervention and active work ahead of
/// settled nodes. Stable sorting retains declaration order within each
/// priority. Long node names are shortened so statuses stay visible.
fn compact_lines(
    snapshot: &GraphChatSnapshot,
    nodes: &[GraphNode],
    awaited: &BTreeSet<&str>,
    text: &str,
    area: Rect,
) -> (Vec<Line<'static>>, String) {
    let width = usize::from(area.width);
    let mut lines = text
        .lines()
        .take(2.min(usize::from(area.height)))
        .map(|line| Line::from(truncate_with_ellipsis(line, width)))
        .collect::<Vec<_>>();
    let capacity = usize::from(area.height).saturating_sub(lines.len());
    let mut ordered = nodes
        .iter()
        .map(|node| (node, node_progress(snapshot, node)))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(node, progress)| node_priority(awaited, node, *progress));
    let shown = &ordered[..capacity.min(ordered.len())];
    let name_column = shown
        .iter()
        .map(|(node, _)| node.id.width())
        .max()
        .unwrap_or_default();
    for (node, _) in shown {
        let status = node_status(snapshot, node, awaited);
        let name_room = width.saturating_sub(status_label_width(status) + STATUS_GAP.width());
        let name = truncate_with_ellipsis(&node.id, name_room);
        let padding = name_column.min(name_room).saturating_sub(name.width());
        let mut spans = vec![
            Span::styled(name, name_style()),
            Span::raw(format!("{}{STATUS_GAP}", " ".repeat(padding))),
        ];
        spans.extend(status_spans(status));
        lines.push(fitted_line(spans, width));
    }
    let hidden = nodes.len().saturating_sub(capacity);
    let overflow = if hidden == 0 {
        "All nodes (compact)".into()
    } else {
        format!("+{hidden} more nodes")
    };

    (lines, overflow)
}

fn node_progress<'a>(
    snapshot: &'a GraphChatSnapshot,
    node: &GraphNode,
) -> Option<&'a GraphNodeProgress> {
    snapshot
        .progress
        .as_ref()
        .and_then(|progress| progress.nodes.iter().find(|state| state.node_id == node.id))
}

/// Sidebar status of one node: its live state, or a pending acceptance of
/// its output by the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeStatus {
    /// A dependent waits for the user to accept this node's output.
    AwaitingApproval,
    /// Live execution state reported by the graph.
    Live(GraphNodeStatus),
}

/// Returns the sidebar status for `node`; nodes of an unapproved or unloaded
/// graph are pending, and a node whose output a dependent waits to see
/// accepted awaits approval.
fn node_status(
    snapshot: &GraphChatSnapshot,
    node: &GraphNode,
    awaited: &BTreeSet<&str>,
) -> NodeStatus {
    if awaited.contains(node.id.as_str()) {
        return NodeStatus::AwaitingApproval;
    }

    NodeStatus::Live(
        node_progress(snapshot, node).map_or(GraphNodeStatus::Pending, |state| state.status),
    )
}

/// Returns the glyph and label spans for one node status.
fn status_spans(status: NodeStatus) -> [Span<'static>; 2] {
    let (glyph, color) = status_glyph(status);
    let style = Style::default().fg(color);

    [
        Span::styled(format!("{glyph} "), style),
        Span::styled(status_label(status), style),
    ]
}

/// Returns the rendered width of one status glyph plus label.
fn status_label_width(status: NodeStatus) -> usize {
    status_glyph(status).0.width() + 1 + status_label(status).width()
}

/// Returns the user-facing label for one node status.
fn status_label(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::AwaitingApproval => "Awaiting your approval",
        NodeStatus::Live(GraphNodeStatus::Pending) => "Waiting",
        NodeStatus::Live(GraphNodeStatus::Queued | GraphNodeStatus::Running) => "Working",
        NodeStatus::Live(GraphNodeStatus::AwaitingAnswers) => "Needs your input",
        NodeStatus::Live(GraphNodeStatus::Succeeded) => "Done",
        NodeStatus::Live(GraphNodeStatus::Failed) => "Failed",
        NodeStatus::Live(GraphNodeStatus::Cancelled) => "Canceled",
    }
}

/// Returns the per-status glyph and semantic color.
fn status_glyph(status: NodeStatus) -> (&'static str, Color) {
    match status {
        NodeStatus::AwaitingApproval => ("●", palette::warning()),
        NodeStatus::Live(GraphNodeStatus::Pending) => ("○", palette::text_muted()),
        NodeStatus::Live(GraphNodeStatus::Queued | GraphNodeStatus::Running) => {
            ("◐", palette::info())
        }
        NodeStatus::Live(GraphNodeStatus::AwaitingAnswers) => ("?", palette::question()),
        NodeStatus::Live(GraphNodeStatus::Succeeded) => ("✓", palette::success()),
        NodeStatus::Live(GraphNodeStatus::Failed) => ("✗", palette::danger()),
        NodeStatus::Live(GraphNodeStatus::Cancelled) => ("⊘", palette::text_subtle()),
    }
}

/// A dependency is satisfied once its source succeeded and the dependent no
/// longer reports it as a blocking gate.
fn is_dependency_satisfied(
    snapshot: &GraphChatSnapshot,
    node: &GraphNode,
    dependency: &GraphDependency,
) -> bool {
    let Some(progress) = snapshot.progress.as_ref() else {
        return false;
    };
    let source_succeeded = progress.nodes.iter().any(|state| {
        state.node_id == dependency.node_id && state.status == GraphNodeStatus::Succeeded
    });

    source_succeeded
        && node_progress(snapshot, node).is_some_and(|state| {
            !state.blocked.iter().any(|reason| {
                matches!(
                    reason,
                    GraphBlockedReason::Dependency { node_id, .. } if *node_id == dependency.node_id
                )
            })
        })
}

/// Names the source unless it is the node drawn directly above, and the
/// output a result gate waits for.
fn edge_label(dependency: &GraphDependency, previous: Option<&str>) -> String {
    let source = (previous != Some(dependency.node_id.as_str()))
        .then(|| format!("from {}", dependency.node_id));
    let gate = match &dependency.gate {
        GraphCompletionGate::ExecutionSucceeded => None,
        GraphCompletionGate::ResultValidated { output_name } => Some(output_name.clone()),
        GraphCompletionGate::ResultAccepted { output_name } => {
            Some(format!("accepted {output_name}"))
        }
    };

    [source, gate]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn node_priority(
    awaited: &BTreeSet<&str>,
    node: &GraphNode,
    progress: Option<&GraphNodeProgress>,
) -> u8 {
    if awaited.contains(node.id.as_str()) {
        return 0;
    }

    match progress {
        Some(node)
            if matches!(
                node.status,
                GraphNodeStatus::AwaitingAnswers | GraphNodeStatus::Failed
            ) =>
        {
            0
        }
        Some(node) if !node.blocked.is_empty() => 1,
        Some(node)
            if matches!(
                node.status,
                GraphNodeStatus::Running | GraphNodeStatus::Queued
            ) =>
        {
            2
        }
        Some(node)
            if matches!(
                node.status,
                GraphNodeStatus::Succeeded | GraphNodeStatus::Cancelled
            ) =>
        {
            4
        }
        _ => 3,
    }
}

fn edge_style(is_satisfied: bool) -> Style {
    if is_satisfied {
        Style::default().fg(palette::success())
    } else {
        muted_style()
    }
}

fn name_style() -> Style {
    Style::default()
        .fg(palette::text())
        .add_modifier(Modifier::BOLD)
}

fn muted_style() -> Style {
    Style::default().fg(palette::text_muted())
}

/// Keeps every graph row on one terminal row so line counts match painting.
fn fitted_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    Line::from(truncate_spans_with_ellipsis(spans, width))
}

#[cfg(test)]
#[path = "graph_chat_test.rs"]
mod tests;
