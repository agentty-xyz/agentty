use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::super::{
    DiffScrollLimitInput, diff_max_scroll_offset, start_line_comment_edit, viewport_rect,
};
use super::support::{
    TEST_TERMINAL_SIZE, diff_mode_fixture, handle, preview_test_app, scrollable_diff_fixture,
};
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineCommentAnchor, DiffLineCommentTarget,
    DiffLineComments, DiffPreview, DiffScrollCache,
};
use crate::runtime::EventResult;
use crate::ui::{RenderCacheStore, page};

#[tokio::test]
async fn test_blank_line_comment_clears_cached_layout() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
    app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    if let AppMode::Diff { scroll_cache, .. } = &mut app.mode {
        *scroll_cache = Some(DiffScrollCache {
            content_area: viewport_rect(TEST_TERMINAL_SIZE),
            file_explorer_selected_index: 1,
            max_scroll_offset: 0,
        });
    }

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            line_comments,
            scroll_cache: None,
            ..
        } if line_comments.comments.is_empty()
    ));
}

#[tokio::test]
async fn test_handle_l_from_files_focus_preserves_scrolled_position() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        diff: scrollable_diff_fixture(),
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 20,
        selected_diff_line_index: 0,
        session_id: "session-id".into(),
    };

    // Act
    let event_result = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            scroll_offset: 20,
            selected_diff_line_index,
            ..
        } if selected_diff_line_index > 0
    ));
}

#[test]
fn test_diff_max_scroll_offset_returns_cached_value_on_matching_key() {
    // Arrange — a cache entry whose key matches the requested viewport and
    // selection.
    let diff = scrollable_diff_fixture();
    let diff_layout_cache = page::diff::DiffLayoutCache::default();
    let markdown_render_cache = crate::ui::markdown::MarkdownRenderCache::default();
    let preview = DiffPreview::default();
    let line_comments = DiffLineComments::default();
    let mut scroll_cache = Some(DiffScrollCache {
        content_area: viewport_rect(TEST_TERMINAL_SIZE),
        file_explorer_selected_index: 0,
        max_scroll_offset: 4242,
    });

    // Act
    let max_scroll_offset = diff_max_scroll_offset(
        &DiffScrollLimitInput {
            content_area: TEST_TERMINAL_SIZE,
            diff: &diff,
            diff_layout_cache: &diff_layout_cache,
            line_comments: &line_comments,
            markdown_render_cache: &markdown_render_cache,
            preview: &preview,
            selected_index: 0,
        },
        &mut scroll_cache,
    );

    // Assert — the cached limit is returned verbatim without recomputing.
    assert_eq!(max_scroll_offset, 4242);
}

#[tokio::test]
async fn test_handle_up_key_recovers_overscrolled_changed_line() {
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
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset,
            selected_diff_line_index: 38,
            ..
        } if scroll_offset < u16::MAX
    ));
}

#[tokio::test]
async fn test_handle_j_resets_scroll_offset() {
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
async fn test_handle_k_resets_scroll_offset() {
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
async fn test_handle_file_focus_scrolls_with_arrows_and_shift_j_k() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        session_id: "session-id".into(),
        diff: scrollable_diff_fixture(),
        scroll_offset: 0,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 7,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
    };
    let navigation = [
        (KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 1),
        (KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT), 2),
        (KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 1),
        (KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT), 0),
    ];

    // Act & Assert
    for (key, expected_scroll_offset) in navigation {
        let event_result = handle(&mut app, TEST_TERMINAL_SIZE, key);

        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                focus: DiffFocus::Files,
                scroll_offset,
                selected_diff_line_index: 7,
                ..
            } if *scroll_offset == expected_scroll_offset
        ));
    }
}

#[tokio::test]
async fn test_start_line_comment_edit_handles_layout_edges() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let diff = scrollable_diff_fixture();
    let render_cache_store = RenderCacheStore::default();
    let target = DiffLineCommentTarget::single(
        render_cache_store
            .diff_layout_cache()
            .content(&diff)
            .selected_changed_line(1, 39)
            .expect("last changed line should resolve from the diff"),
    );

    // Act
    start_line_comment_edit(
        &mut app,
        &render_cache_store,
        TEST_TERMINAL_SIZE,
        target.clone(),
    );

    // Assert
    assert!(matches!(app.mode, AppMode::List));

    // Arrange
    app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());
    let missing_target = DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "missing line".to_string(),
        line: 999,
        path: "src/main.rs".to_string(),
        side: crate::presentation::app_mode::DiffLineSide::New,
    });

    // Act
    start_line_comment_edit(
        &mut app,
        &render_cache_store,
        TEST_TERMINAL_SIZE,
        missing_target,
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if !line_comments.is_editing() && line_comments.comments.is_empty()
    ));

    // Act — a stale whole-file target from another selection is ignored.
    start_line_comment_edit(
        &mut app,
        &render_cache_store,
        TEST_TERMINAL_SIZE,
        DiffCommentTarget::file("src/other.rs"),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if !line_comments.is_editing() && line_comments.comments.is_empty()
    ));

    // Arrange
    app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());
    if let AppMode::Diff {
        selected_diff_line_index,
        ..
    } = &mut app.mode
    {
        *selected_diff_line_index = usize::MAX;
    }

    // Act
    start_line_comment_edit(
        &mut app,
        &render_cache_store,
        TEST_TERMINAL_SIZE,
        target.clone(),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            line_comments,
            scroll_offset,
            selected_diff_line_index: usize::MAX,
            ..
        } if line_comments.is_editing() && *scroll_offset > 0
    ));

    // Arrange
    app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());

    // Act
    start_line_comment_edit(
        &mut app,
        &render_cache_store,
        Rect::new(0, 0, 80, 0),
        target.clone(),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 0,
            ..
        }
    ));

    // Arrange
    app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());
    if let AppMode::Diff {
        selected_diff_line_index,
        ..
    } = &mut app.mode
    {
        *selected_diff_line_index = 39;
    }

    // Act
    start_line_comment_edit(&mut app, &render_cache_store, TEST_TERMINAL_SIZE, target);

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff { scroll_offset, .. } if scroll_offset > 0
    ));
}
