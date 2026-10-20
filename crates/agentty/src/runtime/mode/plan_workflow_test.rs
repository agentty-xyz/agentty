use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::presentation::plan_workflow::{
    PlanEditorKind, PlanWorkflowIntent, PlanWorkflowItem, PlanWorkflowState,
};
use crate::runtime::mode::plan_workflow::handle_key;

fn state() -> PlanWorkflowState {
    PlanWorkflowState {
        chains: vec![PlanWorkflowItem {
            accept_plan: Some(("planner".into(), "plan-version".into())),
            agent_label: String::new(),
            body: String::new(),
            can_cancel: true,
            continuation: Some(PlanEditorKind::RevisePlan),
            id: "workflow".into(),
            status: String::new(),
            user_request: String::new(),
        }],
        ..PlanWorkflowState::default()
    }
}

#[test]
fn coordinator_actions_require_explicit_keys_and_available_capabilities() {
    // Arrange
    let cases = [
        (KeyCode::Enter, Some(PlanWorkflowIntent::Accept)),
        (KeyCode::Char('x'), Some(PlanWorkflowIntent::Cancel)),
        (KeyCode::F(5), Some(PlanWorkflowIntent::Refresh)),
        (KeyCode::Esc, Some(PlanWorkflowIntent::Close)),
        (KeyCode::Char('q'), None),
    ];
    for (key, expected) in cases {
        let mut state = state();

        // Act, Assert
        assert_eq!(
            handle_key(&mut state, KeyEvent::new(key, KeyModifiers::NONE)),
            expected
        );
    }
    let mut state = PlanWorkflowState::default();
    for key in [KeyCode::Enter, KeyCode::Char('x'), KeyCode::Char('r')] {
        assert_eq!(
            handle_key(&mut state, KeyEvent::new(key, KeyModifiers::NONE)),
            None
        );
        assert!(state.editor.is_none());
    }
    assert_eq!(
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)
        ),
        None
    );
}

#[test]
fn editor_preserves_literal_input_and_requires_text_except_for_explicit_retry() {
    // Arrange
    let mut state = state();
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(&mut state, key(KeyCode::Char('r')));

    // Act, Assert
    assert_eq!(
        state.editor.as_ref().expect("editor").kind,
        PlanEditorKind::RevisePlan
    );
    assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), None);
    assert!(state.error.is_some());
    handle_key(&mut state, key(KeyCode::Char('x')));
    handle_key(&mut state, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    handle_key(&mut state, key(KeyCode::Char('y')));
    assert_eq!(state.editor.as_ref().expect("editor").input.text(), "x\ny");
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Enter)),
        Some(PlanWorkflowIntent::Submit)
    );
    assert_eq!(handle_key(&mut state, key(KeyCode::Esc)), None);
    assert!(state.editor.is_none());
    handle_key(&mut state, key(KeyCode::Char('n')));
    assert_eq!(
        state.editor.as_ref().expect("editor").kind,
        PlanEditorKind::New
    );
    state.begin_editing(PlanEditorKind::RetryImplementer);
    assert_eq!(
        handle_key(&mut state, key(KeyCode::Enter)),
        Some(PlanWorkflowIntent::Submit)
    );
}

#[test]
fn loading_blocks_double_submission_and_escape_still_returns_to_chat() {
    // Arrange
    let mut state = state();
    state.begin_editing(PlanEditorKind::New);
    state.is_loading = true;

    // Act, Assert
    for key in [
        KeyCode::Enter,
        KeyCode::Char('x'),
        KeyCode::Char('n'),
        KeyCode::F(5),
    ] {
        assert_eq!(
            handle_key(&mut state, KeyEvent::new(key, KeyModifiers::NONE)),
            None
        );
    }
    assert_eq!(
        handle_key(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Some(PlanWorkflowIntent::Close)
    );
}

#[test]
fn navigation_changes_only_foreground_selection_and_scroll() {
    // Arrange
    let mut state = state();
    state.scroll_offset = 0;

    // Act
    for key in [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
    ] {
        handle_key(&mut state, KeyEvent::new(key, KeyModifiers::NONE));
    }

    // Assert
    assert_eq!(state.scroll_offset, 1);
    handle_key(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(state.scroll_offset, 0);
}
