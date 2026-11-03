//! Keyboard routing for the snapshot-bound graph inspector.

use ag_session::GraphNodeContinuation;
use crossterm::event::{KeyCode, KeyEvent};

use super::input_key::{self, InputCapabilities};
use crate::app::App;
use crate::app::graph_view::GraphViewAction;
use crate::domain::input::InputState;
use crate::presentation::app_mode::AppMode;
use crate::presentation::execution_graph::{GraphEditKind, GraphEditor, GraphView};
use crate::runtime::EventResult;

pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ExecutionGraph(mut view) = std::mem::replace(&mut app.mode, AppMode::List) else {
        return EventResult::Continue;
    };
    if view.editor.is_none() && matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.mode = view.restore.into_view_mode();

        return EventResult::Continue;
    }
    let selection = (
        view.selected_graph,
        view.selected_node,
        view.selected_output,
        view.scroll,
    );
    match resolve_key(&mut view, key) {
        GraphKeyAction::Refresh => {
            view.notice = match app.load_graph_view(&mut view).await {
                Ok(()) => "View refreshed".into(),
                Err(error) => {
                    (
                        view.selected_graph,
                        view.selected_node,
                        view.selected_output,
                        view.scroll,
                    ) = selection;

                    error
                }
            };
        }
        GraphKeyAction::Submit(action) => {
            let result = app.apply_graph_view_action(&view, action).await;
            if let Err(error) = &result
                && let Some(editor) = &mut view.editor
            {
                editor.needs_reconciliation = true;
                view.notice =
                    format!("Draft retained. {error} Ctrl+r to reconcile; Esc to discard.");
            } else {
                view.editor = None;
                let refresh = app.load_graph_view(&mut view).await;
                view.notice = match (result, refresh) {
                    (Err(error), _) | (_, Err(error)) => error,
                    _ => "Action admitted. Refresh to inspect progress.".into(),
                };
            }
        }
        GraphKeyAction::Reconcile => {
            view.notice = match app.reconcile_graph_editor(&mut view).await {
                Ok(()) => {
                    "Draft reconciled. Inspect the refreshed snapshot, then Enter to submit.".into()
                }
                Err(error) => error,
            };
        }
        GraphKeyAction::None => {}
    }
    app.mode = AppMode::ExecutionGraph(view);

    EventResult::Continue
}

enum GraphKeyAction {
    None,
    Reconcile,
    Refresh,
    Submit(GraphViewAction),
}

/// Resolves navigation and editing without database or terminal access.
fn resolve_key(view: &mut GraphView, key: KeyEvent) -> GraphKeyAction {
    if view.editor.is_some() {
        return resolve_editor_key(view, key);
    }
    if matches!(key.code, KeyCode::Char(_)) && !input_key::is_insertable_char_key(key) {
        return GraphKeyAction::None;
    }
    match key.code {
        KeyCode::Char('r') => return GraphKeyAction::Refresh,
        KeyCode::Char('[' | ']') if !view.graphs.is_empty() => {
            let count = view.graphs.len();
            view.selected_graph = if key.code == KeyCode::Char(']') {
                (view.selected_graph + 1) % count
            } else {
                (view.selected_graph + count - 1) % count
            };
            view.selected_node = 0;
            view.selected_output = 0;
            view.scroll = 0;

            return GraphKeyAction::Refresh;
        }
        KeyCode::Char('j' | 'k') | KeyCode::Down | KeyCode::Up => {
            let count = view
                .progress
                .as_ref()
                .map_or(0, |progress| progress.nodes.len());
            if count > 0 {
                view.selected_node = if matches!(key.code, KeyCode::Down | KeyCode::Char('j')) {
                    (view.selected_node + 1) % count
                } else {
                    (view.selected_node + count - 1) % count
                };
                view.selected_output = 0;
                view.scroll = 0;

                return GraphKeyAction::Refresh;
            }
        }
        KeyCode::Char('o') if !view.outputs.is_empty() => {
            view.selected_output = (view.selected_output + 1) % view.outputs.len();
            view.scroll = 0;
        }
        KeyCode::Char('J') | KeyCode::PageDown => view.scroll = view.scroll.saturating_add(5),
        KeyCode::Char('K') | KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(5),
        KeyCode::Home => view.scroll = 0,
        KeyCode::Char('a') if view.can_accept() => {
            return GraphKeyAction::Submit(GraphViewAction::Accept);
        }
        KeyCode::Char('t') if view.can_retry() => {
            return GraphKeyAction::Submit(GraphViewAction::Continue(GraphNodeContinuation::Retry));
        }
        KeyCode::Char('e') if view.can_continue() => open_editor(view, GraphEditKind::Reply),
        KeyCode::Char('n')
            if view.can_continue()
                && view.node().is_some_and(|node| !node.questions.is_empty()) =>
        {
            open_editor(view, GraphEditKind::Answers);
        }
        KeyCode::Char('x') if view.can_cancel() => open_editor(view, GraphEditKind::Cancel),
        KeyCode::Char('s') if view.can_resume() => open_editor(view, GraphEditKind::Resume),
        _ => {}
    }

    GraphKeyAction::None
}

