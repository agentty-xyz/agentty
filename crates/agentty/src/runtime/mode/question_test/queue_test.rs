use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::submit_response;
use super::support::{TEST_TERMINAL_SIZE, handle};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::Status;
use crate::presentation::app_mode::{AppMode, ChatFocus};

#[tokio::test]
async fn submit_response_advances_session_when_reply_is_enqueued() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Question);
    let questions = vec![QuestionItem::new("Which tests should be added?")];
    app.sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should be loaded")
        .questions = questions.clone();
    app.services
        .db()
        .sessions()
        .update_session_questions(
            &session_id,
            &serde_json::to_string(&questions).expect("questions should serialize"),
        )
        .await
        .expect("questions should persist");
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions,
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: session_id.clone().into(),
    };

    // Act
    submit_response(&mut app, "Unit and integration tests".to_string()).await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    assert_eq!(session.status, Status::InProgress);
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: None,
        } if session_id == &session.id
    ));
}

#[tokio::test]
async fn submit_response_keeps_question_status_when_reply_is_not_enqueued() {
    // Arrange — a viewed `Question` session with no runtime handle, so the
    // reply cannot be enqueued on any worker and `App::reply` reports
    // failure.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-no-handle";
    let questions = vec![
        QuestionItem::with_options(
            "Use the default target branch?",
            vec!["Yes".to_string(), "No".to_string()],
        ),
        QuestionItem::new("Which tests should be added?"),
    ];
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(session_id)
            .folder(std::path::PathBuf::from("/tmp/test"))
            .status(Status::Question)
            .questions(questions.clone())
            .build(),
    );
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: session_id.into(),
        questions,
        responses: vec!["Yes".to_string()],
        current_index: 1,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act — answer the final question so `submit_response` attempts the
    // reply.
    submit_response(&mut app, "Unit and integration tests".to_string()).await;

    // Assert — the failed reply leaves the session on `Question` instead of
    // stranding it behind an optimistic `InProgress` no worker will
    // advance, and restores the panel with the completed answers
    // ready to retry.
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    assert_eq!(session.status, Status::Question);
    assert!(matches!(
        app.mode,
        AppMode::Question {
            current_index: 1,
            ref input,
            ref questions,
            ref responses,
            selected_option_index: None,
            ..
        } if questions.len() == 2
            && responses == &vec!["Yes".to_string()]
            && input.text() == "Unit and integration tests"
    ));
}

#[tokio::test]
async fn test_handle_enter_on_last_question_restores_answer_when_reply_is_not_enqueued() {
    // Arrange — free-text mode on last question with no matching session
    // handle.
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        session_id: "missing-session".into(),
        questions: vec![QuestionItem {
            options: vec!["Today".to_string(), "Tomorrow".to_string()],
            text: "Need exact date?".to_string(),
        }],
        responses: Vec::new(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::with_text("March 4, 2026".to_string()),
        scroll_offset: None,
        selected_option_index: None,
    };

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            current_index: 0,
            ref input,
            ref questions,
            ref responses,
            selected_option_index: None,
            ref session_id,
            ..
        } if session_id == "missing-session"
            && questions.len() == 1
            && responses.is_empty()
            && input.text() == "March 4, 2026"
    ));
}
