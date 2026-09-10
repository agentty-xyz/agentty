use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::end_in_progress_turn;
use super::support::{handle, new_test_app_with_session, queued_message};
use crate::domain::session::Status;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::presentation::app_mode::{AppMode, DiffCommentTarget, DiffLineComments};
use crate::runtime::EventResult;

#[tokio::test]
async fn test_handle_launch_follow_up_task_key_opens_linked_sibling_session() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let sibling_session_id = app
        .create_session()
        .await
        .expect("failed to create sibling session");
    let source_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("expected source session in session list");
    source_session.follow_up_tasks = vec![crate::domain::session::SessionFollowUpTask {
        id: 1,
        launched_session_id: Some(sibling_session_id.clone().into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(0),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let result = handle(
        &mut app,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    )
    .await
    .expect("launch/open key should be handled");

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some(sibling_session_id.as_str())
    );
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id == &sibling_session_id
    ));
}

#[tokio::test]
async fn test_end_in_progress_turn_first_press_with_queue_pops_last_queued_message() {
    // Arrange — seed an InProgress session with two queued chat messages
    // so the LIFO pop is observable (the older entry must remain).
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
    let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
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
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    app.save_diff_comment_progress(session_id.clone().into(), line_comments);
    let saved_line_comments = app.diff_comment_progress[session_id.as_str()].clone();

    // Act — first Ctrl+C while the queue is non-empty.
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — the most recently queued message is popped (LIFO), the
    // older message remains, status stays InProgress, and the cancel
    // token is untouched so the running turn keeps streaming.
    let remaining_handle_queue: Vec<String> = queued_messages
        .lock()
        .expect("queued_messages lock")
        .iter()
        .map(|message| message.transcript_text().to_string())
        .collect();
    assert_eq!(
        remaining_handle_queue,
        vec!["first queued".to_string()],
        "only the most recently queued chat message should be popped on first Ctrl+C"
    );
    assert_eq!(
        app.sessions.sessions()[0].queued_messages[0].transcript_text(),
        "first queued",
        "snapshot queued_messages should mirror the handle after LIFO pop"
    );
    assert_eq!(
        app.sessions.sessions()[0].status,
        Status::InProgress,
        "status should stay InProgress while only a queued message is popped"
    );
    let handle_status = *app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles missing")
        .status
        .lock()
        .expect("lock failed");
    assert_eq!(handle_status, Status::InProgress);
    assert!(
        !cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled(),
        "cancel_token must not be cancelled when only a queued message is popped"
    );
    assert_eq!(
        app.diff_comment_progress.get(session_id.as_str()),
        Some(&saved_line_comments),
        "retracting a queued message must preserve its diff comments"
    );
}

#[tokio::test]
async fn test_end_in_progress_turn_drains_queue_one_press_at_a_time_then_cancels() {
    // Arrange — InProgress session with two queued chat messages so we
    // can observe LIFO drain across consecutive presses before falling
    // through to the cancel path.
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
    let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
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

    // Act — first press pops "second queued".
    end_in_progress_turn(&mut app, &session_id).await;
    // Act — second press pops "first queued".
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — queue is empty after two presses, but the running turn
    // still has not been cancelled yet.
    assert!(
        queued_messages
            .lock()
            .expect("queued_messages lock")
            .is_empty(),
        "queue should be drained after one press per queued message"
    );
    assert!(
        app.sessions.sessions()[0].queued_messages.is_empty(),
        "snapshot queued_messages should be empty after LIFO drain"
    );
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    assert!(
        !cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled(),
        "cancel_token must not be cancelled while queued messages are still being drained"
    );

    // Act — third press, with empty queue, falls through to cancel.
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — cancel path engages and the session returns to Review.
    assert!(
        cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled(),
        "cancel_token must be cancelled once the queue is drained"
    );
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn test_end_in_progress_turn_second_press_after_empty_queue_cancels_turn() {
    // Arrange — InProgress session with an empty queue, mirroring the
    // state after the first press has already drained queued messages.
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.sessions.sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Resolving 2 review comments...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::ReviewCommentResolution,
            turn_position: None,
        });
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
    let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
    app.sessions
        .session_handles_mut()
        .insert(session_id.clone().into(), handles);

    // Act — second Ctrl+C now that the queue is empty.
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — falls through to the cancel path: cancel token fires and
    // the session transitions to Review.
    assert!(
        cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled(),
        "cancel_token must be cancelled when the queue is empty"
    );
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    let handle_status = *app
        .sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles missing")
        .status
        .lock()
        .expect("lock failed");
    assert_eq!(handle_status, Status::Review);
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
}
