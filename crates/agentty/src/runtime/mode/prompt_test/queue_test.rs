use super::super::{handle_prompt_submit_key, prompt_context};
use super::support::new_test_prompt_app;

#[tokio::test]
/// Verifies that submitting a `/`-prefixed prompt while the session is
/// `InProgress` queues the raw text via [`App::enqueue_message`] instead
/// of invoking the slash command path.
async fn test_handle_prompt_submit_key_queues_slash_text_when_session_is_in_progress() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model gpt-5", None).await;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.sessions.session_handles_mut().insert(
        session_id.clone(),
        crate::domain::session::SessionHandles::new(crate::domain::session::Status::InProgress),
    );
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::InProgress;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    let queued_len = app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles for in-progress session")
        .queued_messages
        .lock()
        .expect("queue lock")
        .len();
    assert_eq!(
        queued_len, 1,
        "slash-prefixed input must be queued as plain text while turn runs"
    );
    assert_eq!(app.sessions.sessions()[0].queued_messages.len(), 1);
    assert_eq!(
        app.sessions.sessions()[0].queued_messages[0].transcript_text(),
        "/model gpt-5",
        "queued message preserves the original slash-prefixed text"
    );
}

#[tokio::test]
/// Verifies that submitting a prompt while the session is `Rebasing`
/// queues it via [`App::enqueue_message`] instead of trying to start a
/// concurrent reply turn.
async fn test_handle_prompt_submit_key_queues_text_when_session_is_rebasing() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("queued after rebase", None).await;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.sessions.session_handles_mut().insert(
        session_id.clone(),
        crate::domain::session::SessionHandles::new(crate::domain::session::Status::Rebasing),
    );
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Rebasing;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    let queued_messages = app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles for rebasing session")
        .queued_messages
        .lock()
        .expect("queue lock");
    assert_eq!(queued_messages.len(), 1);
    assert_eq!(queued_messages[0].transcript_text(), "queued after rebase");
    assert_eq!(
        app.sessions.sessions()[0].queued_messages[0].transcript_text(),
        "queued after rebase"
    );
}
