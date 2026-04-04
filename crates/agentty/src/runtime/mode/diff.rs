use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::runtime::EventResult;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::state::app_mode::{AppMode, DiffScrollCache, DoneSessionOutputMode, HelpContext};
use crate::ui::util::{diff_view_max_scroll_offset, parse_diff_lines, selected_diff_lines};

/// Handles key input while the app is in `AppMode::Diff`.
///
/// File selection via `j`/`k` wraps around between the first and last file
/// explorer entries. Leaving diff mode restores the prior question snapshot
/// when present; otherwise it rebuilds session view with any cached focused
/// review output for the same session.
pub(crate) fn handle(app: &mut App, content_area: Rect, key: KeyEvent) -> EventResult {
    if key.code == KeyCode::Char('?') {
        let mode = std::mem::replace(&mut app.mode, AppMode::List);
        if let AppMode::Diff {
            diff,
            file_explorer_selected_index,
            restore_question,
            session_id,
            scroll_offset,
            ..
        } = mode
        {
            app.mode = AppMode::Help {
                context: HelpContext::Diff {
                    diff,
                    file_explorer_selected_index,
                    restore_question,
                    session_id,
                    scroll_offset,
                },
                scroll_offset: 0,
            };
        } else {
            app.mode = mode;
        }

        return EventResult::Continue;
    }

    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        let mode = std::mem::replace(&mut app.mode, AppMode::List);
        if let AppMode::Diff {
            restore_question,
            session_id,
            ..
        } = mode
        {
            app.mode = if let Some(snapshot) = restore_question {
                snapshot.into_question_mode()
            } else {
                let (review_status_message, review_text) = app.review_view_state(&session_id);

                AppMode::View {
                    done_session_output_mode: DoneSessionOutputMode::Summary,
                    review_status_message,
                    review_text,
                    session_id,
                    scroll_offset: None,
                }
            };
        } else {
            app.mode = mode;
        }

        return EventResult::Continue;
    }

    if let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        scroll_cache,
        scroll_offset,
        ..
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('j') if is_plain_char_key(key, 'j') => {
                let parsed = parse_diff_lines(diff);
                let count = FileExplorer::count_items(&parsed);
                let new_index =
                    FileExplorer::next_selected_index(*file_explorer_selected_index, count);

                if *file_explorer_selected_index != new_index {
                    *file_explorer_selected_index = new_index;
                    *scroll_cache = None;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Char('k') if is_plain_char_key(key, 'k') => {
                let parsed = parse_diff_lines(diff);
                let count = FileExplorer::count_items(&parsed);
                let new_index =
                    FileExplorer::previous_selected_index(*file_explorer_selected_index, count);

                if *file_explorer_selected_index != new_index {
                    *file_explorer_selected_index = new_index;
                    *scroll_cache = None;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down | KeyCode::Char('J' | 'j')
                if key.code == KeyCode::Down || is_shift_char_key(key, 'j') =>
            {
                let max_scroll_offset = diff_max_scroll_offset(
                    diff,
                    content_area,
                    *file_explorer_selected_index,
                    scroll_cache,
                );
                let clamped_scroll_offset = (*scroll_offset).min(max_scroll_offset);

                *scroll_offset = clamped_scroll_offset
                    .saturating_add(1)
                    .min(max_scroll_offset);
            }
            KeyCode::Up | KeyCode::Char('K' | 'k')
                if key.code == KeyCode::Up || is_shift_char_key(key, 'k') =>
            {
                let max_scroll_offset = diff_max_scroll_offset(
                    diff,
                    content_area,
                    *file_explorer_selected_index,
                    scroll_cache,
                );
                let clamped_scroll_offset = (*scroll_offset).min(max_scroll_offset);

                *scroll_offset = clamped_scroll_offset.saturating_sub(1);
            }
            _ => {}
        }
    }

    EventResult::Continue
}

/// Returns true when the key event is a plain character key with no
/// modifiers.
fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == KeyModifiers::NONE
}

/// Returns true when the key event is a shifted character key, accepting both
/// uppercase and lowercase char payloads emitted by terminals.
fn is_shift_char_key(key: KeyEvent, character: char) -> bool {
    let lowercase_character = character.to_ascii_lowercase();
    let uppercase_character = character.to_ascii_uppercase();

    key.modifiers == KeyModifiers::SHIFT
        && matches!(
            key.code,
            KeyCode::Char(pressed)
                if pressed == lowercase_character || pressed == uppercase_character
        )
}

