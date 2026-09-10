use tempfile::tempdir;

use super::support::{
    create_and_start_session, new_test_app_with_git, test_session_manager, wait_for_status,
};
use crate::app::AppEvent;
use crate::domain::session::Status;
use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};
use crate::presentation::app_mode::{DiffCommentTarget, DiffLineComments};

#[test]
fn test_queued_session_actions_use_explicit_waiting_slots_until_resolved() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);

    // Act
    session_manager.queue_branch_publish(
        "session-id",
        3,
        "review request — publish after this turn".to_string(),
    );
    session_manager.queue_session_sync("session-id", 4);

    // Assert
    let transient_messages = &session_manager.sessions()[0].transient_messages;
    assert!(matches!(
        transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .map(|message| &message.body),
        Some(TransientMessageBody::Queued(label))
            if label.order == 3 && label.text == "review request — publish after this turn"
    ));
    assert!(matches!(
        transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .map(|message| &message.body),
        Some(TransientMessageBody::Queued(label))
            if label.order == 4
                && label.text == "sync — rebase onto the base branch after this turn"
    ));

    // Act
    session_manager.resolve_queued_branch_publish("session-id");
    session_manager.resolve_queued_session_sync("session-id");

    // Assert
    assert!(
        session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
    assert!(
        session_manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .is_none()
    );
}

#[tokio::test]
/// Verifies that submitting a chat message while the session is
/// `InProgress` pushes the prompt onto the in-memory queue and mirrors
/// it into the render snapshot so the row appears inline in the
/// transcript before the running turn finishes.
async fn test_enqueue_message_pushes_prompt_onto_in_memory_queue() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    app.save_diff_comment_progress(session_id.clone(), line_comments);
    let saved_line_comments = app.diff_comment_progress[&session_id].clone();

    // Act
    app.enqueue_message(&session_id, "queued reply")
        .expect("enqueue_message should succeed for InProgress session");

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session present");
    assert_eq!(session.queued_messages[0].transcript_text(), "queued reply");
    let handles = app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles present");
    let queued_len = handles.queued_messages.lock().expect("queue lock").len();
    assert_eq!(queued_len, 1);
    assert_eq!(
        app.diff_comment_progress.get(&session_id),
        Some(&saved_line_comments),
        "queueing must retain comments until the queued turn starts"
    );
}

#[tokio::test]
/// Regression: a queued chat message must remain visible after the
/// reducer reloads sessions from the database. The previous wiring
/// emitted `RefreshSessions` from `enqueue_message`, which rebuilt every
/// `Session` snapshot with `queued_messages: Vec::new()`. The post-reload
/// `sync_session_with_handles` did not restore `queued_messages` from the
/// handles, so the just-pushed entry was silently wiped on the next
/// reducer pass and the inline `≡ queued ›` row briefly disappeared from
/// the transcript before reappearing on a later mutation.
async fn test_enqueue_message_survives_refresh_sessions_reducer_pass() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    app.enqueue_message(&session_id, "queued reply")
        .expect("enqueue_message should succeed for InProgress session");

    // Act
    app.apply_app_events(AppEvent::RefreshSessions).await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session present");
    assert_eq!(
        session.queued_messages[0].transcript_text(),
        "queued reply",
        "queued_messages snapshot must be re-projected from handles after a RefreshSessions \
         reducer pass instead of being wiped to an empty vec"
    );
}

#[tokio::test]
/// Verifies that empty payloads are rejected without mutating the queue
/// so accidentally submitting an empty composer does not stage a noop
/// turn.
async fn test_enqueue_message_rejects_empty_payload() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

    // Act
    let outcome = app.enqueue_message(&session_id, "");

    // Assert
    let error = outcome.expect_err("empty payload should error");
    assert!(matches!(
        error,
        crate::app::session::SessionError::Workflow(_)
    ));
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session present");
    assert_eq!(session.queued_messages, []);
}
