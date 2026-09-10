use crossterm::event::KeyCode;

use super::support::{
    new_test_prompt_app_with_session_mode, press_prompt_key, session_replay_text,
};
use crate::domain::permission::PermissionMode;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_backtab_preserves_mode_and_reports_persistence_failure() {
    // Arrange
    let (mut app, _base_dir, pool) =
        new_test_prompt_app_with_session_mode("draft text", None, false).await;
    sqlx::query(
        "CREATE TRIGGER fail_permission_mode_update BEFORE UPDATE OF permission_mode ON session \
         BEGIN SELECT RAISE(FAIL, 'forced permission mode failure'); END",
    )
    .execute(&pool)
    .await
    .expect("failure trigger should be installed");
    let session_id = app.sessions.sessions()[0].id.clone();

    // Act
    press_prompt_key(&mut app, KeyCode::BackTab).await;
    app.sessions.sync_from_handles();
    let persisted_permission_mode = app
        .services
        .db()
        .sessions()
        .load_session_permission_mode(&session_id)
        .await
        .expect("permission mode should load");

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft text"
    ));
    assert_eq!(
        app.sessions.sessions()[0].permission_mode,
        PermissionMode::AutoEdit
    );
    assert_eq!(persisted_permission_mode, PermissionMode::AutoEdit);
    assert!(
        session_replay_text(&app.sessions.sessions()[0])
            .contains("[Error] Failed to change mode; the session remains unchanged:")
    );
}
