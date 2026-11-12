//! Rendering only; action availability belongs to the inspector snapshot.

use std::fmt::Write as _;

use ag_session::{GraphBlockedReason, GraphCompletionGate, GraphNodeStatus};
use ag_tui_text::text_util::{
    borrowed_paint_line, split_line_at_width, wrap_lines_breaking_long_words,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::presentation::execution_graph::{GraphEditKind, GraphEditor, GraphView};
use crate::presentation::graph_chat::proposal_preview;
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
                            || "Unavailable".into(),
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
    let nodes = view.progress.as_ref().map_or_else(String::new, |progress| {
        progress
            .nodes
            .iter()
            .enumerate()
            .fold(String::new(), |mut text, (index, node)| {
                let _ = writeln!(
                    text,
                    "{} {}: {:?}",
                    if index == view.selected_node {
                        ">"
                    } else {
                        " "
                    },
                    node.node_id,
                    node.status
                );

                text
            })
    });
    let node_scroll = u16::try_from(view.selected_node)
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

fn render_details(frame: &mut Frame, area: Rect, view: &GraphView, markdown: &MarkdownRenderCache) {
    let block = block(if view.selected_proposal().is_some() {
        "Graph proposal (J/K scroll)"
    } else {
        "Node and exact output (J/K scroll)"
    });
    let inner = block.inner(area);
    let width = usize::from(inner.width);
    let metadata = details(view);
    let metadata = wrap_lines_breaking_long_words(&metadata, width);
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
        GraphEditKind::Reply => "Reply to selected task (Enter sends; Esc cancels)",
        GraphEditKind::Cancel => "Type cancel, then Enter to cancel this graph",
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
            actions.push("reply in chat to approve, revise, or cancel");
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
        actions.push("reply in chat to answer");
    }
    if view.can_retry() {
        actions.push("t retry node");
    }
    if view.can_resume() {
        actions.push("s resume graph");
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
    if view.is_focused_review() {
        text.push_str(
            "Focused review: use Regenerate review in session view for a new generation with \
             checkpoints. Generic retry/resume/reply is unavailable.\n",
        );
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
            let reason = match reason {
                GraphBlockedReason::Dependency { gate, node_id } => {
                    format!("{node_id}: {}", gate_label(gate))
                }
                GraphBlockedReason::EnvironmentBusy => "Workspace is busy".into(),
                GraphBlockedReason::AwaitingDispatch => {
                    "Waiting for worker capacity or workspace".into()
                }
                GraphBlockedReason::BudgetExhausted(name) => {
                    format!("Provider-call budget exhausted: {name}")
                }
                GraphBlockedReason::ResumeRequired => "Paused; resume required".into(),
            };
            let _ = writeln!(text, "Blocked: {reason}");
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

fn gate_label(gate: &GraphCompletionGate) -> String {
    match gate {
        GraphCompletionGate::ExecutionSucceeded => "successful execution without questions".into(),
        GraphCompletionGate::ResultValidated { output_name } => format!("validated {output_name}"),
        GraphCompletionGate::ResultAccepted { output_name } => format!("accepted {output_name}"),
    }
}

#[cfg(test)]
#[path = "execution_graph_test.rs"]
mod tests;
