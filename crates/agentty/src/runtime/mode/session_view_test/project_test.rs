use super::super::{ViewContext, show_diff_for_view_session};
use super::support::{apply_pending_session_diff, new_test_app_with_session};
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_show_diff_for_view_session_switches_mode_to_diff() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.clone().into(),
        session_index: 0,
    };

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);
    let loading = matches!(app.mode, AppMode::DiffLoading { .. });
    apply_pending_session_diff(
        &mut app,
        &context.session_id,
        Ok("diff --git a/README.md b/README.md\n+updated content\n"),
    )
    .await;

    // Assert
    assert!(opened);
    assert!(loading);
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            ref session_id,
        scroll_offset: 0,
            ..
        } if session_id == &context.session_id
    ));
}
