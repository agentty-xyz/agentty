use std::cell::Cell;
use std::collections::HashMap;
use std::io;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::layout::Rect;

use super::{
    ChatFocusAction, ChatScrollBatch, ChatScrollMetrics, apply_scroll_key,
    classify_chat_focus_action, is_scroll_key, scroll_offset_down, scroll_offset_up,
    toggle_chat_focus,
};
use crate::app::App;
use crate::app::session_state::SessionGitStatus;
use crate::domain::input::InputState;
use crate::domain::session::{SessionId, SessionRole, Status};
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashState,
};
use crate::test_support::SessionFixtureBuilder;
use crate::ui::RenderCacheStore;

/// Builds a plain key press without modifiers.
fn plain_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn test_is_scroll_key_accepts_scroll_keys_and_rejects_other_keys() {
    // Arrange
    let scroll_keys = [
        plain_key(KeyCode::Char('j')),
        plain_key(KeyCode::Up),
        plain_key(KeyCode::Char('G')),
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    ];
    let other_keys = [
        plain_key(KeyCode::Char('x')),
        plain_key(KeyCode::Char('d')),
        plain_key(KeyCode::Enter),
    ];

    // Act, Assert
    assert!(scroll_keys.into_iter().all(is_scroll_key));
    assert!(!other_keys.into_iter().any(is_scroll_key));
}

#[test]
fn test_classify_chat_focus_action_distinguishes_input_and_chat_keys() {
    // Arrange
    let input_key = plain_key(KeyCode::Char('q'));
    let chat_keys = [
        (plain_key(KeyCode::Tab), ChatFocusAction::ToggleFocus),
        (plain_key(KeyCode::Char('d')), ChatFocusAction::OpenDiff),
        (plain_key(KeyCode::Char('j')), ChatFocusAction::Scroll),
        (plain_key(KeyCode::Esc), ChatFocusAction::Swallow),
    ];

    // Act, Assert
    assert_eq!(
        classify_chat_focus_action(ChatFocus::Input, input_key),
        None
    );
    assert_eq!(
        classify_chat_focus_action(ChatFocus::Input, plain_key(KeyCode::Tab)),
        Some(ChatFocusAction::ToggleFocus)
    );
    for (key, action) in chat_keys {
        assert_eq!(
            classify_chat_focus_action(ChatFocus::Chat, key),
            Some(action)
        );
    }
}

#[test]
fn test_classify_chat_focus_action_keeps_ctrl_d_as_scroll() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

    // Act
    let action = classify_chat_focus_action(ChatFocus::Chat, key);

    // Assert
    assert_eq!(action, Some(ChatFocusAction::Scroll));
}

#[test]
fn test_toggle_chat_focus_switches_between_panels() {
    // Arrange
    let mut focus = ChatFocus::Input;

    // Act
    toggle_chat_focus(&mut focus);
    let chat_focus = focus;
    toggle_chat_focus(&mut focus);

    // Assert
    assert_eq!(chat_focus, ChatFocus::Chat);
    assert_eq!(focus, ChatFocus::Input);
}

#[test]
fn test_apply_scroll_key_steps_down_one_line() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };
    let mut scroll_offset = Some(0);

    // Act
    let is_consumed = apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('j')));

    // Assert
    assert!(is_consumed);
    assert_eq!(scroll_offset, Some(1));
}

#[test]
fn test_apply_scroll_key_steps_up_one_line() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };
    let mut scroll_offset = Some(5);

    // Act
    let is_consumed = apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('k')));

    // Assert
    assert!(is_consumed);
    assert_eq!(scroll_offset, Some(4));
}

#[test]
fn test_apply_scroll_key_jumps_to_top_and_bottom() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };
    let mut scroll_offset = None;

    // Act
    apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('g')));
    let top_offset = scroll_offset;
    apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('G')));

    // Assert
    assert_eq!(top_offset, Some(0));
    assert_eq!(scroll_offset, None);
}

#[test]
fn test_apply_scroll_key_scrolls_half_page_up_from_bottom() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };
    let mut scroll_offset = None;

    // Act
    let is_consumed = apply_scroll_key(
        &mut scroll_offset,
        metrics,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );

    // Assert
    assert!(is_consumed);
    assert_eq!(scroll_offset, Some(15));
}

#[test]
fn test_apply_scroll_key_scrolls_half_page_down() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };
    let mut scroll_offset = Some(5);

    // Act
    let is_consumed = apply_scroll_key(
        &mut scroll_offset,
        metrics,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );

    // Assert
    assert!(is_consumed);
    assert_eq!(scroll_offset, Some(10));
}