fn open_editor(view: &mut GraphView, kind: GraphEditKind) {
    view.notice.clear();
    view.notice_scroll = 0;
    view.editor = Some(GraphEditor {
        answers: Vec::new(),
        input: if kind == GraphEditKind::Resume {
            InputState::with_text(view.resume_limits.clone())
        } else {
            InputState::default()
        },
        kind,
        needs_reconciliation: false,
    });
}

fn resolve_editor_key(view: &mut GraphView, key: KeyEvent) -> GraphKeyAction {
    match key.code {
        KeyCode::PageDown => {
            view.notice_scroll = view.notice_scroll.saturating_add(1);

            return GraphKeyAction::None;
        }
        KeyCode::PageUp => {
            view.notice_scroll = view.notice_scroll.saturating_sub(1);

            return GraphKeyAction::None;
        }
        _ => view.notice_scroll = 0,
    }
    if key.code == KeyCode::Esc {
        view.editor = None;

        return GraphKeyAction::None;
    }
    if view
        .editor
        .as_ref()
        .is_some_and(|editor| editor.needs_reconciliation)
    {
        if input_key::is_control_key(key) && key.code == KeyCode::Char('r') {
            return GraphKeyAction::Reconcile;
        }
        if input_key::is_enter_key(key.code) && !input_key::should_insert_newline(key) {
            view.notice =
                "Draft retained. Ctrl+r to reconcile before submitting; Esc to discard.".into();

            return GraphKeyAction::None;
        }
    }
    if input_key::is_enter_key(key.code) && !input_key::should_insert_newline(key) {
        let question_count = view.node().map_or(0, |node| node.questions.len());
        let Some(editor) = view.editor.as_mut() else {
            return GraphKeyAction::None;
        };
        if editor.kind == GraphEditKind::Answers {
            let answer = editor.input.text().trim().to_string();
            if answer.is_empty() {
                view.notice = "Enter an answer to this question".into();

                return GraphKeyAction::None;
            }
            if editor.answers.len() + 1 < question_count {
                editor.answers.push(answer);
                editor.input = InputState::default();

                return GraphKeyAction::None;
            }
        }

        return match App::graph_editor_action(view) {
            Ok(GraphViewAction::Resume(text)) => match view.parse_budgets(&text) {
                Ok(_) => GraphKeyAction::Submit(GraphViewAction::Resume(text)),
                Err(error) => {
                    view.notice = error;
                    GraphKeyAction::None
                }
            },
            Ok(action) => GraphKeyAction::Submit(action),
            Err(error) => {
                view.notice = error;
                GraphKeyAction::None
            }
        };
    }
    if let Some(command) = input_key::command_for_key(key, InputCapabilities::MULTILINE)
        && let Some(editor) = &mut view.editor
    {
        editor.input.apply(command);
    }

    GraphKeyAction::None
}

#[cfg(test)]
#[path = "execution_graph_test.rs"]
mod tests;
