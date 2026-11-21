//! Compact execution graph rendering beside the regular chat composer.

use std::collections::BTreeSet;

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate,
    GraphDependency, GraphNode, GraphNodeProgress, GraphNodeStatus, GraphProposalVersionOutcome,
};
use ag_tui_text::text_util::{
    inline_text, truncate_spans_with_ellipsis, truncate_with_ellipsis,
    wrap_lines_breaking_long_words,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::domain::agent::ReasoningLevel;
use crate::presentation::app_mode::AppMode;
use crate::presentation::execution_graph::{
    node_reasoning_level, outputs_awaiting_acceptance, takes_revision_guidance,
};
use crate::presentation::graph_chat::GraphChatSnapshot;
use crate::ui::help_format;
use crate::ui::style::palette;

/// Indent of objective rows below node names, aligned past the status glyph.
const DETAIL_INDENT: &str = "  ";

/// Display-width cap of a wrapped objective, keeping it to a few rows.
const OBJECTIVE_MAX_WIDTH: usize = 128;

/// Minimum gap between a node name and its right-aligned status.
const STATUS_GAP: usize = 2;

/// Rail drawn for a satisfied dependency.
const SATISFIED_RAIL: &str = "│";

/// Rail drawn for a dependency that still blocks its dependent.
const PENDING_RAIL: &str = "┊";

/// Inspector shortcut label shown after its key at the bottom of the panel.
const INSPECTOR_LABEL: &str = ": details and actions";

/// Rotating phases of the glyph marking active work. The first phase is the
/// static working glyph, so frame zero matches the settled rendering.
const ACTIVE_GLYPHS: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// Spinner frames per animation phase, so each phase stays visible at the
/// runtime's forced redraw cadence.
const FRAMES_PER_PHASE: usize = 2;

/// Progress bar cell for finished and active nodes.
const BAR_FILLED: &str = "━";

/// Progress bar cell for nodes that have not started or did not succeed.
const BAR_TRACK: &str = "─";

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

/// Renders the proposal status and a timeline of nodes whose status glyphs
/// sit on a dependency rail. A muted `model [effort]` row follows each node
/// name; task-conversation nodes show the session's `reasoning_level`, and
/// fresh utility nodes their fixed level. When space runs out, wrapped
/// objectives first shrink to one row and are then dropped together with the
/// model rows; oversized graphs then collapse to one prioritized row per node.
/// The inspector shortcut stays pinned to the bottom row. Active work
/// animates at `spinner_frame`: working glyphs rotate, and a progress bar
/// below the status sweeps a highlight through the nodes in flight.
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    snapshot: Option<&GraphChatSnapshot>,
    reasoning_level: ReasoningLevel,
    spinner_frame: usize,
) {
    let block = panel_block(snapshot);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let header = summary(snapshot, spinner_frame);
    let graph = snapshot.and_then(launched_nodes);
    let awaited = graph.map_or_default(|(snapshot, _)| awaited_producers(snapshot));
    let reserved_rows = if snapshot.is_some() { 2 } else { 0 };
    let room = usize::from(inner.height).saturating_sub(reserved_rows);
    let mut header_lines = wrap_summary(&header, width);
    if let Some((snapshot, nodes)) = graph
        && let Some(progress) = &snapshot.progress
    {
        header_lines.extend(progress_bar(progress, nodes.len(), width, spinner_frame));
    }
    let detail_levels: &[ObjectiveDetail] = if graph.is_some() {
        &[
            ObjectiveDetail::Wrapped,
            ObjectiveDetail::OneRow,
            ObjectiveDetail::Hidden,
        ]
    } else {
        &[ObjectiveDetail::Hidden]
    };
    let fitting = detail_levels.iter().find_map(|&objective_detail| {
        let mut lines = header_lines.clone();
        if let Some((snapshot, nodes)) = graph {
            lines.push(Line::default());
            lines.extend(graph_lines(
                snapshot,
                nodes,
                &awaited,
                width,
                objective_detail,
                reasoning_level,
                spinner_frame,
            ));
        }

        (lines.len() <= room).then_some(lines)
    });
    if let Some(lines) = fitting {
        let [body, hint] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        if snapshot.is_some() {
            frame.render_widget(Paragraph::new(lines), body);
            frame.render_widget(Paragraph::new(inspector_hint()), hint);
        } else {
            frame.render_widget(Paragraph::new(lines), inner);
        }

        return;
    }
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(inner);
    let (lines, overflow) = if let Some((snapshot, nodes)) = graph {
        compact_lines(snapshot, nodes, &awaited, &header, body, spinner_frame)
    } else {
        (
            header_lines
                .into_iter()
                .take(usize::from(body.height))
                .collect(),
            "More details".into(),
        )
    };
    frame.render_widget(Paragraph::new(lines), body);
    frame.render_widget(
        Paragraph::new(vec![Line::from(overflow), inspector_hint()]),
        footer,
    );
}

