use ag_session::GraphNodeContinuation;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::support::{TEST_TERMINAL_SIZE, handle};
use crate::app::App;
use crate::app::graph_question::tests::{awaiting_node_repository, unavailable, viewing_app};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn completed_graph_answers_reach_the_asking_node() {
    // Arrange
    let (mut repository, _execution) = awaiting_node_repository(usize::MAX);
    repository
        .expect_continue_node()
        .once()
        .withf(|request| {
            request.kind
                == GraphNodeContinuation::Reply {
                    attachments: Vec::new(),
                    message: "Clarifications:\n1. Q: Which update?\n   A: Docs".into(),
                }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = viewing_app(repository).await;
    app.reconcile_graph_question_mode().await;
    type_answer(&mut app, "Docs");

    // Act
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::View { session_id, .. } if session_id == "sess"
    ));
    assert_eq!(session_status(&app), Some(Status::Review));
}

#[tokio::test]
async fn retryable_graph_answer_rejection_reopens_the_final_question_with_its_answer() {
    // Arrange
    let (mut repository, _execution) = awaiting_node_repository(usize::MAX);
    repository
        .expect_continue_node()
        .once()
        .returning(|_| Err(unavailable()));
    let (mut app, _directory) = viewing_app(repository).await;
    app.reconcile_graph_question_mode().await;
    type_answer(&mut app, "Docs");

    // Act
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Question { input, responses, .. }
            if input.text() == "Docs" && responses.is_empty()
    ));
    assert!(app.graph_questions.active_target("sess").is_some());
}

#[tokio::test]
async fn stale_graph_answers_close_the_panel_and_release_its_target() {
    // Arrange
    let (repository, _execution) = awaiting_node_repository(1);
    let (mut app, _directory) = viewing_app(repository).await;
    app.reconcile_graph_question_mode().await;
    type_answer(&mut app, "Docs");

    // Act
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::View { session_id, .. } if session_id == "sess"
    ));
    assert!(app.graph_questions.active_target("sess").is_none());
}

#[tokio::test]
async fn ctrl_c_closes_graph_questions_without_ending_the_session_turn() {
    // Arrange
    let (repository, _execution) = awaiting_node_repository(usize::MAX);
    let (mut app, _directory) = viewing_app(repository).await;
    app.reconcile_graph_question_mode().await;

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::View { session_id, .. } if session_id == "sess"
    ));
    assert_eq!(session_status(&app), Some(Status::Review));
    assert!(app.graph_questions.active_target("sess").is_none());
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

fn session_status(app: &App) -> Option<Status> {
    app.sessions
        .session_for_id("sess")
        .map(|session| session.status)
}

fn type_answer(app: &mut App, answer: &str) {
    if let AppMode::Question { input, .. } = &mut app.mode {
        *input = InputState::with_text(answer.to_string());
    }
}
