use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::presentation::app_mode::{AppMode, DiffFocus, DiffLineComments, HelpContext};
use crate::presentation::help_action::{HelpAction, ViewSessionState};
use crate::runtime::EventResult;

#[test]
fn test_handle_question_mark_restores_list_mode() {
    // Arrange
    let mut mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: vec![HelpAction::new("quit", "q", "Quit")],
        },
        scroll_offset: 0,
    };

    // Act
    let result = handle(
        &mut mode,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(mode, AppMode::List));
}

#[test]
fn test_handle_quit_key_restores_view_mode() {
    // Arrange
    let mut mode = AppMode::Help {
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
        &mut mode,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(
        mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(5),
            ..
        } if session_id == "s1"
    ));
}

#[test]
fn test_handle_down_key_increments_scroll_offset() {
    // Arrange
    let mut mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: vec![HelpAction::new("quit", "q", "Quit")],
        },
        scroll_offset: 0,
    };

    // Act
    handle(&mut mode, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    // Assert
    assert!(matches!(
        mode,
        AppMode::Help {
            scroll_offset: 1,
            ..
        }
    ));
}

#[test]
fn test_handle_up_key_saturates_at_zero() {
    // Arrange
    let mut mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: vec![HelpAction::new("quit", "q", "Quit")],
        },
        scroll_offset: 0,
    };

    // Act
    handle(&mut mode, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

    // Assert
    assert!(matches!(
        mode,
        AppMode::Help {
            scroll_offset: 0,
            ..
        }
    ));
}

#[test]
fn test_handle_non_help_mode_leaves_mode_unchanged() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    let result = handle(
        &mut mode,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(mode, AppMode::List));
}

#[test]
fn test_handle_restores_diff_mode_with_content() {
    // Arrange
    let mut mode = AppMode::Help {
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
        &mut mode,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        mode,
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

#[test]
fn scrolling_aliases_saturate_and_unrelated_keys_preserve_help() {
    for (key, initial_offset, expected_offset) in [
        (KeyCode::Char('j'), 3, 4),
        (KeyCode::Down, u16::MAX, u16::MAX),
        (KeyCode::Char('k'), 3, 2),
        (KeyCode::Up, 0, 0),
        (KeyCode::Enter, 3, 3),
    ] {
        // Arrange
        let mut mode = AppMode::Help {
            context: HelpContext::List {
                keybindings: Vec::new(),
            },
            scroll_offset: initial_offset,
        };

        // Act
        let result = handle(&mut mode, KeyEvent::new(key, KeyModifiers::NONE));

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(
            mode,
            AppMode::Help { scroll_offset, .. } if scroll_offset == expected_offset
        ));
    }
}

#[test]
fn escape_restores_the_previous_list_mode() {
    // Arrange
    let mut mode = AppMode::Help {
        context: HelpContext::List {
            keybindings: Vec::new(),
        },
        scroll_offset: 7,
    };

    // Act
    handle(&mut mode, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // Assert
    assert!(matches!(mode, AppMode::List));
}
