use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::super::{
    handle_line_comment_edit_key, should_submit_line_comments, sync_comment_at_mention,
};
use super::support::{
    TEST_TERMINAL_SIZE, aligned_file_diff_fixture, diff_mode_fixture, enter_diff_mode, handle,
    preview_test_app, scrollable_diff_fixture,
};
use crate::app::AppEvent;
use crate::domain::input::InputState;
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineCommentAnchor, DiffLineCommentTarget,
    DiffLineComments, DiffPreview, DiffSidebarFocus, HelpContext,
};
use crate::runtime::EventResult;
use crate::ui::page;

#[tokio::test]
async fn test_handle_escape_returns_changed_line_focus_to_files() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        diff: scrollable_diff_fixture(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 4,
        selected_diff_line_index: 8,
        session_id: "session-id".into(),
    };

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            focus: DiffFocus::Files,
            selected_diff_line_index: 8,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_non_diff_mode_leaves_mode_unchanged() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
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
async fn test_handle_down_key_selects_next_changed_line() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: scrollable_diff_fixture(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
            scroll_offset: 0,
            selected_diff_line_index: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_down_key_clamps_changed_line_at_bottom() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let diff = scrollable_diff_fixture();
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff,
        scroll_offset: u16::MAX,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 39,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
            selected_diff_line_index: 39,
            ..
        } if scroll_offset < u16::MAX
    ));
}

#[tokio::test]
async fn test_handle_l_focuses_changed_line_aligned_with_selected_file_row() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let diff = aligned_file_diff_fixture();
    app.mode = AppMode::Diff {
        diff: diff.clone(),
        file_explorer_selected_index: 7,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 2,
        selected_diff_line_index: 0,
        session_id: "session-id".into(),
    };
    let content_area = Rect::new(0, 0, 80, 20);

    // Act
    handle(
        &mut app,
        content_area,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            selected_diff_line_index: 7,
            ..
        }
    ));
    let selected_anchor = page::diff::DiffLayoutCache::default()
        .content(&diff)
        .selected_changed_line(7, 7)
        .expect("aligned changed line should resolve");
    assert_eq!(selected_anchor.line, 40);
}

#[tokio::test]
async fn test_handle_collects_inline_comments_before_building_next_turn() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -1 +1,2 @@\n",
        " fn main() {}\n",
        "+println!(\"review\");\n",
        "+review();\n",
    );
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert — both comments stay inside Diff mode until explicit submit.
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.comments.len() == 2
                && line_comments.comments[0].input.text() == "A"
                && line_comments.comments[1].input.text() == "B"
    ));

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            focus: crate::presentation::app_mode::ChatFocus::Input,
            input,
            session_id,
            ..
        } if session_id == "session-id"
            && input.text() == concat!(
                "Line comments:\n",
                "- src/main.rs:2 [new]: A\n",
                "- src/main.rs:3 [new]: B",
            )
    ));
}

#[tokio::test]
async fn test_handle_selects_inline_comment_and_reopens_editor_on_enter() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -0,0 +1,2 @@\n",
        "+first();\n",
        "+second();\n",
    );
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "first();".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        }));
        line_comments
            .editing_input_mut()
            .expect("seeded inline comment should be editable")
            .insert_text("Explain this");
        line_comments.finish_editing();
        line_comments.clear_comment_selection();
    }

    // Act — move from the source row to its comment, back, and onto the
    // comment again.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    let selected_comment = matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.selected_comment_index() == Some(0)
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );
    let selected_source = matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.selected_comment_index().is_none()
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(selected_comment);
    assert!(selected_source);
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            line_comments,
            selected_diff_line_index: 0,
            ..
        } if line_comments.is_editing()
            && line_comments.selected_comment_index() == Some(0)
            && line_comments.comments[0].input.text() == "Explain this"
    ));
}