#[test]
fn test_apply_scroll_key_ignores_unrelated_keys() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };
    let mut scroll_offset = Some(3);

    // Act
    let is_consumed = apply_scroll_key(&mut scroll_offset, metrics, plain_key(KeyCode::Char('x')));

    // Assert
    assert!(!is_consumed);
    assert_eq!(scroll_offset, Some(3));
}

#[test]
fn test_scroll_offset_down_returns_none_at_end_of_content() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 20,
        view_height: 10,
    };

    // Act
    let next_offset = scroll_offset_down(Some(9), metrics, 1);

    // Assert
    assert_eq!(next_offset, None);
}

#[test]
fn test_scroll_offset_up_uses_bottom_when_scroll_is_unset() {
    // Arrange
    let metrics = ChatScrollMetrics {
        total_lines: 30,
        view_height: 10,
    };

    // Act
    let next_offset = scroll_offset_up(None, metrics, 5);

    // Assert
    assert_eq!(next_offset, 15);
}

#[tokio::test]
async fn test_metrics_use_footer_only_viewport_when_session_is_missing() {
    // Arrange
    let (app, _base_dir) = crate::test_support::new_test_app().await;
    let missing_session_id = SessionId::from("missing-session");
    let terminal_size = Rect::new(0, 0, 80, 24);

    // Act
    let metrics = ChatScrollMetrics::new(
        &app,
        &RenderCacheStore::default(),
        &missing_session_id,
        0,
        terminal_size,
    );

    // Assert
    let empty_metrics = ChatScrollMetrics::empty(terminal_size);
    assert_eq!(metrics.total_lines, 0);
    assert_eq!(metrics.view_height, empty_metrics.view_height);
}

#[tokio::test]
async fn test_metrics_reserve_header_row_for_merge_conflict_alert() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let session = SessionFixtureBuilder::new().status(Status::Review).build();
    let session_id = session.id.clone();
    app.sessions.push_session(session);
    app.mode = crate::presentation::app_mode::AppMode::View {
        scroll_offset: None,
        session_id: session_id.clone(),
    };
    let terminal_size = Rect::new(0, 0, 80, 24);
    let normal_metrics = ChatScrollMetrics::new(
        &app,
        &RenderCacheStore::default(),
        &session_id,
        0,
        terminal_size,
    );
    app.sessions.replace_session_git_statuses(HashMap::from([(
        session_id.clone(),
        SessionGitStatus {
            base_status: Some((1, 1)),
            has_merge_conflict: Some(true),
            remote_status: None,
        },
    )]));

    // Act
    let conflict_metrics = ChatScrollMetrics::new(
        &app,
        &RenderCacheStore::default(),
        &session_id,
        0,
        terminal_size,
    );

    // Assert
    assert_eq!(conflict_metrics.view_height + 1, normal_metrics.view_height);
}

#[tokio::test]
async fn test_orchestrator_metrics_reserve_campaign_board_height() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let mut session = SessionFixtureBuilder::new()
        .role(SessionRole::Orchestrator)
        .status(Status::Review)
        .transcript(
            (0..60)
                .map(|index| format!("line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .build();
    session.orchestration_progress =
        Some("Phase: AwaitingApproval\nParallel workers: 3\n1. ui - awaiting approval".to_string());
    let session_id = session.id.clone();
    app.sessions.push_session(session);
    app.mode = crate::presentation::app_mode::AppMode::View {
        scroll_offset: None,
        session_id: session_id.clone(),
    };
    let terminal_size = Rect::new(0, 0, 80, 24);

    // Act
    let orchestrator_metrics = ChatScrollMetrics::new(
        &app,
        &RenderCacheStore::default(),
        &session_id,
        0,
        terminal_size,
    );
    app.sessions.sessions_mut()[0].role = SessionRole::Worker;
    let regular_metrics = ChatScrollMetrics::new(
        &app,
        &RenderCacheStore::default(),
        &session_id,
        0,
        terminal_size,
    );

    // Assert
    assert_eq!(
        orchestrator_metrics.view_height + 7,
        regular_metrics.view_height
    );
}

#[tokio::test]
async fn test_scroll_batch_measures_once_and_preserves_key_steps() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::View {
        session_id: SessionId::from("scroll-batch"),
        scroll_offset: Some(0),
    };
    let mut batch = ChatScrollBatch::default();
    let mut measurements = 0;

    // Act
    for _ in 0..5 {
        assert!(
            batch
                .handle_with_measurement(&mut app, plain_key(KeyCode::Char('j')), |_, _| {
                    measurements += 1;

                    Ok(Some(ChatScrollMetrics {
                        total_lines: 100,
                        view_height: 10,
                    }))
                })
                .expect("scroll should succeed")
        );
    }

    // Assert
    assert_eq!(measurements, 1);
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(5),
            ..
        }
    ));
}

