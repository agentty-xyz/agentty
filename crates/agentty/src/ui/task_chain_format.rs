//! Pure formatting for task-chain transcript rows.

use ag_session::BuiltinAgentDefinition;
use ag_tui_text::text_util;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::presentation::task_chain_graph::{TaskChainGraph, TaskChainNodeStatus};
use crate::ui::style;

/// Title row of the execution-graph card.
const GRAPH_TITLE: &str = "⬡ Execution graph";

/// Muted row shown until the chain snapshot loads.
const GRAPH_PLACEHOLDER: &str = "Loading status…";

/// Indent of node rows below the card title.
const NODE_INDENT: &str = "  ";

/// Indent of edge rows and wrapped status rows below node names.
const DETAIL_INDENT: &str = "    ";

/// Gap between the node-name column and its status.
const STATUS_GAP: &str = "  ";

/// Returns the role header shown above one role-attributed answer.
pub(crate) fn agent_answer_header_line(
    agent: BuiltinAgentDefinition,
    width: usize,
) -> Line<'static> {
    let accent = Style::default()
        .fg(style::palette::accent())
        .add_modifier(Modifier::BOLD);

    fitted_line(
        vec![
            Span::styled("◆ ", accent),
            Span::styled(agent.display_name(), accent),
        ],
        width,
    )
}

/// Returns the compact vertical execution-graph card.
///
/// Node statuses share one column when it fits; narrower panes move each
/// status below its node. `None` renders a neutral loading placeholder.
pub(crate) fn graph_card_lines(graph: Option<&TaskChainGraph>, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![fitted_line(
        vec![Span::styled(
            GRAPH_TITLE,
            Style::default()
                .fg(style::palette::text())
                .add_modifier(Modifier::BOLD),
        )],
        width,
    )];
    let Some(graph) = graph else {
        lines.push(fitted_line(
            vec![
                Span::raw(NODE_INDENT),
                Span::styled(GRAPH_PLACEHOLDER, muted_style()),
            ],
            width,
        ));

        return lines;
    };
    let name_width = graph
        .nodes
        .iter()
        .map(|node| node.agent.display_name().width())
        .max()
        .unwrap_or_default();
    let status_width = graph
        .nodes
        .iter()
        .map(|node| status_label_width(node.status))
        .max()
        .unwrap_or_default();
    let fits_inline = NODE_INDENT.width() + name_width + STATUS_GAP.width() + status_width <= width;

    for (index, node) in graph.nodes.iter().enumerate() {
        let name = Span::styled(
            node.agent.display_name(),
            Style::default()
                .fg(style::palette::text())
                .add_modifier(Modifier::BOLD),
        );
        let status = status_spans(node.status);
        if fits_inline {
            let padding = name_width - node.agent.display_name().width();
            let mut spans = vec![
                Span::raw(NODE_INDENT),
                name,
                Span::raw(format!("{}{STATUS_GAP}", " ".repeat(padding))),
            ];
            spans.extend(status);
            lines.push(fitted_line(spans, width));
        } else {
            lines.push(fitted_line(vec![Span::raw(NODE_INDENT), name], width));
            let mut spans = vec![Span::raw(DETAIL_INDENT)];
            spans.extend(status);
            lines.push(fitted_line(spans, width));
        }
        if let Some(edge) = graph.edges.get(index) {
            let edge_style = if edge.is_satisfied {
                Style::default().fg(style::palette::success())
            } else {
                muted_style()
            };
            let label = if edge.is_satisfied {
                format!("│ {} ✓", edge.label)
            } else {
                format!("│ {}", edge.label)
            };
            lines.push(fitted_line(
                vec![Span::raw(DETAIL_INDENT), Span::styled(label, edge_style)],
                width,
            ));
            lines.push(fitted_line(
                vec![Span::raw(DETAIL_INDENT), Span::styled("▼", edge_style)],
                width,
            ));
        }
    }

    lines
}

/// Returns the glyph and label spans for one node status.
fn status_spans(status: TaskChainNodeStatus) -> [Span<'static>; 2] {
    let (glyph, color) = status_glyph(status);
    let style = Style::default().fg(color);

    [
        Span::styled(format!("{glyph} "), style),
        Span::styled(status.label(), style),
    ]
}

/// Returns the per-status glyph and semantic color.
fn status_glyph(status: TaskChainNodeStatus) -> (&'static str, Color) {
    match status {
        TaskChainNodeStatus::Waiting => ("○", style::palette::text_muted()),
        TaskChainNodeStatus::Working => ("◐", style::palette::info()),
        TaskChainNodeStatus::NeedsInput => ("?", style::palette::question()),
        TaskChainNodeStatus::AwaitingApproval => ("●", style::palette::warning()),
        TaskChainNodeStatus::Done => ("✓", style::palette::success()),
        TaskChainNodeStatus::Failed => ("✗", style::palette::danger()),
        TaskChainNodeStatus::Canceled => ("⊘", style::palette::text_subtle()),
    }
}

/// Returns the rendered width of one status glyph plus label.
fn status_label_width(status: TaskChainNodeStatus) -> usize {
    status_glyph(status).0.width() + 1 + status.label().width()
}

fn muted_style() -> Style {
    Style::default().fg(style::palette::text_muted())
}

/// Keeps every card row on one terminal row so line counts match painting.
fn fitted_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    Line::from(text_util::truncate_spans_with_ellipsis(spans, width))
}

#[cfg(test)]
#[path = "task_chain_format_test.rs"]
mod tests;
