use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the app is showing the help overlay.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    if let AppMode::Help {
        scroll_offset,
        context: _,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('?' | 'q') | KeyCode::Esc => {
                let mode = std::mem::replace(&mut app.mode, AppMode::List);
                if let AppMode::Help { context, .. } = mode {
                    app.mode = context.restore_mode();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                *scroll_offset = scroll_offset.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll_offset = scroll_offset.saturating_sub(1);
            }
            _ => {}
        }
    }

    EventResult::Continue
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::presentation::app_mode::{DiffFocus, DiffLineComments, HelpContext};
    use crate::presentation::help_action::{HelpAction, ViewSessionState};

    #[tokio::test]
    async fn test_handle_question_mark_restores_list_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::List {
                keybindings: vec![HelpAction::new("quit", "q", "Quit")],
            },
            scroll_offset: 0,
        };

        // Act
        let result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_quit_key_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::View {
                can_fork_session: true,
                can_merge_session_branch: true,
                can_mutate_session_branch: true,
                can_open_worktree: true,
                can_rebase_session_branch: true,
                can_show_diff: true,
                can_reply_to_session: true,
                can_start_staged_session: false,
                can_view_review_comments: false,
                publish_pull_request_action: None,
                session_id: "s1".into(),
                session_state: ViewSessionState::Interactive,
                scroll_offset: Some(5),
            },
            scroll_offset: 0,
        };

        // Act
        let result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(5),
                ..
            } if session_id == "s1"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::List {
                keybindings: vec![HelpAction::new("quit", "q", "Quit")],
            },
            scroll_offset: 0,
        };

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_saturates_at_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::List {
                keybindings: vec![HelpAction::new("quit", "q", "Quit")],
            },
            scroll_offset: 0,
        };

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_non_help_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_restores_diff_mode_with_content() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                can_comment: true,
                session_id: "s1".into(),
                diff: "diff content".to_string(),
                focus: DiffFocus::Content,
                line_comments: DiffLineComments::default(),
                preview: crate::presentation::app_mode::DiffPreview::default(),
                review_comments: None,
                restore: None,
                scroll_offset: 7,
                selected_diff_line_index: 4,
                file_explorer_selected_index: 0,
            },
            scroll_offset: 3,
        };

        // Act
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                ref session_id,
                ref diff,
                restore: None,
                scroll_cache: None,
                scroll_offset: 7,
                file_explorer_selected_index: 0,
                focus: DiffFocus::Content,
                selected_diff_line_index: 4,
                ..
            } if session_id == "s1" && diff == "diff content"
        ));
    }
}
