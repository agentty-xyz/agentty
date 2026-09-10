use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::handle_key_event;
use crate::presentation::app_mode::AppMode;
use crate::runtime::{EventResult, PresentationState};

#[tokio::test]
async fn test_handle_key_event_routes_question_input_through_terminal_bounds() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: crate::presentation::app_mode::ChatFocus::Input,
        input: crate::domain::input::InputState::default(),
        questions: vec![crate::domain::question::QuestionItem::new("Which branch?")],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "session-id".into(),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let event_result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::Question { ref input, .. } if input.text() == "x"
    ));
}
