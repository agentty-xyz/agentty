use super::super::mark_session_in_progress;
use crate::domain::session::{SessionHandles, Status};

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
