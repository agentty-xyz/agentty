use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{selected_preview_is_visible, viewport_rect};
use super::support::{
    TEST_TERMINAL_SIZE, diff_mode_fixture, handle, next_diff_preview_event, preview_test_app,
};
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffPreviewUnavailableReason,
    DiffReviewComments, DiffScrollCache, DiffSidebarFocus,
};
use crate::runtime::EventResult;
use crate::ui::page;

#[test]
fn test_selected_preview_is_visible_only_for_matching_markdown_selection() {
    // Arrange
    let diff = "diff --git a/README.md b/README.md\n+changed";
    let cache = page::diff::DiffLayoutCache::default();
    let matching_preview = DiffPreview::Ready {
        content: "# Changed".to_string(),
        path: "README.md".to_string(),
        request_id: 1,
    };
    let stale_preview = DiffPreview::Ready {
        content: "# Stale".to_string(),
        path: "OTHER.md".to_string(),
        request_id: 2,
    };

    // Act
    let matching_is_visible = selected_preview_is_visible(diff, 0, &cache, &matching_preview);
    let stale_is_visible = selected_preview_is_visible(diff, 0, &cache, &stale_preview);
    let unsupported_is_visible =
        selected_preview_is_visible(diff, 0, &cache, &DiffPreview::Unsupported { request_id: 3 });

    // Assert
    assert!(matching_is_visible);
    assert!(!stale_is_visible);
    assert!(!unsupported_is_visible);
}

#[tokio::test]
async fn test_handle_l_focuses_visible_markdown_preview() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        "diff --git a/README.md b/README.md\n+changed",
        0,
        DiffFocus::Files,
        DiffPreview::Ready {
            content: "# Preview".to_string(),
            path: "README.md".to_string(),
            request_id: 1,
        },
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            selected_diff_line_index: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_c_focuses_linked_review_comments() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        diff: "diff output".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments::loading(1)),
        restore: None,
        scroll_cache: Some(DiffScrollCache {
            content_area: viewport_rect(TEST_TERMINAL_SIZE),
            file_explorer_selected_index: 0,
            max_scroll_offset: 3,
        }),
        scroll_offset: 2,
        session_id: "session-id".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            }),
            scroll_cache: None,
            scroll_offset: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_preview_arrow_keys_scroll_both_directions() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let preview_content = (0..40)
        .map(|index| format!("preview line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.mode = diff_mode_fixture(
        "diff --git a/README.md b/README.md\n+changed",
        0,
        DiffFocus::Content,
        DiffPreview::Ready {
            content: preview_content,
            path: "README.md".to_string(),
            request_id: 1,
        },
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            scroll_offset: 0,
            scroll_cache: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_preview_key_loads_renders_event_and_toggles_off() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file()
        .withf(|_, path| path == "README.md")
        .times(1)
        .returning(|_, _| {
            Box::pin(async {
                Ok(ag_git::WorktreeFileContent::Text(
                    "# Rendered preview".to_string(),
                ))
            })
        });
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = AppMode::Diff {
        diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 7,
        session_id: "session-id".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    let event = next_diff_preview_event(&mut app).await;
    app.apply_app_events(event).await;
    let ready = matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Ready {
                ref content,
                request_id: 1,
                ..
            },
            scroll_offset: 0,
            ..
        } if content == "# Rendered preview"
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );

    // Assert
    assert!(ready);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Off { request_id: 2 },
            scroll_offset: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_preview_key_ignores_non_markdown_selection() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 3,
        session_id: "session-id".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Off { request_id: 0 },
            scroll_offset: 3,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_preview_key_reports_missing_session_worktree() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        session_id: "missing-session".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Unavailable {
                reason: DiffPreviewUnavailableReason::LoadFailed(ref error),
                ..
            },
            ..
        } if error == "Session worktree is unavailable"
    ));
}

#[tokio::test]
async fn test_handle_selection_change_keeps_preview_sticky_for_unsupported_row() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Diff {
        diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::Ready {
            content: "# Preview".to_string(),
            path: "docs/README.md".to_string(),
            request_id: 4,
        },
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 6,
        session_id: "session-id".into(),
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
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            selected_diff_line_index: 0,
            preview: DiffPreview::Unsupported { request_id: 5 },
            scroll_offset: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_enter_does_not_edit_line_when_review_comments_are_focused() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let mut review_comments = DiffReviewComments::loading(1);
    review_comments.sidebar_focus = DiffSidebarFocus::Comments;
    app.mode = AppMode::Diff {
        diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
        file_explorer_selected_index: 1,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(review_comments),
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        session_id: "session-id".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            line_comments: DiffLineComments { ref comments, .. },
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            }),
            ..
        } if comments.is_empty()
    ));
}

#[tokio::test]
async fn test_handle_quit_key_restores_cached_review_output() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.review_cache.insert(
        "session-id".into(),
        crate::app::ReviewCacheEntry::Ready {
            text: "Focused review".to_string(),
            diff_hash: 7,
        },
    );
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
