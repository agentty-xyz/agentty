//! Rendering only; action availability belongs to the inspector snapshot.

use std::fmt::Write as _;

use ag_session::{GraphNodePlacement, GraphNodeStatus};
use ag_tui_text::text_util::{
    borrowed_paint_line, split_line_at_width, wrap_lines_breaking_long_words,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::presentation::execution_graph::{
    FOCUSED_REVIEW_NOTE, GraphEditKind, GraphEditor, GraphView, PROPOSAL_REPLY_HINT,
    QUESTION_REPLY_HINT, UNAVAILABLE_STATUS_LABEL, blocked_reason_label,
};
use crate::presentation::graph_chat::{
    CHILD_INDENT, ChildrenProgress, gate_label, proposal_preview,
};
use crate::ui::Component;
use crate::ui::component::chat_input::ChatInput;
use crate::ui::markdown::MarkdownRenderCache;
use crate::ui::style::palette;

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    view: &GraphView,
    markdown: &MarkdownRenderCache,
) {
    let panes = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(if view.editor.is_some() {
            if view.notice.is_empty() { 9 } else { 13 }
        } else {
            7
        }),
    ])
    .split(area);
    let title = view.selected_proposal().map_or_else(
        || {
            view.graph().map_or_else(
                || "Execution graphs: none saved for this session".into(),
                |graph| {
                    format!(
                        "Graph {}/{}: {} | revision {} | {}",
                        view.selected_graph + 1,
                        view.graphs.len(),
                        graph.id,
                        graph.revision,
                        view.progress.as_ref().map_or_else(
                            || UNAVAILABLE_STATUS_LABEL.into(),
                            |progress| format!("{:?}", progress.status)
                        )
                    )
                },
            )
        },
        |state| format!("Proposal {} v{}", state.input.id, state.version.number),
    );
    frame.render_widget(
        Paragraph::new(title).block(block("Execution graphs")),
        panes[0],
    );
    let columns = Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(panes[1]);
    let order = view.node_order();
    let nodes = node_list(
        view,
        &order,
        usize::from(columns[0].width.saturating_sub(2)),
    );
    let selected_row = order
        .iter()
        .position(|placement| placement.index == view.selected_node)
        .unwrap_or_default();
    let node_scroll = u16::try_from(selected_row)
        .unwrap_or(u16::MAX)
        .saturating_sub(columns[0].height.saturating_sub(3));
    if view.selected_proposal().is_none() {
        frame.render_widget(
            Paragraph::new(nodes)
                .scroll((node_scroll, 0))
                .block(block("Nodes (j/k)")),
            columns[0],
        );
    }
    render_details(
        frame,
        if view.selected_proposal().is_some() {
            panes[1]
        } else {
            columns[1]
        },
        view,
        markdown,
    );
    if let Some(editor) = &view.editor {
        render_editor(frame, panes[2], view, editor);
    } else {
        frame.render_widget(
            Paragraph::new(format!("{}\n{}", actions(view).join(" | "), view.notice))
                .wrap(Wrap { trim: false })
                .block(block("Controls")),
            panes[2],
        );
    }
}

/// Node rows in the inspector's order, marking the selected one: children
/// indented under their parent, and a parent's status followed by how far
/// its children are. When a parent's row is wider than `width`, its
/// children's progress is dropped first, since the details pane repeats it.
fn node_list(view: &GraphView, order: &[GraphNodePlacement], width: usize) -> String {
    let (Some(graph), Some(progress)) = (view.graph(), view.progress.as_ref()) else {
        return String::new();
    };

    order.iter().fold(String::new(), |mut text, placement| {
        let node = &progress.nodes[placement.index];
        let row = format!(
            "{} {}{}: {:?}",
            if placement.index == view.selected_node {
                ">"
            } else {
                " "
            },
            if placement.is_child { CHILD_INDENT } else { "" },
            node.node_id,
            node.status
        );
        let children = ChildrenProgress::of(
            &graph.nodes,
            Some(progress),
            &graph.nodes[placement.index].id,
        )
        .map(|children| format!(" ({})", children.label()))
        .filter(|children| row.width() + children.width() <= width)
        .unwrap_or_default();
        let _ = writeln!(text, "{row}{children}");

        text
    })
}

