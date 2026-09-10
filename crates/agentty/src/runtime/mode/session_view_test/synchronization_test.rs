use super::super::{
    end_in_progress_turn, open_merge_confirmation, rebase_view_session, view_context,
};
use super::support::{
    new_test_app_with_session, queued_message, session_fixture, session_replay_text,
};
use crate::domain::session::Status;
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::presentation::help_action;
use crate::presentation::help_action::ViewSessionState;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;

#[test]
fn test_view_session_state_maps_merge_queue_statuses() {
    // Arrange
    let merge_queue_statuses = [Status::Queued, Status::Merging];

    // Act
    let mapped_states: Vec<ViewSessionState> = merge_queue_statuses
        .iter()
        .map(|status| help_action::session_view_state(&session_fixture(*status, false)))
        .collect();

    // Assert
    assert!(
        mapped_states
            .iter()
            .all(|state| *state == ViewSessionState::MergeQueue)
    );
}

#[tokio::test]
async fn test_open_merge_confirmation_sets_confirmation_mode_with_view_restore_state() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(5),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    open_merge_confirmation(&mut app, &context);

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            ref confirmation_message,
            ref confirmation_title,
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(5),
                session_id: ref restored_session_id,
            }),
            session_id: Some(ref mode_session_id),
            selected_confirmation_index: DEFAULT_OPTION_INDEX,
        } if confirmation_title == "Confirm Merge"
            && confirmation_message == "Add this session to merge queue?"
            && restored_session_id == &session_id
            && mode_session_id == &session_id
    ));
}

#[tokio::test]
async fn test_rebase_view_session_appends_error_output_without_review_status() {
    // Arrange
    let (app, _base_dir, session_id) = new_test_app_with_session().await;
    let mut app = app;

    // Act
    rebase_view_session(&mut app, &session_id).await;

    // Assert
    app.sessions.sync_from_handles();
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert!(output.contains("[Sync Error]"));
}

#[tokio::test]
/// Regression: when the worker has already drained the oldest queued
/// prompt via `pop_front` but the deferred `RefreshSessions` reducer
/// has not yet rebuilt the snapshot, a `Ctrl+C` press must still align
/// the snapshot with the current handle state instead of removing the
/// snapshot's last entry positionally and leaving a phantom queued row.
async fn test_pop_last_queued_chat_message_resyncs_snapshot_after_worker_drain() {
    // Arrange — two prompts queued; simulate the worker popping the
    // oldest entry off the handle without yet refreshing the snapshot.
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
    let queued_messages = std::sync::Arc::clone(&handles.queued_messages);
    {
        let mut queued = queued_messages.lock().expect("queued_messages lock");
        queued.push_back(queued_message(0, "first queued"));
        queued.push_back(queued_message(1, "second queued"));
    }
    app.sessions
        .session_handles_mut()
        .insert(session_id.clone().into(), handles);
    app.sessions.sessions_mut()[0].queued_messages = vec![
        queued_message(0, "first queued"),
        queued_message(1, "second queued"),
    ];

    // Simulate the worker `pop_front` draining the oldest entry before
    // the snapshot has been refreshed.
    {
        let mut queued = queued_messages.lock().expect("queued_messages lock");
        queued.pop_front();
    }

    // Act — Ctrl+C while the handle has [second] but the snapshot still
    // reads [first, second].
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — handle is now empty (the user retracted "second"), and
    // the snapshot reflects the post-pop handle state instead of
    // positionally dropping the snapshot's last entry (which would
    // leave a phantom "first queued" row pointing at a turn the worker
    // is already running).
    assert!(
        queued_messages
            .lock()
            .expect("queued_messages lock")
            .is_empty(),
        "handle queue should be empty after retracting the only remaining entry"
    );
    assert!(
        app.sessions.sessions()[0].queued_messages.is_empty(),
        "snapshot must rebuild from the handle state and not show a phantom row for a prompt the \
         worker is already executing"
    );
}
