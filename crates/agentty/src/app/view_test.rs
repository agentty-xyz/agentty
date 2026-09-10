use super::visible_review_session_id;
use crate::app::Tab;
use crate::presentation::app_mode::{AppMode, DiffFocus, DiffLineComments};

#[test]
fn visible_review_session_id_includes_diff_comments() {
    // Arrange
    let mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: crate::presentation::app_mode::DiffPreview::default(),
        review_comments: Some(crate::presentation::app_mode::DiffReviewComments::loading(
            1,
        )),
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };

    // Act
    let session_id = visible_review_session_id(&mode);

    // Assert
    assert_eq!(session_id, Some("session-id"));
}

#[test]
fn visible_review_session_id_includes_loading_diff() {
    // Arrange
    let mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: None,
        request_id: 1,
        restore: None,
        session_id: "loading-session".into(),
        sidebar_focus: crate::presentation::app_mode::DiffSidebarFocus::Files,
    };

    // Act
    let session_id = visible_review_session_id(&mode);

    // Assert
    assert_eq!(session_id, Some("loading-session"));
}

#[tokio::test]
async fn view_snapshot_builds_settings_screen_only_for_settings_tab() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;

    // Act
    app.tabs.set(Tab::Sessions);
    let sessions_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();
    app.tabs.set(Tab::Settings);
    let settings_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();

    // Assert
    assert!(!sessions_tab_has_settings_screen);
    assert!(settings_tab_has_settings_screen);
}
