//! Key-to-intent mapping for the separate planning workflow panel.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::presentation::plan_workflow::{PlanEditorKind, PlanWorkflowIntent, PlanWorkflowState};
use crate::runtime::EventResult;
use crate::runtime::mode::input_key::{self, InputCapabilities};

/// Routes the panel's explicit intent to the application without awaiting work.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    if let AppMode::PlanWorkflow { state, .. } = &mut app.mode {
        let intent = handle_key(state, key);
        if let Some(intent) = intent {
            app.handle_plan_workflow_intent(intent);
        }
    }

    EventResult::Continue
}

/// Edits foreground state and returns only explicit coordinator actions.
pub(crate) fn handle_key(
    state: &mut PlanWorkflowState,
    key: KeyEvent,
) -> Option<PlanWorkflowIntent> {
    if key.code == KeyCode::Esc {
        if state.editor.take().is_none() || state.is_loading {
            return Some(PlanWorkflowIntent::Close);
        }

        return None;
    }
    if state.is_loading {
        return None;
    }
    if let Some(editor) = &mut state.editor {
        if input_key::is_enter_key(key.code) && key.modifiers == KeyModifiers::NONE {
            if editor.kind.requires_text() && editor.input.text().trim().is_empty() {
                state.error = Some("Enter text before submitting.".into());

                return None;
            }

            return Some(PlanWorkflowIntent::Submit);
        }
        if let Some(command) = input_key::command_for_key(key, InputCapabilities::MULTILINE) {
            editor.input.apply(command);
            state.error = None;
        }

        return None;
    }
    if key.modifiers != KeyModifiers::NONE {
        return None;
    }
    match key.code {
        KeyCode::Enter
            if state
                .selected()
                .is_some_and(|chain| chain.accept_plan.is_some()) =>
        {
            Some(PlanWorkflowIntent::Accept)
        }
        KeyCode::Char('x') if state.selected().is_some_and(|chain| chain.can_cancel) => {
            Some(PlanWorkflowIntent::Cancel)
        }
        KeyCode::Char('n') => {
            state.begin_editing(PlanEditorKind::New);

            None
        }
        KeyCode::Char('r') => {
            if let Some(kind) = state.selected().and_then(|chain| chain.continuation) {
                state.begin_editing(kind);
            }

            None
        }
        KeyCode::Tab => {
            state.select_next();

            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.scroll_offset = state.scroll_offset.saturating_add(1);

            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.scroll_offset = state.scroll_offset.saturating_sub(1);

            None
        }
        KeyCode::F(5) => Some(PlanWorkflowIntent::Refresh),
        _ => None,
    }
}

#[cfg(test)]
#[path = "plan_workflow_test.rs"]
mod tests;
