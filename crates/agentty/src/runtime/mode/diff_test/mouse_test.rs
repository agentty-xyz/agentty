use super::super::handle_mouse_wheel;
use super::support::{aligned_file_diff_fixture, diff_mode_fixture, scrollable_diff_fixture};
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffPreview, DiffReviewComments, DiffSidebarFocus,
    ViewportRect,
};
use crate::presentation::viewport::{LayoutSnapshot, ScrollRegion};
use crate::runtime::mouse_handler::WheelDirection;
use crate::ui::RenderCacheStore;

/// Builds a layout whose diff panel and file list occupy distinct columns.
fn diff_layout() -> LayoutSnapshot {
    LayoutSnapshot {
        diff_file_list: Some(ViewportRect {
            height: 12,
            width: 20,
            x: 0,
            y: 0,
        }),
        diff_panel: Some(ScrollRegion {
            area: ViewportRect {
                height: 12,
                width: 60,
                x: 20,
                y: 0,
            },
            scrollbar: None,
            total_lines: 30,
            viewport_height: 10,
        }),
        ..LayoutSnapshot::default()
    }
}

#[tokio::test]
async fn test_handle_mouse_wheel_over_diff_panel_scrolls_and_clamps() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        &scrollable_diff_fixture(),
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );
    if let AppMode::Diff { scroll_offset, .. } = &mut app.mode {
        *scroll_offset = 18;
    }
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    let scrolled_down = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        30,
        5,
        WheelDirection::Down,
    );
    let clamped = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        30,
        5,
        WheelDirection::Down,
    );
    let scrolled_up = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        30,
        5,
        WheelDirection::Up,
    );

    // Assert
    assert!(scrolled_down);
    assert!(!clamped);
    assert!(scrolled_up);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 17,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_mouse_wheel_over_file_list_moves_selection_and_refreshes_preview() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        &aligned_file_diff_fixture(),
        0,
        DiffFocus::Content,
        DiffPreview::Unsupported { request_id: 0 },
    );
    if let AppMode::Diff { scroll_offset, .. } = &mut app.mode {
        *scroll_offset = 4;
    }
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    let moved_down = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Down,
    );
    let index_after_down = match &app.mode {
        AppMode::Diff {
            file_explorer_selected_index,
            ..
        } => *file_explorer_selected_index,
        _ => usize::MAX,
    };
    let moved_up = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Up,
    );

    // Assert
    assert!(moved_down);
    assert_eq!(index_after_down, 1);
    assert!(moved_up);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            file_explorer_selected_index: 0,
            preview: DiffPreview::Unsupported { .. },
            scroll_offset: 0,
            selected_diff_line_index: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_mouse_wheel_over_empty_file_list_keeps_selection() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture("", 0, DiffFocus::Files, DiffPreview::default());
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    let changed = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Down,
    );

    // Assert
    assert!(!changed);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            file_explorer_selected_index: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_mouse_wheel_ignores_non_diff_mode_and_pointer_outside_regions() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    app.mode = AppMode::List;
    let panel_in_list_mode = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        30,
        5,
        WheelDirection::Down,
    );
    let file_list_in_list_mode = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Down,
    );
    app.mode = diff_mode_fixture(
        &scrollable_diff_fixture(),
        0,
        DiffFocus::Files,
        DiffPreview::default(),
    );
    let outside_regions = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        30,
        20,
        WheelDirection::Down,
    );

    // Assert
    assert!(!panel_in_list_mode);
    assert!(!file_list_in_list_mode);
    assert!(!outside_regions);
    assert!(matches!(app.mode, AppMode::Diff { .. }));
}

/// Reads the selected file index out of diff mode.
fn selected_file_index(mode: &AppMode) -> usize {
    match mode {
        AppMode::Diff {
            file_explorer_selected_index,
            ..
        } => *file_explorer_selected_index,
        _ => usize::MAX,
    }
}

#[tokio::test]
async fn test_handle_mouse_wheel_over_file_list_ignores_input_while_editing_comment() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        &aligned_file_diff_fixture(),
        0,
        DiffFocus::Content,
        DiffPreview::Unsupported { request_id: 0 },
    );
    if let AppMode::Diff { line_comments, .. } = &mut app.mode {
        line_comments.start_editing_target(DiffCommentTarget::File {
            path: "src/main.rs".into(),
        });
    }
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    let moved = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Down,
    );

    // Assert
    assert!(!moved);
    assert_eq!(selected_file_index(&app.mode), 0);
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. } if line_comments.is_editing()
    ));
}

#[tokio::test]
async fn test_handle_mouse_wheel_over_file_list_ignores_input_while_selecting_rows() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        &aligned_file_diff_fixture(),
        0,
        DiffFocus::Content,
        DiffPreview::Unsupported { request_id: 0 },
    );
    if let AppMode::Diff {
        line_comments,
        selected_diff_line_index,
        ..
    } = &mut app.mode
    {
        *selected_diff_line_index = 2;
        line_comments.start_selection(2);
    }
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    let moved = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Down,
    );

    // Assert
    assert!(!moved);
    assert_eq!(selected_file_index(&app.mode), 0);
    assert!(matches!(
        &app.mode,
        AppMode::Diff {
            line_comments,
            selected_diff_line_index: 2,
            ..
        } if line_comments.is_selecting()
    ));
}

#[tokio::test]
async fn test_handle_mouse_wheel_over_file_list_ignores_input_while_comments_sidebar_focused() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = diff_mode_fixture(
        &aligned_file_diff_fixture(),
        0,
        DiffFocus::Content,
        DiffPreview::Unsupported { request_id: 0 },
    );
    if let AppMode::Diff {
        review_comments,
        scroll_offset,
        ..
    } = &mut app.mode
    {
        *scroll_offset = 3;
        *review_comments = Some(DiffReviewComments {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..DiffReviewComments::loading(1)
        });
    }
    let render_cache_store = RenderCacheStore::default();
    let layout = diff_layout();

    // Act
    let moved = handle_mouse_wheel(
        &mut app,
        &render_cache_store,
        &layout,
        5,
        5,
        WheelDirection::Down,
    );

    // Assert
    assert!(!moved);
    assert_eq!(selected_file_index(&app.mode), 0);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 3,
            ..
        }
    ));
}
