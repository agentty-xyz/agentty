use super::super::{
    activate_question_at_mention, dismiss_question_at_mention, exit_to_list_saving_progress,
    handle_paste, handle_question_at_mention_down, handle_question_at_mention_select,
    handle_question_at_mention_up, mark_session_in_progress, reset_question_at_mention_selection,
    store_question_response, take_question_snapshot,
};
use crate::domain::session::{SessionHandles, Status};
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn mark_session_in_progress_advances_snapshot_and_handle_status() {
    // Arrange — a session sits in `Question` with a matching runtime
    // handle.

    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-in-progress";
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(session_id)
            .folder(std::path::PathBuf::from("/tmp/test"))
            .status(Status::Question)
            .build(),
    );
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Question),
    );

    // Act
    mark_session_in_progress(&mut app, session_id);

    // Assert — both the snapshot and the shared handle advance to
    // InProgress so sync_from_handles keeps the session off `Question`.
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    assert_eq!(session.status, Status::InProgress);

    let handles = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("handle should exist");
    assert_eq!(
        *handles.status.lock().expect("lock should succeed"),
        Status::InProgress
    );
}

#[tokio::test]
async fn mark_session_in_progress_ignores_an_unloaded_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("loaded-session")
            .status(Status::Question)
            .build(),
    );

    // Act
    mark_session_in_progress(&mut app, "missing-session");

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::Question);
    assert!(
        app.sessions
            .session_handles()
            .get("missing-session")
            .is_none()
    );
}

#[tokio::test]
async fn question_helpers_leave_other_modes_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::List;

    // Act
    exit_to_list_saving_progress(&mut app);
    let snapshot = take_question_snapshot(&mut app);
    handle_paste(&mut app, "pasted");
    activate_question_at_mention(&mut app, "session-id");
    dismiss_question_at_mention(&mut app);
    handle_question_at_mention_up(&mut app);
    handle_question_at_mention_down(&mut app);
    handle_question_at_mention_select(&mut app);
    let completed_response = store_question_response(&mut app, "answer".to_string());

    // Assert
    assert!(snapshot.is_none());
    assert!(completed_response.is_none());
    assert!(app.question_progress.is_empty());
    assert!(matches!(app.mode, AppMode::List));
}

#[test]
fn question_mention_selection_reset_leaves_other_modes_unchanged() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    reset_question_at_mention_selection(&mut mode);

    // Assert
    assert!(matches!(mode, AppMode::List));
}
