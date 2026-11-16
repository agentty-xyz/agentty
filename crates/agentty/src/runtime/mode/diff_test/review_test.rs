//! Diff markdown preview, review-comment focus, and review-output restore
//! tests.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use tokio::sync::Semaphore;

use super::super::{selected_preview_is_visible, viewport_rect};
use super::support::{
    TEST_TERMINAL_SIZE, diff_mode_fixture, handle, navigate, next_diff_preview_event,
    preview_test_app,
};
use crate::app::{AppEvent, DiffPreviewContent};
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffPreviewUnavailableReason,
    DiffReviewComments, DiffScrollCache, DiffSidebarFocus,
};
use crate::presentation::diff_image::{DiffImagePair, DiffImageSide};
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

#[test]
fn test_navigation_l_focuses_visible_markdown_preview() {
    // Arrange
    let mut mode = diff_mode_fixture(
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
    navigate(
        &mut mode,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        mode,
        AppMode::Diff {
            focus: DiffFocus::Content,
            selected_diff_line_index: 0,
            ..
        }
    ));
}

#[test]
fn test_navigation_c_focuses_linked_review_comments() {
    // Arrange
    let mut mode = AppMode::Diff {
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
    navigate(
        &mut mode,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        mode,
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

#[test]
fn test_navigation_preview_arrow_keys_scroll_both_directions() {
    // Arrange
    let preview_content = (0..40)
        .map(|index| format!("preview line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut mode = diff_mode_fixture(
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
    navigate(
        &mut mode,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    navigate(
        &mut mode,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        mode,
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
async fn test_handle_preview_key_reports_worktree_read_failures() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file()
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("unreadable".to_string())) })
        });
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = diff_mode_fixture(
        "diff --git a/README.md b/README.md\n+preview",
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    let event = next_diff_preview_event(&mut app).await;

    // Assert
    assert!(matches!(
        event,
        AppEvent::DiffPreviewLoaded {
            content: DiffPreviewContent::Markdown(Err(ref error)),
            ..
        } if error.contains("unreadable")
    ));
}

/// Returns the image sides and path of a first-request image preview event.
fn image_preview_parts(event: AppEvent) -> Option<(Box<DiffImagePair>, String)> {
    match event {
        AppEvent::DiffPreviewLoaded {
            content: DiffPreviewContent::Image(images),
            path,
            request_id: 1,
            ..
        } => Some((images, path)),
        _ => None,
    }
}

/// Encodes a 1×1 opaque PNG of one color.
fn one_pixel_png(color: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&color, 1, 1, ExtendedColorType::Rgba8)
        .expect("PNG fixture should encode");

    bytes
}

#[tokio::test]
async fn test_handle_preview_key_loads_both_sides_of_a_modified_image() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file_bytes()
        .withf(|_, path| path == "assets/logo.png")
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Ok(ag_git::FileBytes::Bytes(one_pixel_png([9, 8, 7, 255]))) })
        });
    mock_git_client
        .expect_read_blob_bytes()
        .withf(|_, object_id| object_id == "3ece5bf")
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("no blob".to_string())) })
        });
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = diff_mode_fixture(
        concat!(
            "diff --git a/assets/logo.png b/assets/logo.png\n",
            "index 3ece5bf..f2220e6 100644\n",
            "Binary files a/assets/logo.png and b/assets/logo.png differ",
        ),
        1,
        DiffFocus::Files,
        DiffPreview::default(),
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    let loading = matches!(
        &app.mode,
        AppMode::Diff {
            preview: DiffPreview::Loading { path, request_id: 1 },
            ..
        } if path == "assets/logo.png"
    );
    let event = next_diff_preview_event(&mut app).await;

    // Assert
    assert!(loading);
    let (images, path) = image_preview_parts(event).expect("expected an image preview event");
    assert_eq!(path, "assets/logo.png");
    assert!(matches!(
        &images.after,
        DiffImageSide::Image(image) if image.pixel(0, 0) == [9, 8, 7, 255]
    ));
    assert!(matches!(
        &images.before,
        DiffImageSide::Unavailable(reason) if reason.contains("no blob")
    ));
}

