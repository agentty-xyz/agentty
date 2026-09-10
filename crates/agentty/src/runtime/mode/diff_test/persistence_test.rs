use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::support::{TEST_TERMINAL_SIZE, handle, next_diff_preview_event, preview_test_app};
use crate::app::AppEvent;
use crate::presentation::app_mode::{AppMode, DiffFocus, DiffLineComments, DiffPreview};

#[tokio::test]
async fn test_handle_selection_change_reloads_next_markdown_file() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file()
        .withf(|_, path| path == "SECOND.MD")
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Ok(ag_git::WorktreeFileContent::Text("# Second".to_string())) })
        });
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    app.mode = AppMode::Diff {
        diff: concat!(
            "diff --git a/README.md b/README.md\n+first\n",
            "diff --git a/SECOND.MD b/SECOND.MD\n+second\n",
        )
        .to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::Ready {
            content: "# First".to_string(),
            path: "README.md".to_string(),
            request_id: 2,
        },
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 4,
        session_id: "session-id".into(),
    };

    // Act
    handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    let preview_event = next_diff_preview_event(&mut app).await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            selected_diff_line_index: 0,
            preview: DiffPreview::Loading {
                ref path,
                request_id: 3,
            },
            scroll_offset: 0,
            ..
        } if path == "SECOND.MD"
    ));
    assert!(matches!(
        preview_event,
        AppEvent::DiffPreviewLoaded { ref path, .. } if path == "SECOND.MD"
    ));
}
