use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{apply_view_scroll_and_output_mode, handle_with_cache};
use super::support::{new_test_app_with_session, session_replay_text};
use crate::domain::session::Status;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::presentation::app_mode::AppMode;
use crate::runtime::mode::chat_scroll::ChatScrollMetrics;
use crate::runtime::mode::{chat_scroll, session_output_metric};
use crate::ui::RenderCacheStore;

#[tokio::test]
async fn test_scroll_offset_down_does_not_jump_to_bottom_for_wrapped_output() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        "word ".repeat(60),
    )]);
    app.sessions.sessions_mut()[0].transcript = Some(transcript);
    let metrics = ChatScrollMetrics {
        total_lines: session_output_metric::tests::rendered_output_line_count(
            &app,
            &session_id,
            0,
            20,
            5,
        ),
        view_height: 5,
    };

    // Act
    let next_offset = chat_scroll::scroll_offset_down(Some(0), metrics, 1);

    // Assert
    assert_eq!(next_offset, Some(1));
}

#[tokio::test]
async fn test_view_total_lines_counts_wrapped_output_lines() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].transcript = Some(crate::test_support::assistant_transcript(
        "word ".repeat(40),
    ));
    let raw_line_count = u16::try_from(
        session_replay_text(&app.sessions.sessions()[0])
            .lines()
            .count(),
    )
    .unwrap_or(u16::MAX);

    // Act
    let total_lines =
        session_output_metric::tests::rendered_output_line_count(&app, &session_id, 0, 20, 5);

    // Assert
    assert!(total_lines > raw_line_count);
}

#[tokio::test]
async fn test_append_output_for_session_appends_text() {
    // Arrange
    let (app, _base_dir, session_id) = new_test_app_with_session().await;
    let mut app = app;

    // Act
    app.append_output_for_session(&session_id, "line one").await;

    // Assert
    app.sessions.sync_from_handles();
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert_eq!(output, "line one");
}

#[tokio::test]
async fn test_apply_view_scroll_and_output_mode_updates_scroll_state() {
    // Arrange
    let (mut app, _base_dir, expected_session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: expected_session_id.clone().into(),
        scroll_offset: Some(3),
    };

    // Act
    apply_view_scroll_and_output_mode(&mut app, Some(1));

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(1),
        } if session_id == &expected_session_id
    ));
}

#[tokio::test]
async fn test_scroll_keys_bypass_action_snapshot_and_keep_navigation_working() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Review)
        .build();
    let session_id = session.id.clone();
    app.sessions.push_session(session);
    app.sessions.sessions_mut()[0].transcript =
        Some(SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "```mermaid\ngraph TD\nA --> B\n```\n".repeat(100),
        )]));
    app.mode = AppMode::View {
        session_id,
        scroll_offset: Some(0),
    };
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
    let cache = RenderCacheStore::default();

    // Act
    for key in ['j', 'j', 'k'] {
        handle_with_cache(
            &mut app,
            &cache,
            &mut terminal,
            KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
        )
        .await
        .expect("scroll");
    }

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(1),
            ..
        }
    ));
    handle_with_cache(
        &mut app,
        &cache,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await
    .expect("leave session");
    assert!(matches!(app.mode, AppMode::List));
}
