//! Compact execution graph rendering beside the regular chat composer.

use std::collections::BTreeSet;

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    ExecutionGraphProgress, ExecutionGraphStatus, GraphNode, GraphNodePlacement, GraphNodeProgress,
    GraphNodeStatus, GraphProposalVersionOutcome,
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
use crate::presentation::graph_chat::{
    AWAITING_APPROVAL_LABEL, CANCELLED_LABEL, CHILD_INDENT, ChildrenProgress, GENERATING_LABEL,
    GraphChatSnapshot, NEEDS_INPUT_LABEL, NodeStatus, STARTING_LABEL, approval_request, edge_label,
    generation_failed_label, graph_status_label, is_dependency_satisfied, launched_graph_nodes,
};
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
        rows.push((CANCELLED_LABEL.into(), muted_style()));

        return rows;
    }
    if let Some(progress) = &snapshot.progress {
        let (glyph, label, color) = graph_status(progress.status, spinner_frame);
        rows.push((format!("{glyph} {label}"), color_style(color)));
        if let Some((producer, _)) = outputs_awaiting_acceptance(progress).first() {
            let takes_revision = launched_nodes(snapshot).is_some_and(|(_, nodes)| {
                takes_revision_guidance(nodes, progress, &producer.node_id)
            });
            rows.push((
                approval_request(
                    &producer.node_id,
                    takes_revision,
                    progress.status == ExecutionGraphStatus::Paused,
                ),
                color_style(palette::warning()),
            ));
        }
    } else if version.approval.is_some() {
        rows.push((STARTING_LABEL.into(), muted_style()));
    }
    match &version.outcome {
        GraphProposalVersionOutcome::Generating => {
            rows.push((
                format!("{} {GENERATING_LABEL}", active_glyph(spinner_frame)),
                muted_style(),
            ));
        }
        GraphProposalVersionOutcome::Failed(error) => {
            rows.push((
                generation_failed_label(error),
                color_style(palette::danger()),
            ));
        }
        GraphProposalVersionOutcome::Generated { launch, response } => {
            if let GraphProposalOutcome::Clarification { .. } = &response.outcome {
                rows.push((NEEDS_INPUT_LABEL.into(), color_style(palette::question())));
            }
            if launch.is_some() && version.approval.is_none() {
                rows.push((
                    AWAITING_APPROVAL_LABEL.into(),
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
    let (glyph, color) = match status {
        ExecutionGraphStatus::Running => (active_glyph(spinner_frame), palette::info()),
        ExecutionGraphStatus::Blocked => ("●", palette::warning()),
        ExecutionGraphStatus::Paused => ("‖", palette::text_muted()),
        ExecutionGraphStatus::Completed => ("✓", palette::success()),
        ExecutionGraphStatus::Failed => ("✗", palette::danger()),
        ExecutionGraphStatus::Cancelled => ("⊘", palette::text_subtle()),
    };

    (glyph, graph_status_label(status), color)
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
    let nodes = launched_graph_nodes(&snapshot.proposal);

    (!nodes.is_empty()).then_some((snapshot, nodes))
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

/// Vertical node timeline with each top-level node followed by its children,
/// indented, in declaration order. Each node row leads with its status
/// glyph, and the rail column below it carries one highlighted label row per
/// dependency of the next node: solid once satisfied, dotted while it still
/// blocks. A bare rail row sets those labels apart from the next node. A
/// parent's progress line follows its name and says whether its children
/// still work. Each node's model and `reasoning_level`, then its objective at
/// `objective_detail`, follow beside the rail; both are omitted when
/// objectives are hidden. Unconnected top-level roots are separated by a
/// blank row.
fn graph_lines(
    snapshot: &GraphChatSnapshot,
    nodes: &[GraphNode],
    awaited: &BTreeSet<&str>,
    width: usize,
    objective_detail: ObjectiveDetail,
    reasoning_level: ReasoningLevel,
    spinner_frame: usize,
) -> Vec<Line<'static>> {
    let order = GraphNode::hierarchy_order(nodes);
    let satisfied = nodes
        .iter()
        .map(|node| {
            node.dependencies
                .iter()
                .map(|dependency| {
                    is_dependency_satisfied(snapshot.progress.as_ref(), node, dependency)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for (position, placement) in order.iter().enumerate() {
        let node = &nodes[placement.index];
        let node_satisfied = &satisfied[placement.index];
        let previous = position
            .checked_sub(1)
            .map(|previous| nodes[order[previous].index].id.as_str());
        if node.dependencies.is_empty() && position > 0 && !placement.is_child {
            lines.push(Line::default());
        }
        for (dependency, is_satisfied) in node.dependencies.iter().zip(node_satisfied) {
            let mut spans = level_indent(placement.is_child);
            spans.extend([
                rail(*is_satisfied),
                Span::raw(" "),
                Span::styled(
                    edge_label(dependency, previous),
                    edge_label_style(*is_satisfied),
                ),
            ]);
            lines.push(fitted_line(spans, width));
        }
        if !node.dependencies.is_empty() {
            let mut spans = level_indent(placement.is_child);
            spans.push(rail(
                node_satisfied.iter().all(|is_satisfied| *is_satisfied),
            ));
            lines.push(Line::from(spans));
        }
        let indent_width = if placement.is_child {
            CHILD_INDENT.width()
        } else {
            0
        };
        lines.extend(
            node_lines(
                node,
                node_status(snapshot, node, awaited),
                width.saturating_sub(indent_width),
                spinner_frame,
            )
            .into_iter()
            .map(|line| {
                let mut spans = level_indent(placement.is_child);
                spans.extend(line.spans);

                Line::from(spans)
            }),
        );
        let next = order
            .get(position + 1)
            .map(|next| (next.is_child, satisfied[next.index].as_slice()));
        let prefix = detail_prefix(placement.is_child, next);
        if let Some(children) = ChildrenProgress::of(nodes, snapshot.progress.as_ref(), &node.id) {
            lines.push(children_line(children, &prefix, width));
        }
        if objective_detail == ObjectiveDetail::Hidden {
            continue;
        }
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

/// Leading cells of a row drawn at a node's level: none for a top-level
/// node, [`CHILD_INDENT`] for a child.
fn level_indent(is_child: bool) -> Vec<Span<'static>> {
    if is_child {
        return vec![Span::raw(CHILD_INDENT)];
    }

    Vec::new()
}

/// Prefix of the detail rows below a node, so its text starts one
/// [`DETAIL_INDENT`] past the node's own indent. When the `next` node, given
/// as its level and dependency states, has dependencies and is not nested
/// deeper, the prefix carries that node's rail in its column, solid once
/// every dependency is satisfied; a child's rail starts at its own edges.
fn detail_prefix(is_child: bool, next: Option<(bool, &[bool])>) -> Vec<Span<'static>> {
    match next {
        Some((next_is_child, next_satisfied))
            if !next_satisfied.is_empty() && (is_child || !next_is_child) =>
        {
            let mut prefix = level_indent(next_is_child);
            prefix.extend([
                rail(next_satisfied.iter().all(|is_satisfied| *is_satisfied)),
                Span::raw(" "),
            ]);
            if is_child && !next_is_child {
                prefix.push(Span::raw(CHILD_INDENT));
            }

            prefix
        }
        _ => {
            let mut prefix = level_indent(is_child);
            prefix.push(Span::raw(DETAIL_INDENT));

            prefix
        }
    }
}

/// A parent's progress line after `prefix`, in the failure color once a
/// child failed, info-colored while a child works, and muted otherwise.
fn children_line(
    children: ChildrenProgress,
    prefix: &[Span<'static>],
    width: usize,
) -> Line<'static> {
    let style = if children.has_failed() {
        color_style(palette::danger())
    } else if children.is_working() {
        color_style(palette::info())
    } else {
        muted_style()
    };
    let mut spans = prefix.to_vec();
    spans.push(Span::styled(children.label(), style));

    fitted_line(spans, width)
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
    let room = width.saturating_sub(prefix.iter().map(Span::width).sum());

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
    let label = status.label();
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
/// settled nodes. Each parent moves with its indented children, ranked by
/// its most urgent member, and its children follow it by priority, so the
/// child that needs the user comes right after its parent. Stable sorting
/// retains declaration order within each priority. Long node names are
/// shortened so statuses stay visible.
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
    let priority = |placement: &GraphNodePlacement| {
        let node = &nodes[placement.index];

        node_priority(awaited, node, node_progress(snapshot, node))
    };
    let mut groups: Vec<Vec<GraphNodePlacement>> = Vec::new();
    for placement in GraphNode::hierarchy_order(nodes) {
        match (placement.is_child, groups.last_mut()) {
            (true, Some(group)) => group.push(placement),
            _ => groups.push(vec![placement]),
        }
    }
    for group in &mut groups {
        group[1..].sort_by_key(priority);
    }
    groups.sort_by_key(|group| group.iter().map(priority).min());
    let ordered = groups.into_iter().flatten().collect::<Vec<_>>();
    for placement in &ordered[..capacity.min(ordered.len())] {
        let node = &nodes[placement.index];
        let indent = level_indent(placement.is_child);
        let row_width = width.saturating_sub(indent.iter().map(Span::width).sum());
        let status = node_status(snapshot, node, awaited);
        let name_room = row_width.saturating_sub(status_label_width(status) + STATUS_GAP);
        let mut spans = indent;
        spans.extend(
            node_row(
                truncate_with_ellipsis(&node.id, name_room),
                status,
                row_width,
                spinner_frame,
            )
            .spans,
        );
        lines.push(Line::from(spans));
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
    let label = status.label();
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
    status_glyph(status, 0).0.width() + 1 + status.label().width()
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