#[tokio::test]
async fn test_scroll_batch_ignores_non_scroll_and_unfocused_keys() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let mut batch = ChatScrollBatch::default();
    let measurements = Cell::new(0);
    let measure_missing = |_: &App, _: &SessionId| {
        measurements.set(measurements.get() + 1);

        Ok(None)
    };

    // Act
    let ignored_action =
        batch.handle_with_measurement(&mut app, plain_key(KeyCode::Char('q')), measure_missing);
    let ignored_list_scroll =
        batch.handle_with_measurement(&mut app, plain_key(KeyCode::Char('j')), measure_missing);
    assert_eq!(measurements.get(), 0);
    app.mode = AppMode::View {
        session_id: SessionId::from("missing"),
        scroll_offset: Some(0),
    };
    let missing_session =
        batch.handle_with_measurement(&mut app, plain_key(KeyCode::Char('j')), measure_missing);
    let failed_measurement =
        batch.handle_with_measurement(&mut app, plain_key(KeyCode::Char('j')), |_, _| {
            Err(io::Error::other("size unavailable"))
        });

    // Assert
    assert!(!ignored_action.expect("ignored action"));
    assert!(!ignored_list_scroll.expect("ignored list scroll"));
    assert!(!missing_session.expect("missing session falls through"));
    assert_eq!(measurements.get(), 1);
    assert_eq!(
        failed_measurement
            .expect_err("measurement error")
            .to_string(),
        "size unavailable"
    );
    assert!(batch.metrics.is_none());
}

#[tokio::test]
async fn test_scroll_batch_missing_session_does_not_cache_empty_metrics() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let session = SessionFixtureBuilder::new()
        .status(Status::Review)
        .transcript("line\n".repeat(60))
        .build();
    app.mode = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: Some(0),
    };
    let terminal =
        Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("test terminal");
    let cache = RenderCacheStore::default();
    let mut batch = ChatScrollBatch::default();
    let key = plain_key(KeyCode::Char('j'));

    // Act
    let missing = batch.handle(&mut app, &cache, &terminal, key);
    app.sessions.push_session(session);
    let loaded = batch.handle(&mut app, &cache, &terminal, key);

    // Assert
    assert!(!missing.expect("missing session falls through"));
    assert!(loaded.expect("loaded session scrolls"));
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(1),
            ..
        }
    ));
}

#[tokio::test]
async fn test_scroll_batch_respects_prompt_and_question_focus() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let prompt = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Chat,
        history_state: PromptHistoryState::default(),
        input: InputState::default(),
        scroll_offset: Some(0),
        session_id: SessionId::from("focused"),
        slash_state: PromptSlashState::default(),
    };
    let question = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Chat,
        input: InputState::default(),
        questions: Vec::new(),
        responses: Vec::new(),
        scroll_offset: Some(0),
        selected_option_index: None,
        session_id: SessionId::from("focused"),
    };

    // Act
    for mode in [prompt, question] {
        app.mode = mode;
        let mut batch = ChatScrollBatch::default();
        let handled =
            batch.handle_with_measurement(&mut app, plain_key(KeyCode::Char('j')), |_, _| {
                Ok(Some(ChatScrollMetrics {
                    total_lines: 30,
                    view_height: 10,
                }))
            });

        // Assert
        assert!(handled.expect("focused scroll"));
        if let AppMode::Prompt {
            focus,
            scroll_offset,
            ..
        }
        | AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            assert_eq!(*scroll_offset, Some(1));
            *focus = ChatFocus::Input;
        }
        let ignored =
            batch.handle_with_measurement(&mut app, plain_key(KeyCode::Char('j')), |_, _| {
                Err(io::Error::other("input must not measure"))
            });
        assert!(!ignored.expect("input key falls through"));

        if let AppMode::Prompt {
            at_mention_state,
            focus,
            ..
        } = &mut app.mode
        {
            *focus = ChatFocus::Chat;
            *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
            let dropdown_key =
                batch.handle_with_measurement(&mut app, plain_key(KeyCode::Down), |_, _| {
                    Err(io::Error::other("dropdown must keep arrow-key precedence"))
                });
            assert!(!dropdown_key.expect("dropdown key falls through"));
        }
    }
}