#[tokio::test]
async fn test_handle_preview_key_skips_old_side_of_an_added_image() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file_bytes()
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("unreadable".to_string())) })
        });
    mock_git_client.expect_read_blob_bytes().times(0);
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = diff_mode_fixture(
        concat!(
            "diff --git a/new.gif b/new.gif\n",
            "new file mode 100644\n",
            "index 0000000..f2220e6\n",
            "Binary files /dev/null and b/new.gif differ",
        ),
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    let event = next_diff_preview_event(&mut app).await;

    // Assert
    let (images, _) = image_preview_parts(event).expect("expected an image preview event");
    assert!(matches!(
        &images.after,
        DiffImageSide::Unavailable(reason) if reason.contains("unreadable")
    ));
    assert_eq!(images.before, DiffImageSide::Absent);
}

#[tokio::test]
async fn test_handle_preview_key_reuses_new_side_for_a_renamed_image() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file_bytes()
        .withf(|_, path| path == "renamed.png")
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Ok(ag_git::FileBytes::Bytes(one_pixel_png([1, 2, 3, 255]))) })
        });
    mock_git_client.expect_read_blob_bytes().times(0);
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = diff_mode_fixture(
        concat!(
            "diff --git a/logo.png b/renamed.png\n",
            "similarity index 100%\n",
            "rename from logo.png\n",
            "rename to renamed.png",
        ),
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    let event = next_diff_preview_event(&mut app).await;

    // Assert
    let (images, path) = image_preview_parts(event).expect("expected an image preview event");
    assert_eq!(path, "renamed.png");
    assert!(matches!(
        &images.after,
        DiffImageSide::Image(image) if image.pixel(0, 0) == [1, 2, 3, 255]
    ));
    assert_eq!(images.before, images.after);
}

#[tokio::test]
async fn test_superseded_image_load_publishes_no_preview() {
    // Arrange
    let stale_read_gate = Arc::new(Semaphore::new(0));
    let mut mock_git_client = ag_git::MockGitClient::new();
    let gate = Arc::clone(&stale_read_gate);
    mock_git_client
        .expect_read_worktree_file_bytes()
        .withf(|_, path| path == "a.png")
        .times(1)
        .returning(move |_, _| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                let _released = gate.acquire().await;

                Ok(ag_git::FileBytes::Bytes(one_pixel_png([1, 1, 1, 255])))
            })
        });
    mock_git_client
        .expect_read_worktree_file_bytes()
        .withf(|_, path| path == "b.png")
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Ok(ag_git::FileBytes::Bytes(one_pixel_png([2, 2, 2, 255]))) })
        });
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = diff_mode_fixture(
        concat!(
            "diff --git a/a.png b/a.png\n",
            "new file mode 100644\n",
            "index 0000000..1111111\n",
            "Binary files /dev/null and b/a.png differ\n",
            "diff --git a/b.png b/b.png\n",
            "new file mode 100644\n",
            "index 0000000..2222222\n",
            "Binary files /dev/null and b/b.png differ",
        ),
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    let current_event = next_diff_preview_event(&mut app).await;
    stale_read_gate.add_permits(1);
    let stale_event = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            let event = app.next_app_event().await;
            if matches!(event, Some(AppEvent::DiffPreviewLoaded { .. }) | None) {
                return event;
            }
        }
    })
    .await;

    // Assert
    assert!(matches!(
        current_event,
        AppEvent::DiffPreviewLoaded { ref path, request_id: 2, .. } if path == "b.png"
    ));
    assert!(
        stale_event.is_err(),
        "superseded load should publish nothing"
    );
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
async fn test_handle_selection_change_refreshes_only_enabled_preview() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    for (preview, expected_preview) in [
        (
            DiffPreview::Off { request_id: 4 },
            DiffPreview::Off { request_id: 4 },
        ),
        (
            DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "docs/README.md".to_string(),
                request_id: 4,
            },
            DiffPreview::Unsupported { request_id: 5 },
        ),
    ] {
        app.mode = AppMode::Diff {
            diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview,
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
            &app.mode,
            AppMode::Diff {
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                selected_diff_line_index: 0,
                preview,
                scroll_offset: 0,
                ..
            } if *preview == expected_preview
        ));
    }
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
            request_id: uuid::Uuid::nil(),
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