fn render_details(frame: &mut Frame, area: Rect, view: &GraphView, markdown: &MarkdownRenderCache) {
    let block = block(if view.selected_proposal().is_some() {
        "Graph proposal (J/K scroll)"
    } else {
        "Node and exact output (J/K scroll)"
    });
    let inner = block.inner(area);
    let width = usize::from(inner.width);
    let metadata = details(view);
    // Proposal previews are Markdown; node metadata stays plain text.
    let proposal = view
        .selected_proposal()
        .map(|_| markdown.render(&metadata, width));
    let metadata = proposal.as_deref().map_or_else(
        || wrap_lines_breaking_long_words(&metadata, width),
        |lines| {
            lines
                .iter()
                .flat_map(|line| split_line_at_width(borrowed_paint_line(line), width))
                .collect()
        },
    );
    let output = view
        .outputs
        .get(view.selected_output)
        .filter(|_| {
            view.graph()
                .is_some_and(|graph| graph.nodes.get(view.selected_node).is_some())
        })
        .map_or_default(|output| markdown.render(output.display(), width));
    // Split oversized Markdown tokens before slicing the viewport. Borrow
    // cached spans for lines that fit, leaving the cached output shared.
    let lines = metadata
        .into_iter()
        .chain(
            output
                .iter()
                .flat_map(|line| split_line_at_width(borrowed_paint_line(line), width)),
        )
        .skip(usize::from(view.scroll))
        .take(usize::from(inner.height))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_editor(frame: &mut Frame, area: Rect, view: &GraphView, editor: &GraphEditor) {
    let label = match editor.kind {
        GraphEditKind::Reply => "Reply to selected task (Enter sends; Esc cancels)".into(),
        GraphEditKind::Cancel => "Type cancel, then Enter to cancel this graph".into(),
        GraphEditKind::ExtendBudget => format!(
            "Type how many provider calls to add to budget {}, then Enter (recorded; Esc cancels)",
            view.exhausted_budget().unwrap_or_default()
        ),
    };
    let editor_areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(if view.notice.is_empty() { 1 } else { 5 }),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(label).wrap(Wrap { trim: false }),
        editor_areas[0],
    );
    ChatInput::new("Graph input", editor.input.text(), editor.input.cursor)
        .footer(if editor.needs_reconciliation {
            "Ctrl+r reconcile draft | Esc discard"
        } else {
            "Enter send | Alt+Enter newline | Esc cancel"
        })
        .render(frame, editor_areas[1]);
    if !view.notice.is_empty() {
        frame.render_widget(
            Paragraph::new(view.notice.as_str())
                .wrap(Wrap { trim: false })
                .scroll((view.notice_scroll, 0))
                .block(block("Notice (PgUp/PgDn scroll)")),
            editor_areas[2],
        );
    }
}

fn block(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::border()))
}

fn actions(view: &GraphView) -> Vec<&'static str> {
    let mut actions = vec!["q back", "[/] graph", "j/k node", "r refresh", "o output"];
    if view.proposal.is_some() {
        actions.push("p proposal");
    }
    if view.selected_proposal().is_some() {
        let mut actions = vec!["q back", "[/] execution graphs", "r refresh", "J/K scroll"];
        if view
            .selected_proposal()
            .is_some_and(|state| !state.version.is_cancelled && state.version.approval.is_none())
        {
            actions.push(PROPOSAL_REPLY_HINT);
        }
        return actions;
    }
    if view.can_accept() {
        actions.push("a accept displayed version");
    }
    if view.can_continue() {
        actions.push("e reply/revise");
    }
    if view
        .node()
        .is_some_and(|node| node.status == GraphNodeStatus::AwaitingAnswers)
    {
        actions.push(QUESTION_REPLY_HINT);
    }
    if view.can_retry() {
        actions.push("t retry node");
    }
    if view.can_resume() {
        actions.push("s resume graph");
    }
    if view.can_extend_budget() {
        actions.push("b add provider calls");
    }
    if view.can_cancel() {
        actions.push("x cancel graph");
    }

    actions
}

fn details(view: &GraphView) -> String {
    if let Some(proposal) = view.selected_proposal() {
        return proposal_preview(proposal);
    }
    let Some(graph) = view.graph() else {
        return "No saved execution graphs. Press q to return.".into();
    };
    let Some(node) = graph.nodes.get(view.selected_node) else {
        return String::new();
    };
    let mut text = format!(
        "{}\nAgent: {} ({}, {})\nObjective: {}\n",
        node.id,
        node.definition.name,
        node.agent.kind(),
        node.agent.model().as_str(),
        node.objective
    );
    if let Some(parent) = &node.parent_id {
        let _ = writeln!(text, "Parent: {parent} (shares its workspace)");
    }
    if let Some(progress) = ChildrenProgress::of(&graph.nodes, view.progress.as_ref(), &node.id) {
        let children = graph
            .nodes
            .iter()
            .filter(|child| child.parent_id.as_deref() == Some(node.id.as_str()))
            .map(|child| child.id.as_str())
            .collect::<Vec<_>>();
        let _ = writeln!(
            text,
            "Children: {} ({})",
            children.join(", "),
            progress.label()
        );
    }
    if view.is_focused_review() {
        text.push_str(FOCUSED_REVIEW_NOTE);
        text.push('\n');
    }
    for dependency in &node.dependencies {
        let _ = writeln!(
            text,
            "Requires {}: {}",
            dependency.node_id,
            gate_label(&dependency.gate)
        );
    }
    if let Some(progress) = view.node() {
        let _ = writeln!(
            text,
            "Status: {:?} | Attempts: {}",
            progress.status,
            progress.executions.len()
        );
        for reason in &progress.blocked {
            let _ = writeln!(text, "Blocked: {}", blocked_reason_label(reason));
        }
        if let Some(error) = &progress.last_error {
            let _ = writeln!(text, "Error: {error}");
        }
        for question in &progress.questions {
            let _ = writeln!(
                text,
                "Question: {} {}",
                question.text,
                question.options.join(" / ")
            );
        }
    }
    if let Some(output) = view.outputs.get(view.selected_output) {
        let result = &output.result;
        let reference = &result.reference;
        let _ = writeln!(
            text,
            "\nOutput {}/{}: {} v{} ({:?})",
            view.selected_output + 1,
            view.outputs.len(),
            reference.output_name,
            reference.version,
            result.state
        );
    }

    text
}

#[cfg(test)]
#[path = "execution_graph_test.rs"]
mod tests;
