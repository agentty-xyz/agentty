use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::presentation::app_mode::AppMode;
use crate::presentation::plan_workflow::{
    PlanEditorKind, PlanWorkflowIntent, PlanWorkflowItem, PlanWorkflowState,
};
use crate::runtime::key_handler::handle_key_event;
use crate::runtime::mode::plan_workflow::{handle, handle_key};
use crate::runtime::{EventResult, PresentationState};

fn state() -> PlanWorkflowState {
    PlanWorkflowState {
        chains: vec![PlanWorkflowItem {
            accept_plan: Some(("planner".into(), "plan-version".into())),
            agent_label: String::new(),
            body: String::new(),
            can_cancel: true,
            continuation: Some(PlanEditorKind::RevisePlan),
            current_attempt_id: "plan-version".into(),
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
    state.is_submitting = true;

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
fn background_snapshot_refresh_keeps_typing_responsive_without_admitting_stale_actions() {
    // Arrange
    let mut state = state();
    state.begin_editing(PlanEditorKind::RevisePlan);
    state.is_loading = true;

    // Act
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    let submit = handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert_eq!(state.editor.as_ref().expect("editor").input.text(), "x");
    assert_eq!(submit, None);
    state.editor = None;
    assert_eq!(
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ),
        None
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
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    );
    assert_eq!(state.scroll_offset, 6);
    state.begin_editing(PlanEditorKind::RevisePlan);
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
    );
    assert_eq!(state.scroll_offset, 1);
    state.editor = None;
    handle_key(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(state.scroll_offset, 0);
}

#[tokio::test]
async fn panel_dispatch_edits_and_closes_without_waiting_for_the_session_actor() {
    // Arrange
    let (mut app, _root) = crate::test_support::new_test_app().await;
    app.mode = AppMode::List;
    assert!(matches!(
        handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        EventResult::Continue
    ));
    app.mode = AppMode::PlanWorkflow {
        session_id: "session".into(),
        state: Box::default(),
    };
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
    let presentation = PresentationState::default();

    // Act
    handle_key_event(
        &mut app,
        &presentation,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
    )
    .await
    .expect("input route");

    // Assert
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.as_ref().is_some_and(|editor| editor.kind == PlanEditorKind::New))
    );
    assert!(!app.has_visible_tick_driven_ui());
    for _ in 0..2 {
        handle_key_event(
            &mut app,
            &presentation,
            &mut terminal,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await
        .expect("close route");
    }
    assert!(matches!(app.mode, AppMode::View { .. }));
}