#[tokio::test]
async fn test_handle_navigation_key_in_non_diff_mode_leaves_mode_unchanged() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::List;

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );

    // Assert — navigation keys are a no-op outside diff mode.
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_up_key_saturates_changed_line_at_zero() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: scrollable_diff_fixture(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
            selected_diff_line_index: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_j_wraps_file_selection_from_last_to_first() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
        scroll_offset: 10,
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
async fn test_handle_shift_j_selects_next_changed_line() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: scrollable_diff_fixture(),
        scroll_offset: 3,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 2,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
            file_explorer_selected_index: 0,
            selected_diff_line_index: 3,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_shift_k_saturates_changed_line_at_zero() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: scrollable_diff_fixture(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Content,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
            file_explorer_selected_index: 0,
            selected_diff_line_index: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_shift_v_selects_changed_rows_for_one_comment() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -0,0 +1,3 @@\n",
        "+first();\n",
        "+second();\n",
        "+third();\n",
    );
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
    let expected_target = DiffLineCommentTarget::from_anchors(vec![
        DiffLineCommentAnchor {
            content: "first();".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        },
        DiffLineCommentAnchor {
            content: "second();".to_string(),
            line: 2,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        },
    ])
    .expect("first two changed rows should create a range target");

    // Act — start downward selection, then cancel without leaving content
    // focus.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            line_comments,
            selected_diff_line_index: 1,
            ..
        } if !line_comments.is_selecting()
    ));

    // Act — select upward from the second row and open one range editor.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            line_comments,
            selected_diff_line_index: 0,
            ..
        } if line_comments.is_editing()
            && line_comments.is_selecting()
            && line_comments.comments[0].target
                == DiffCommentTarget::from(expected_target)
    ));

    // Act — completed comments cannot submit while a new range is active.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if !line_comments.is_editing() && !line_comments.is_selecting()
    ));

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
    );
    let should_submit =
        should_submit_line_comments(&app, KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

    // Assert
    assert!(!should_submit);
}

#[tokio::test]
async fn test_handle_k_wraps_file_selection_from_first_to_last() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
        scroll_offset: 10,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
async fn test_handle_unhandled_key_keeps_diff_mode_unchanged() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: "diff output".to_string(),
        scroll_offset: 4,
        file_explorer_selected_index: 2,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
    };

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );

    // Assert — an unhandled key leaves the diff selection and scroll
    // intact.
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
async fn test_handle_enter_and_l_from_files_focus_first_changed_line() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;

    // Act, Assert
    for key_code in [KeyCode::Enter, KeyCode::Char('l')] {
        app.mode = AppMode::Diff {
            diff: scrollable_diff_fixture(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 7,
            session_id: "session-id".into(),
        };
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(key_code, KeyModifiers::NONE),
        );

        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                line_comments: DiffLineComments {
                    editing_index: None,
                    ref comments,
                    ..
                },
                selected_diff_line_index: 0,
                ..
            } if comments.is_empty()
        ));
    }
}

#[tokio::test]
async fn test_handle_enter_ignores_folders_and_files_without_changed_lines() {
    // Arrange
    let (mut folder_app, _folder_base_dir) = crate::test_support::new_test_app().await;
    let (mut unchanged_app, _unchanged_base_dir) = crate::test_support::new_test_app().await;
    folder_app.mode = diff_mode_fixture(
        "diff --git a/src/main.rs b/src/main.rs\n+added",
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );
    unchanged_app.mode = diff_mode_fixture(
        "diff --git a/README.md b/README.md\n@@ -1 +1 @@\n unchanged",
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );

    // Act
    for app in [&mut folder_app, &mut unchanged_app] {
        handle(
            app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
    }

    // Assert
    assert!(matches!(
        folder_app.mode,
        AppMode::Diff {
            focus: DiffFocus::Files,
            ..
        }
    ));
    assert!(matches!(
        unchanged_app.mode,
        AppMode::Diff {
            focus: DiffFocus::Files,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_comments_on_selected_file_from_active_row_selection() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "@@ -0,0 +1 @@\n",
        "+fn main() {}\n",
    );
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());

    // Act — start a row selection from inside the file, then replace it
    // with a whole-file comment.
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
    );
    let selection_cleared_while_editing = matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.is_editing() && !line_comments.is_selecting()
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(selection_cleared_while_editing);
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            line_comments,
            ..
        } if line_comments.comments.len() == 1
            && line_comments.comments[0].target
                == DiffCommentTarget::file("src/main.rs")
            && line_comments.comments[0].input.text() == "R"
            && !line_comments.is_selecting()
    ));

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. }
            if input.text() == "File comments:\n- src/main.rs: R"
    ));
}

#[tokio::test]
async fn test_handle_modified_enter_inserts_diff_comment_newline() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.is_editing()
                && line_comments.comments[0].input.text() == "A\nB"
    ));

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. } if !line_comments.is_editing()
    ));
}

