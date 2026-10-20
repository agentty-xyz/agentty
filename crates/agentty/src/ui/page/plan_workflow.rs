//! Separate Planner and Implementer output, outside the ordinary transcript.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::presentation::plan_workflow::PlanWorkflowState;
use crate::ui::component::chat_input::ChatInput;
use crate::ui::{Component, markdown, style};

/// Paints one already-loaded workflow snapshot without executing decisions.
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &PlanWorkflowState,
    markdown_cache: &markdown::MarkdownRenderCache,
) {
    let editor_height = if state.editor.is_some() { 6 } else { 0 };
    let [header, body, editor, footer] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(0),
        Constraint::Length(editor_height),
        Constraint::Length(2),
    ])
    .areas(area);
    let selected = state.selected();
    let header_text = selected.map_or_else(
        || {
            vec![Line::from(
                "Start a read-only Planner, inspect its plan, then accept to run Implementer.",
            )]
        },
        |chain| {
            vec![
                Line::from(vec![
                    Span::raw(chain.status.as_str()),
                    Span::raw(" · "),
                    Span::raw(chain.agent_label.as_str()),
                ]),
                Line::from(chain.user_request.lines().next().unwrap_or_default()),
                Line::from(format!(
                    "Workflow {}/{} · {}",
                    state.selected_index + 1,
                    state.chains.len(),
                    chain.id
                )),
            ]
        },
    );
    frame.render_widget(
        Paragraph::new(header_text).block(
            Block::bordered()
                .title(" Plan then implement ")
                .border_style(style::border_style()),
        ),
        header,
    );

    let block = Block::bordered()
        .title(" Agent results ")
        .border_style(style::border_style());
    let inner = block.inner(body);
    frame.render_widget(block, body);
    let text = selected.map_or("No workflows yet. Press n to write a request.", |chain| {
        chain.body.as_str()
    });
    let lines = markdown_cache.render(text, usize::from(inner.width));
    let visible = lines
        .iter()
        .skip(usize::from(state.scroll_offset).min(lines.len().saturating_sub(1)))
        .take(usize::from(inner.height))
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);

    if let Some(composer) = &state.editor {
        ChatInput::new(
            composer.kind.title(),
            composer.input.text(),
            composer.input.cursor,
        )
        .footer("Enter submit · Alt+Enter newline · Esc discard")
        .active(!state.is_loading)
        .render(frame, editor);
    }
    let action_text = if state.is_loading {
        "Loading workflow…"
    } else if state.editor.is_some() {
        "The original request and accepted plan remain unchanged."
    } else {
        "n new · Enter accept plan · r revise / answer / retry · x cancel · Tab next · ↑↓ scroll · \
         F5 refresh · Esc chat"
    };
    let notice = state.error.as_deref().unwrap_or(action_text);
    frame.render_widget(
        Paragraph::new(Line::from(notice)).wrap(Wrap { trim: false }),
        footer,
    );
}

#[cfg(test)]
#[path = "plan_workflow_test.rs"]
mod tests;