/// Returns the max valid scroll offset for the active diff selection.
fn diff_max_scroll_offset(
    diff: &str,
    content_area: Rect,
    selected_index: usize,
    scroll_cache: &mut Option<DiffScrollCache>,
) -> u16 {
    if let Some(cached_scroll_limit) = scroll_cache
        && cached_scroll_limit.content_area == content_area
        && cached_scroll_limit.file_explorer_selected_index == selected_index
    {
        return cached_scroll_limit.max_scroll_offset;
    }

    let parsed_lines = parse_diff_lines(diff);
    let tree_items = FileExplorer::file_tree_items(&parsed_lines);
    let selected_lines = selected_diff_lines(&parsed_lines, &tree_items, selected_index);
    let max_scroll_offset = diff_view_max_scroll_offset(&selected_lines, content_area);

    *scroll_cache = Some(DiffScrollCache {
        content_area,
        file_explorer_selected_index: selected_index,
        max_scroll_offset,
    });

    max_scroll_offset
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;
    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;

    const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 12);

    /// Builds one client bundle with deterministic agent availability for
    /// test app startup.
    fn test_app_clients() -> crate::app::AppClients {
        crate::app::AppClients::new().with_agent_availability_probe(std::sync::Arc::new(
            crate::infra::agent::StaticAgentAvailabilityProbe {
                available_agent_kinds: crate::domain::agent::AgentKind::ALL.to_vec(),
            },
        ))
    }

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");

        (app, base_dir)
    }

    /// Returns a diff long enough to keep the diff pane scrollable in tests.
    fn scrollable_diff_fixture() -> String {
        (0..40)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn test_handle_quit_key_returns_to_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_key_restores_cached_review_output() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.review_cache.insert(
            "session-id".to_string(),
            crate::app::ReviewCacheEntry::Ready {
                text: "Focused review".to_string(),
                diff_hash: 7,
            },
        );
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                review_status_message: None,
                review_text: Some(ref review_text),
                scroll_offset: None,
                ..
            } if session_id == "session-id" && review_text == "Focused review"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_shift_j_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 3,
            file_explorer_selected_index: 2,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 4,
                file_explorer_selected_index: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_saturates_scroll_offset_at_zero() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_shift_k_saturates_scroll_offset_at_zero() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 2,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_j_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_j_wraps_file_selection_from_last_to_first() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_wraps_file_selection_from_first_to_last() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 5,
            file_explorer_selected_index: 3,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    ref session_id,
                    ref diff,
                    scroll_offset: 5,
                    file_explorer_selected_index: 3,
                    ..
                },
                scroll_offset: 0,
            } if session_id == "session-id" && diff == "diff output"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_clamps_scroll_offset_at_bottom() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        let diff = scrollable_diff_fixture();
        let max_scroll_offset = diff_max_scroll_offset(&diff, TEST_TERMINAL_SIZE, 0, &mut None);
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff,
            scroll_offset: max_scroll_offset,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset,
                ..
            } if scroll_offset == max_scroll_offset
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_recovers_immediately_from_overscrolled_state() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        let diff = scrollable_diff_fixture();
        let max_scroll_offset = diff_max_scroll_offset(&diff, TEST_TERMINAL_SIZE, 0, &mut None);
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff,
            scroll_offset: u16::MAX,
            file_explorer_selected_index: 0,
            restore_question: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset,
                ..
            } if scroll_offset == max_scroll_offset.saturating_sub(1)
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_question_snapshot_restores_question_mode() {
        // Arrange — diff opened from question mode carries a snapshot.
        use crate::domain::input::InputState;
        use crate::infra::agent::protocol::QuestionItem;
        use crate::ui::state::app_mode::QuestionModeSnapshot;

        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-q".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            restore_question: Some(QuestionModeSnapshot {
                at_mention_state: None,
                current_index: 0,
                input: InputState::default(),
                questions: vec![QuestionItem {
                    options: Vec::new(),
                    text: "Q?".to_string(),
                }],
                responses: Vec::new(),
                scroll_offset: None,
                selected_option_index: None,
                session_id: "session-q".to_string(),
            }),
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — restored to Question mode, not View.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                focus: crate::ui::state::app_mode::QuestionFocus::Answer,
                ..
            } if session_id == "session-q"
        ));
    }

    #[tokio::test]
    async fn test_handle_question_then_help_then_exit_preserves_restore_question() {
        // Arrange — diff opened from question mode, then user opens help with `?`.
        use crate::domain::input::InputState;
        use crate::infra::agent::protocol::QuestionItem;
        use crate::ui::state::app_mode::QuestionModeSnapshot;

        let (mut app, _base_dir) = new_test_app().await;
        let snapshot = QuestionModeSnapshot {
            at_mention_state: None,
            current_index: 1,
            input: InputState::default(),
            questions: vec![
                QuestionItem {
                    options: Vec::new(),
                    text: "Q1?".to_string(),
                },
                QuestionItem {
                    options: Vec::new(),
                    text: "Q2?".to_string(),
                },
            ],
            responses: vec!["answer-1".to_string()],
            scroll_offset: None,
            selected_option_index: None,
            session_id: "session-q".to_string(),
        };

        app.mode = AppMode::Diff {
            session_id: "session-q".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 3,
            file_explorer_selected_index: 1,
            restore_question: Some(snapshot),
            scroll_cache: None,
        };

        // Act — open help overlay.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Intermediate assert — help carries the snapshot.
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    restore_question: Some(_),
                    ..
                },
                ..
            }
        ));

        // Act — close help overlay, returning to diff.
        crate::runtime::mode::help::handle(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Intermediate assert — diff still carries the snapshot.
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                restore_question: Some(_),
                ..
            }
        ));

        // Act — exit diff.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert — restored to Question mode, not View.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                current_index: 1,
                focus: crate::ui::state::app_mode::QuestionFocus::Answer,
                ..
            } if session_id == "session-q"
        ));
    }
}