/// Bordered panel titled with the proposal version and, once a graph is
/// resolved, a right-aligned count of finished nodes.
fn panel_block(snapshot: Option<&GraphChatSnapshot>) -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::border()));
    let Some(snapshot) = snapshot else {
        return block.title("Execution graph");
    };
    let block = block.title(format!(
        "Execution graph · v{}",
        snapshot.proposal.version.number
    ));
    let Some((_, nodes)) = launched_nodes(snapshot) else {
        return block;
    };
    let done = snapshot
        .progress
        .iter()
        .flat_map(|progress| &progress.nodes)
        .filter(|state| state.status == GraphNodeStatus::Succeeded)
        .count();

    block.title_top(
        Line::styled(format!("{done}/{} done", nodes.len()), muted_style()).right_aligned(),
    )
}

/// Proposal lifecycle rows shown above the node graph, each with one style.
/// Running graphs and generating previews lead with a rotating glyph.
fn summary(snapshot: Option<&GraphChatSnapshot>, spinner_frame: usize) -> Vec<(String, Style)> {
    let Some(snapshot) = snapshot else {
        return vec![
            (
                "Describe your task in chat to propose an execution graph.".into(),
                Style::default(),
            ),
            (String::new(), Style::default()),
            (
                "No nodes run before you approve the exact proposal version.".into(),
                muted_style(),
            ),
        ];
    };
    let version = &snapshot.proposal.version;
    let mut rows = Vec::new();
    if let Some(error) = &snapshot.error {
        rows.push((
            format!("Status unavailable (last snapshot): {error}"),
            color_style(palette::warning()),
        ));
    }
    if version.is_cancelled {
        rows.push(("Cancelled. No graph was launched.".into(), muted_style()));

        return rows;
    }
    if let Some(progress) = &snapshot.progress {
        let (glyph, label, color) = graph_status(progress.status, spinner_frame);
        rows.push((format!("{glyph} {label}"), color_style(color)));
        if let Some((producer, _)) = outputs_awaiting_acceptance(progress).first() {
            let takes_revision = launched_nodes(snapshot).is_some_and(|(_, nodes)| {
                takes_revision_guidance(nodes, progress, &producer.node_id)
            });
            let action = if takes_revision {
                "reply in chat to accept or revise it"
            } else {
                "reply in chat to accept it"
            };
            // A reply to a paused graph resumes it before acting.
            let resume = if progress.status == ExecutionGraphStatus::Paused {
                " and resume the graph"
            } else {
                ""
            };
            rows.push((
                format!(
                    "{} awaits your approval; {action}{resume}",
                    producer.node_id
                ),
                color_style(palette::warning()),
            ));
        }
    } else if version.approval.is_some() {
        rows.push(("Approved; loading execution status".into(), muted_style()));
    }
    match &version.outcome {
        GraphProposalVersionOutcome::Generating => {
            rows.push((
                format!("{} Generating preview...", active_glyph(spinner_frame)),
                muted_style(),
            ));
        }
        GraphProposalVersionOutcome::Failed(error) => {
            rows.push((
                format!("Generation failed: {error}"),
                color_style(palette::danger()),
            ));
        }
        GraphProposalVersionOutcome::Generated { launch, response } => {
            if let GraphProposalOutcome::Clarification { .. } = &response.outcome {
                rows.push(("Needs your input".into(), color_style(palette::question())));
            }
            if launch.is_some() && version.approval.is_none() {
                rows.push((
                    "Awaiting your approval".into(),
                    color_style(palette::warning()),
                ));
            }
        }
    }

    rows
}

/// Returns the glyph, label, and semantic color of the overall graph status;
/// a running graph's glyph rotates with `spinner_frame`.
fn graph_status(
    status: ExecutionGraphStatus,
    spinner_frame: usize,
) -> (&'static str, &'static str, Color) {
    match status {
        ExecutionGraphStatus::Running => (active_glyph(spinner_frame), "Running", palette::info()),
        ExecutionGraphStatus::Blocked => ("●", "Blocked", palette::warning()),
        ExecutionGraphStatus::Paused => ("‖", "Paused", palette::text_muted()),
        ExecutionGraphStatus::Completed => ("✓", "Completed", palette::success()),
        ExecutionGraphStatus::Failed => ("✗", "Failed", palette::danger()),
        ExecutionGraphStatus::Cancelled => ("⊘", "Canceled", palette::text_subtle()),
    }
}

