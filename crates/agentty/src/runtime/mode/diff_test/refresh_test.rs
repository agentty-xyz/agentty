//! Live open-diff refresh tests covering selection, preview, and deferral.

use super::super::apply_refreshed_diff;
use super::support::{diff_mode_fixture, next_diff_preview_event, preview_test_app};
use crate::app::App;
use crate::presentation::app_mode::{AppMode, DiffCommentTarget, DiffFocus, DiffPreview};
use crate::ui::RenderCacheStore;

/// Visible diff with `b.rs` and a two-row `c.rs`.
const VISIBLE_DIFF: &str = concat!(
    "diff --git a/b.rs b/b.rs\n@@ -0,0 +1 @@\n+b\n",
    "diff --git a/c.rs b/c.rs\n@@ -0,0 +1,2 @@\n+one\n+two\n",
);

/// Queues `diff` as the newer re-read for the fixture session.
fn queue_refreshed_diff(app: &mut App, diff: &str) {
    app.queue_refreshed_session_diff("session-id".into(), diff.to_string());
}

/// Opens the visible diff with `c.rs` and its second changed row selected.
fn select_second_row_of_c(app: &mut App) {
    app.mode = diff_mode_fixture(VISIBLE_DIFF, 1, DiffFocus::Content, DiffPreview::default());
    if let AppMode::Diff {
        scroll_offset,
        selected_diff_line_index,
        ..
    } = &mut app.mode
    {
        *scroll_offset = 3;
        *selected_diff_line_index = 1;
    }
}

/// Returns the open diff's text, selected tree row, changed row, and scroll.
fn diff_state(app: &App) -> Option<(&str, usize, usize, u16)> {
    let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        scroll_offset,
        selected_diff_line_index,
        ..
    } = &app.mode
    else {
        return None;
    };

    Some((
        diff,
        *file_explorer_selected_index,
        *selected_diff_line_index,
        *scroll_offset,
    ))
}

#[tokio::test]
async fn test_refreshed_diff_keeps_selected_file_and_changed_row() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    select_second_row_of_c(&mut app);
    let refreshed_diff = concat!(
        "diff --git a/a.rs b/a.rs\n@@ -0,0 +1 @@\n+a\n",
        "diff --git a/b.rs b/b.rs\n@@ -0,0 +1 @@\n+b\n",
        "diff --git a/c.rs b/c.rs\n@@ -1 +1,2 @@\n-gone\n+one\n+two\n",
    );
    queue_refreshed_diff(&mut app, refreshed_diff);

    // Act
    apply_refreshed_diff(&mut app, &RenderCacheStore::default());

    // Assert — `a.rs` shifts `c.rs` down and `-gone` shifts `+two` down.
    assert_eq!(diff_state(&app), Some((refreshed_diff, 2, 2, 3)));
    assert!(app.take_refreshed_session_diff().is_none());
}

#[tokio::test]
async fn test_refreshed_diff_clamps_changed_row_missing_from_selected_file() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let render_cache_store = RenderCacheStore::default();
    let edited_diff = concat!(
        "diff --git a/b.rs b/b.rs\n@@ -0,0 +1 @@\n+b\n",
        "diff --git a/c.rs b/c.rs\n@@ -0,0 +1,2 @@\n+uno\n+dos\n",
    );
    let shortened_diff = concat!(
        "diff --git a/b.rs b/b.rs\n@@ -0,0 +1 @@\n+b\n",
        "diff --git a/c.rs b/c.rs\n@@ -0,0 +1 @@\n+uno\n",
    );

    // Act
    select_second_row_of_c(&mut app);
    queue_refreshed_diff(&mut app, edited_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);
    let (_, edited_index, edited_row, edited_scroll) =
        diff_state(&app).expect("diff mode should stay open");
    queue_refreshed_diff(&mut app, shortened_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);

    // Assert — an edited row keeps its position while it still exists.
    assert_eq!((edited_index, edited_row, edited_scroll), (1, 1, 3));
    assert_eq!(diff_state(&app), Some((shortened_diff, 1, 0, 3)));
}

#[tokio::test]
async fn test_refreshed_diff_resets_selection_when_selected_file_disappears() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    select_second_row_of_c(&mut app);
    let refreshed_diff = "diff --git a/b.rs b/b.rs\n@@ -0,0 +1 @@\n+b\n";
    queue_refreshed_diff(&mut app, refreshed_diff);

    // Act
    apply_refreshed_diff(&mut app, &RenderCacheStore::default());

    // Assert
    assert_eq!(diff_state(&app), Some((refreshed_diff, 0, 0, 0)));
}

#[tokio::test]
async fn test_refreshed_diff_waits_for_comment_edit_and_drops_stale_pages() {
    // Arrange
    let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
    let render_cache_store = RenderCacheStore::default();
    let refreshed_diff = "diff --git a/b.rs b/b.rs\n@@ -0,0 +1 @@\n+b\n";
    select_second_row_of_c(&mut app);
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffCommentTarget::file("c.rs"));
    }

    // Act
    apply_refreshed_diff(&mut app, &render_cache_store);
    queue_refreshed_diff(&mut app, refreshed_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);
    let visible_while_editing = diff_state(&app).map(|state| state.0.to_string());
    let waits_while_editing = app.take_refreshed_session_diff().is_some();
    app.mode = diff_mode_fixture(VISIBLE_DIFF, 1, DiffFocus::Files, DiffPreview::default());
    if let AppMode::Diff { session_id, .. } = &mut app.mode {
        *session_id = "other-session".into();
    }
    queue_refreshed_diff(&mut app, refreshed_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);
    let other_session_diff = diff_state(&app).map(|state| state.0.to_string());
    app.mode = AppMode::List;
    queue_refreshed_diff(&mut app, refreshed_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);

    // Assert
    assert_eq!(visible_while_editing.as_deref(), Some(VISIBLE_DIFF));
    assert!(waits_while_editing);
    assert_eq!(other_session_diff.as_deref(), Some(VISIBLE_DIFF));
    assert!(matches!(app.mode, AppMode::List));
    assert!(app.take_refreshed_session_diff().is_none());
}

#[tokio::test]
async fn test_refreshed_diff_reloads_preview_only_when_markdown_changes() {
    // Arrange
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_read_worktree_file()
        .withf(|_, path| path == "README.md")
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Ok(ag_git::WorktreeFileContent::Text("# New".to_string())) })
        });
    let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
    let render_cache_store = RenderCacheStore::default();
    let readme_diff = "diff --git a/README.md b/README.md\n@@ -0,0 +1 @@\n+# New\n";
    let added_file_diff = format!("{readme_diff}diff --git a/z.rs b/z.rs\n@@ -0,0 +1 @@\n+z\n");
    app.mode = diff_mode_fixture(
        "diff --git a/README.md b/README.md\n@@ -0,0 +1 @@\n+# Old\n",
        0,
        DiffFocus::Files,
        DiffPreview::Ready {
            content: "# Old".to_string(),
            path: "README.md".to_string(),
            request_id: 1,
        },
    );

    // Act
    queue_refreshed_diff(&mut app, readme_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);
    let event = next_diff_preview_event(&mut app).await;
    app.apply_app_events(event).await;
    queue_refreshed_diff(&mut app, &added_file_diff);
    apply_refreshed_diff(&mut app, &render_cache_store);

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            diff,
            preview: DiffPreview::Ready { content, request_id: 2, .. },
            ..
        } if *diff == added_file_diff && content == "# New"
    ));
}