#[tokio::test]
async fn test_comment_lookup_selection_and_dismissal() {
    for code in [
        KeyCode::Tab,
        KeyCode::Enter,
        KeyCode::Char('\r'),
        KeyCode::Char('\n'),
        KeyCode::Esc,
    ] {
        for has_match in [true, false] {
            // Arrange
            let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
            app.mode = diff_mode_fixture(
                "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
                1,
                DiffFocus::Content,
                DiffPreview::default(),
            );
            if let AppMode::Diff { line_comments, .. } = &mut app.mode {
                line_comments.start_editing_target(DiffCommentTarget::File {
                    path: "src/main.rs".into(),
                });
                let input = line_comments
                    .editing_input_mut()
                    .expect("comment is editable");
                *input = InputState::with_text("Use @src/pending after".into());
                input.cursor = "Use @src".len();
            }
            sync_comment_at_mention(&mut app);
            let entries = if has_match {
                vec![crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/lib.rs".into(),
                }]
            } else {
                Vec::new()
            };
            app.apply_app_events(AppEvent::AtMentionEntriesLoaded {
                entries: entries.clone(),
                session_id: "session-id".into(),
            })
            .await;

            // Act
            for key in [KeyCode::Down, KeyCode::Up, code] {
                handle_line_comment_edit_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE));
            }
            app.apply_app_events(AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id: "session-id".into(),
            })
            .await;

            // Assert
            let AppMode::Diff { line_comments, .. } = &app.mode else {
                unreachable!("diff mode expected")
            };
            assert!(line_comments.is_editing());
            assert!(line_comments.at_mention_state.is_none());
            assert_eq!(
                line_comments.comments[0].input.text(),
                if has_match && code != KeyCode::Esc {
                    "Use @src/lib.rs  after"
                } else {
                    "Use @src/pending after"
                }
            );
        }
    }
}

#[tokio::test]
async fn test_handle_quit_key_returns_to_view_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: "diff output".to_string(),
        scroll_offset: 7,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
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
async fn test_handle_quit_saves_and_reopens_diff_comments() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "review();".to_string(),
        line: 1,
        path: "src/main.rs".to_string(),
        side: crate::presentation::app_mode::DiffLineSide::New,
    }));
    line_comments
        .editing_input_mut()
        .expect("comment should be editable")
        .insert_text("Keep this comment");
    line_comments.finish_editing();
    line_comments.start_selection(0);
    app.mode = AppMode::Diff {
        diff: "diff output".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session_id: "session-id".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );

    // Assert
    let saved_comments = app
        .diff_comment_progress
        .get("session-id")
        .expect("comments should be saved after leaving Diff mode");
    assert_eq!(saved_comments.comments[0].input.text(), "Keep this comment");
    assert_eq!(saved_comments.editing_index, None);
    assert_eq!(saved_comments.selection_anchor_index, None);
    assert_eq!(saved_comments.selected_comment_index, None);

    // Act
    enter_diff_mode(
        &mut app,
        "session-id",
        "diff output".to_string(),
        None,
        DiffSidebarFocus::Files,
    );

    // Assert
    assert!(app.diff_comment_progress.is_empty());
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.comments[0].input.text() == "Keep this comment"
    ));

    // Act
    app.clear_diff_comment_progress("session-id");

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            line_comments,
            scroll_cache: None,
            ..
        } if line_comments.comments.is_empty()
    ));

    // Arrange
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
        line_comments
            .editing_input_mut()
            .expect("file comment should be editable")
            .insert_text("Clear from help too");
        line_comments.finish_editing();
    }
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );

    // Act
    app.clear_diff_comment_progress("session-id");

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Help {
            context: HelpContext::Diff { line_comments, .. },
            ..
        } if line_comments.comments.is_empty()
    ));
}

#[tokio::test]
async fn test_handle_plain_j_k_and_f_navigate_changed_lines_and_focus_files() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        &scrollable_diff_fixture(),
        0,
        DiffFocus::Content,
        DiffPreview::default(),
    );

    // Act
    for key_code in [KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Char('f')] {
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(key_code, KeyModifiers::NONE),
        );
    }

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            focus: DiffFocus::Files,
            selected_diff_line_index: 0,
            ..
        }
    ));
}