/// Returns the rotating active-work glyph for one spinner frame.
fn active_glyph(spinner_frame: usize) -> &'static str {
    ACTIVE_GLYPHS[spinner_frame / FRAMES_PER_PHASE % ACTIVE_GLYPHS.len()]
}

/// Full-width bar of finished nodes in the success color, then active nodes
/// in the info color with a highlight sweeping through them, then a muted
/// track for the rest. Active nodes always claim at least one cell.
fn progress_bar(
    progress: &ExecutionGraphProgress,
    node_count: usize,
    width: usize,
    spinner_frame: usize,
) -> Option<Line<'static>> {
    if node_count == 0 || width == 0 {
        return None;
    }
    let count = |matches: fn(GraphNodeStatus) -> bool| {
        progress
            .nodes
            .iter()
            .filter(|state| matches(state.status))
            .count()
            .min(node_count)
    };
    let done = count(|status| status == GraphNodeStatus::Succeeded);
    let active =
        count(|status| matches!(status, GraphNodeStatus::Queued | GraphNodeStatus::Running));
    let done_cells = done * width / node_count;
    let active_cells = if active == 0 {
        0
    } else {
        ((done + active).min(node_count) * width / node_count)
            .saturating_sub(done_cells)
            .max(1)
            .min(width - done_cells)
    };
    let highlight = (spinner_frame / FRAMES_PER_PHASE)
        .checked_rem(active_cells)
        .map(|offset| done_cells + offset);
    let spans = (0..width)
        .map(|cell| {
            if cell < done_cells {
                Span::styled(BAR_FILLED, color_style(palette::success()))
            } else if Some(cell) == highlight {
                Span::styled(BAR_FILLED, color_style(palette::text()))
            } else if cell < done_cells + active_cells {
                Span::styled(BAR_FILLED, color_style(palette::info()))
            } else {
                Span::styled(BAR_TRACK, color_style(palette::text_subtle()))
            }
        })
        .collect::<Vec<_>>();

    Some(Line::from(spans))
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

/// Word-wraps styled summary rows to the panel width.
fn wrap_summary(rows: &[(String, Style)], width: usize) -> Vec<Line<'static>> {
    rows.iter()
        .flat_map(|(text, style)| {
            wrap_lines_breaking_long_words(text, width)
                .into_iter()
                .map(|line| Line::styled(line.to_string(), *style))
        })
        .collect()
}

/// How much of each node objective the timeline shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectiveDetail {
    /// Word-wrapped beside the rail, capped at [`OBJECTIVE_MAX_WIDTH`].
    Wrapped,
    /// One row, shortened with an ellipsis.
    OneRow,
    /// Not shown.
    Hidden,
}

/// Vertical node timeline in declaration order. Each node row leads with its
/// status glyph, and the rail column below it carries one highlighted label
/// row per dependency of the next node: solid once satisfied, dotted while it
/// still blocks. A bare rail row sets those labels apart from the next node.
/// Each node's model and `reasoning_level`, then its objective at
/// `objective_detail`, follow its name beside the rail; both are omitted when
/// objectives are hidden. Unconnected roots are separated by a blank row.
fn graph_lines(
    snapshot: &GraphChatSnapshot,
    nodes: &[GraphNode],
    awaited: &BTreeSet<&str>,
    width: usize,
    objective_detail: ObjectiveDetail,
    reasoning_level: ReasoningLevel,
    spinner_frame: usize,
) -> Vec<Line<'static>> {
    let satisfied = nodes
        .iter()
        .map(|node| {
            node.dependencies
                .iter()
                .map(|dependency| is_dependency_satisfied(snapshot, node, dependency))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .map(|previous| nodes[previous].id.as_str());
        if node.dependencies.is_empty() && index > 0 {
            lines.push(Line::default());
        }
        for (dependency, is_satisfied) in node.dependencies.iter().zip(&satisfied[index]) {
            lines.push(fitted_line(
                vec![
                    rail(*is_satisfied),
                    Span::raw(" "),
                    Span::styled(
                        edge_label(dependency, previous),
                        edge_label_style(*is_satisfied),
                    ),
                ],
                width,
            ));
        }
        if !node.dependencies.is_empty() {
            let is_satisfied = satisfied[index].iter().all(|is_satisfied| *is_satisfied);
            lines.push(Line::from(rail(is_satisfied)));
        }
        lines.extend(node_lines(
            node,
            node_status(snapshot, node, awaited),
            width,
            spinner_frame,
        ));
        if objective_detail == ObjectiveDetail::Hidden {
            continue;
        }
        let prefix = match satisfied.get(index + 1) {
            Some(next) if !next.is_empty() => {
                vec![
                    rail(next.iter().all(|is_satisfied| *is_satisfied)),
                    Span::raw(" "),
                ]
            }
            _ => vec![Span::raw(DETAIL_INDENT)],
        };
        lines.push(model_line(node, reasoning_level, &prefix, width));
        lines.extend(objective_lines(
            &node.objective,
            &prefix,
            width,
            objective_detail,
        ));
    }

    lines
}

/// Muted `model [effort]` row after `prefix`, shortened to `width`.
fn model_line(
    node: &GraphNode,
    reasoning_level: ReasoningLevel,
    prefix: &[Span<'static>],
    width: usize,
) -> Line<'static> {
    let mut spans = prefix.to_vec();
    spans.push(Span::styled(
        format!(
            "{} [{}]",
            node.agent.model().as_str(),
            node_reasoning_level(node, reasoning_level).as_str()
        ),
        muted_style(),
    ));

    fitted_line(spans, width)
}

/// Objective rows after `prefix`: one shortened row, or word-wrapped rows of
/// at most [`OBJECTIVE_MAX_WIDTH`] columns in total.
fn objective_lines(
    objective: &str,
    prefix: &[Span<'static>],
    width: usize,
    objective_detail: ObjectiveDetail,
) -> Vec<Line<'static>> {
    let objective = inline_text(objective);
    let row = |text: String| {
        let mut spans = prefix.to_vec();
        spans.push(Span::styled(text, muted_style()));

        fitted_line(spans, width)
    };
    if objective_detail == ObjectiveDetail::OneRow {
        return vec![row(objective)];
    }
    let objective = truncate_with_ellipsis(&objective, OBJECTIVE_MAX_WIDTH);
    let room = width.saturating_sub(DETAIL_INDENT.width());

    wrap_lines_breaking_long_words(&objective, room)
        .into_iter()
        .map(|line| row(line.to_string()))
        .collect()
}

/// Draws one rail cell, solid and green once its dependency is satisfied.
fn rail(is_satisfied: bool) -> Span<'static> {
    if is_satisfied {
        Span::styled(SATISFIED_RAIL, color_style(palette::success()))
    } else {
        Span::styled(PENDING_RAIL, muted_style())
    }
}

/// Italic dependency labels stand apart from muted objectives: accent while
/// the dependency blocks, green once satisfied.
fn edge_label_style(is_satisfied: bool) -> Style {
    let color = if is_satisfied {
        palette::success()
    } else {
        palette::accent()
    };

    color_style(color).add_modifier(Modifier::ITALIC)
}

/// Names what a dependency waits for, omitting the source of an output gate
/// when it is the node drawn directly above.
fn edge_label(dependency: &GraphDependency, previous: Option<&str>) -> String {
    let source = dependency.node_id.as_str();
    let from = if previous == Some(source) {
        String::new()
    } else {
        format!(" from {source}")
    };

    match &dependency.gate {
        GraphCompletionGate::ExecutionSucceeded => format!("after {source}"),
        GraphCompletionGate::ResultValidated { output_name } => {
            format!("needs {output_name}{from}")
        }
        GraphCompletionGate::ResultAccepted { output_name } => {
            format!("needs {output_name} accepted{from}")
        }
    }
}

/// One node row with a right-aligned status, or the name and status on two
/// rows when both do not fit.
fn node_lines(
    node: &GraphNode,
    status: NodeStatus,
    width: usize,
    spinner_frame: usize,
) -> Vec<Line<'static>> {
    if node.id.width() + STATUS_GAP + status_label_width(status) <= width {
        return vec![node_row(node.id.clone(), status, width, spinner_frame)];
    }
    let (glyph, color) = status_glyph(status, spinner_frame);
    let label = status_label(status);
    let padding = width
        .saturating_sub(label.width())
        .max(DETAIL_INDENT.width());

    vec![
        fitted_line(
            vec![
                Span::styled(format!("{glyph} "), color_style(color)),
                Span::styled(node.id.clone(), name_style(status)),
            ],
            width,
        ),
        fitted_line(
            vec![
                Span::raw(" ".repeat(padding)),
                Span::styled(label, color_style(color)),
            ],
            width,
        ),
    ]
}

/// Fits one row per node, keeping intervention and active work ahead of
/// settled nodes. Stable sorting retains declaration order within each
/// priority. Long node names are shortened so statuses stay visible.
fn compact_lines(
    snapshot: &GraphChatSnapshot,
    nodes: &[GraphNode],
    awaited: &BTreeSet<&str>,
    header: &[(String, Style)],
    area: Rect,
    spinner_frame: usize,
) -> (Vec<Line<'static>>, String) {
    let width = usize::from(area.width);
    let mut lines = header
        .iter()
        .take(2.min(usize::from(area.height)))
        .map(|(text, style)| Line::styled(truncate_with_ellipsis(text, width), *style))
        .collect::<Vec<_>>();
    let capacity = usize::from(area.height).saturating_sub(lines.len());
    let mut ordered = nodes
        .iter()
        .map(|node| (node, node_progress(snapshot, node)))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(node, progress)| node_priority(awaited, node, *progress));
    for (node, _) in &ordered[..capacity.min(ordered.len())] {
        let status = node_status(snapshot, node, awaited);
        let name_room = width.saturating_sub(status_label_width(status) + STATUS_GAP);
        lines.push(node_row(
            truncate_with_ellipsis(&node.id, name_room),
            status,
            width,
            spinner_frame,
        ));
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

/// Status glyph and node name on the left with the status label
/// right-aligned, keeping at least [`STATUS_GAP`] columns between them.
fn node_row(name: String, status: NodeStatus, width: usize, spinner_frame: usize) -> Line<'static> {
    let (glyph, color) = status_glyph(status, spinner_frame);
    let label = status_label(status);
    let padding = width
        .saturating_sub(name.width() + status_label_width(status))
        .max(STATUS_GAP);
    let name_style = name_style(status);

    fitted_line(
        vec![
            Span::styled(format!("{glyph} "), color_style(color)),
            Span::styled(name, name_style),
            Span::raw(" ".repeat(padding)),
            Span::styled(label, color_style(color)),
        ],
        width,
    )
}

/// Returns the width of one status glyph, its separating space, and label.
/// Every animation phase of a glyph has the same width, so the first frame
/// measures all of them.
fn status_label_width(status: NodeStatus) -> usize {
    status_glyph(status, 0).0.width() + 1 + status_label(status).width()
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

/// Returns the per-status glyph and semantic color; working nodes' glyphs
/// rotate with `spinner_frame`.
fn status_glyph(status: NodeStatus, spinner_frame: usize) -> (&'static str, Color) {
    match status {
        NodeStatus::AwaitingApproval => ("●", palette::warning()),
        NodeStatus::Live(GraphNodeStatus::Pending) => ("○", palette::text_muted()),
        NodeStatus::Live(GraphNodeStatus::Queued | GraphNodeStatus::Running) => {
            (active_glyph(spinner_frame), palette::info())
        }
        NodeStatus::Live(GraphNodeStatus::AwaitingAnswers) => ("?", palette::question()),
        NodeStatus::Live(GraphNodeStatus::Succeeded) => ("✓", palette::success()),
        NodeStatus::Live(GraphNodeStatus::Failed) => ("✗", palette::danger()),
        NodeStatus::Live(GraphNodeStatus::Cancelled) => ("⊘", palette::text_subtle()),
    }
}

/// Bold names mark nodes that are working or need the user; settled and
/// waiting nodes stay regular weight.
fn name_style(status: NodeStatus) -> Style {
    let style = color_style(palette::text());
    let needs_attention = matches!(
        status,
        NodeStatus::AwaitingApproval
            | NodeStatus::Live(
                GraphNodeStatus::Queued
                    | GraphNodeStatus::Running
                    | GraphNodeStatus::AwaitingAnswers
                    | GraphNodeStatus::Failed
            )
    );
    if needs_attention {
        return style.add_modifier(Modifier::BOLD);
    }

    style
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

/// Inspector shortcut with the key emphasized like other footer hints.
fn inspector_hint() -> Line<'static> {
    Line::from(vec![
        help_format::footer_key_span("w"),
        help_format::footer_muted_span(INSPECTOR_LABEL),
    ])
}

fn color_style(color: Color) -> Style {
    Style::default().fg(color)
}

fn muted_style() -> Style {
    color_style(palette::text_muted())
}

/// Keeps every graph row on one terminal row so line counts match painting.
fn fitted_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    Line::from(truncate_spans_with_ellipsis(spans, width))
}

#[cfg(test)]
#[path = "graph_chat_test.rs"]
mod tests;
