//! Session manager tests for sending queued chat into a running turn.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use super::support::{new_test_app_with_git, wait_for_status};
use crate::app::App;
use crate::app::test_support::{SteerableSessionChannel, TestSessionRunFactory};
use crate::domain::session::{QueuedMessage, Status};
use crate::domain::session_message::SessionMessageKind;

/// Queues one prompt directly on the session handles.
fn push_queued(app: &App, session_id: &str, text: &str, harness_steers: bool) {
    let handles = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("handles");
    let order = handles.next_queued_work_order();
    handles
        .queued_messages
        .lock()
        .expect("queue")
        .push_back(QueuedMessage::new(order, text.into()).with_steering(harness_steers));
}

/// Returns the transcript texts of the queued prompts on the handles.
fn queued_texts(app: &App, session_id: &str) -> Vec<String> {
    app.sessions
        .session_handles()
        .get(session_id)
        .expect("handles")
        .queued_message_snapshot()
        .iter()
        .map(|message| message.transcript_text().to_string())
        .collect()
}

/// Waits until the transcript records a `kind` message containing `text`.
async fn wait_for_message(app: &mut App, session_id: &str, kind: SessionMessageKind, text: &str) {
    let mut recorded = false;
    for _ in 0..200 {
        app.sessions.sync_from_handles();
        recorded = app
            .sessions
            .session_or_err(session_id)
            .expect("session")
            .transcript
            .as_ref()
            .is_some_and(|transcript| {
                transcript
                    .messages()
                    .iter()
                    .any(|message| message.kind == kind && message.content.contains(text))
            });
        if recorded {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(recorded, "`{text}` never reached the transcript");
}

/// Waits until the worker reports whether it is running a chat turn.
async fn wait_for_steerable_turn(app: &App, session_id: &str, expected: bool) {
    let handles = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("handles");
    for _ in 0..200 {
        if handles.has_steerable_turn() == expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(handles.has_steerable_turn(), expected);
}

/// Starts one session turn on `channel` and waits until the worker runs it.
async fn start_running_turn(app: &mut App, channel: Arc<SteerableSessionChannel>) -> String {
    let session_id = app.create_session().await.expect("session");
    TestSessionRunFactory::install(&mut app.services).register(&session_id, channel);
    app.sessions
        .reply(&app.services, &session_id, "Initial")
        .await;
    wait_for_status(app, &session_id, Status::InProgress).await;
    wait_for_steerable_turn(app, &session_id, true).await;

    session_id
}

#[tokio::test]
async fn steering_sends_leading_steerable_prompts_into_the_running_turn() {
    // Arrange
    let dir = tempdir().expect("temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let channel = Arc::new(SteerableSessionChannel::default());
    let session_id = start_running_turn(&mut app, Arc::clone(&channel)).await;
    let steered_without_messages = app.steer_queued_messages(&session_id);
    push_queued(&app, &session_id, "also add tests", true);
    push_queued(&app, &session_id, "with an image", false);
    app.sessions.sync_from_handles();
    let can_steer = app
        .sessions
        .session_or_err(&session_id)
        .expect("session")
        .can_steer_queued_messages();

    // Act
    let steered = app.steer_queued_messages(&session_id);
    wait_for_message(
        &mut app,
        &session_id,
        SessionMessageKind::UserPrompt,
        "also add tests",
    )
    .await;

    // Assert
    assert!(!steered_without_messages);
    assert!(can_steer);
    assert!(steered);
    assert_eq!(
        *channel.steered.lock().expect("steered"),
        ["also add tests"]
    );
    assert_eq!(queued_texts(&app, &session_id), ["with an image"]);
    channel.release.cancel();
    wait_for_status(&mut app, &session_id, Status::Review).await;
}

#[tokio::test]
async fn rejected_steering_requeues_the_prompt_for_the_next_turn() {
    // Arrange
    let dir = tempdir().expect("temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let channel = Arc::new(SteerableSessionChannel::default());
    let session_id = start_running_turn(&mut app, Arc::clone(&channel)).await;
    push_queued(&app, &session_id, "reject this", true);

    // Act
    let steered = app.steer_queued_messages(&session_id);
    wait_for_message(
        &mut app,
        &session_id,
        SessionMessageKind::WorkflowNotice,
        "[Queue Error] Could not send the queued message now: turn ended.",
    )
    .await;
    let requeued = queued_texts(&app, &session_id);
    channel.release.cancel();

    // Assert
    assert!(steered);
    assert_eq!(requeued, ["reject this"]);
    wait_for_message(
        &mut app,
        &session_id,
        SessionMessageKind::UserPrompt,
        "reject this",
    )
    .await;
    wait_for_status(&mut app, &session_id, Status::Review).await;
    assert!(channel.steered.lock().expect("steered").is_empty());
}

#[tokio::test]
async fn steering_answered_after_its_turn_ended_runs_before_newer_queued_prompts() {
    // Arrange: the steered prompt is still unanswered when its turn ends,
    // and a newer prompt that could not be steered waits in the queue.
    let dir = tempdir().expect("temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let channel = Arc::new(SteerableSessionChannel::default());
    let session_id = start_running_turn(&mut app, Arc::clone(&channel)).await;
    push_queued(&app, &session_id, "late addition", true);
    push_queued(&app, &session_id, "with an image", false);

    // Act
    let steered = app.steer_queued_messages(&session_id);
    channel.release.cancel();
    wait_for_steerable_turn(&app, &session_id, false).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let queued_before_the_answer = queued_texts(&app, &session_id);
    channel.late_steer.cancel();
    wait_for_message(
        &mut app,
        &session_id,
        SessionMessageKind::UserPrompt,
        "with an image",
    )
    .await;

    // Assert: the worker starts nothing until the delivery settles, so the
    // returned prompt keeps its place ahead of the newer one.
    assert!(steered);
    assert_eq!(queued_before_the_answer, ["with an image"]);
    let prompts = app
        .sessions
        .session_or_err(&session_id)
        .expect("session")
        .transcript
        .as_ref()
        .expect("transcript")
        .messages()
        .iter()
        .filter(|message| message.kind == SessionMessageKind::UserPrompt)
        .map(|message| message.content.clone())
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["Initial", "late addition", "with an image"]);
    assert!(channel.steered.lock().expect("steered").is_empty());
    wait_for_status(&mut app, &session_id, Status::Review).await;
}

#[tokio::test]
async fn steering_keeps_prompts_queued_outside_a_chat_turn() {
    // Arrange: the worker stays alive after its chat turn, then the session
    // is held in progress the way an agent-task or graph turn holds it.
    let dir = tempdir().expect("temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let channel = Arc::new(SteerableSessionChannel::default());
    let session_id = start_running_turn(&mut app, Arc::clone(&channel)).await;
    channel.release.cancel();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    wait_for_steerable_turn(&app, &session_id, false).await;
    *app.sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles")
        .status
        .lock()
        .expect("status") = Status::InProgress;
    push_queued(&app, &session_id, "queued reply", true);
    app.sessions.sync_from_handles();
    let can_steer = app
        .sessions
        .session_or_err(&session_id)
        .expect("session")
        .can_steer_queued_messages();

    // Act
    let steered = app.steer_queued_messages(&session_id);

    // Assert
    assert!(!can_steer);
    assert!(!steered);
    assert_eq!(queued_texts(&app, &session_id), ["queued reply"]);
    assert!(channel.steered.lock().expect("steered").is_empty());
}

#[tokio::test]
async fn steering_keeps_prompts_queued_while_the_turn_token_is_cancelled() {
    // Arrange: the token slot holds a stopped earlier turn's token, as it
    // does until the running chat turn installs its own.
    let dir = tempdir().expect("temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let channel = Arc::new(SteerableSessionChannel::default());
    let session_id = start_running_turn(&mut app, Arc::clone(&channel)).await;
    let cancel_token = Arc::clone(
        &app.sessions
            .session_handles()
            .get(session_id.as_str())
            .expect("handles")
            .cancel_token,
    );
    let stopped_turn_token = CancellationToken::new();
    stopped_turn_token.cancel();
    let running_turn_token = std::mem::replace(
        &mut *cancel_token.lock().expect("cancel token"),
        stopped_turn_token,
    );
    push_queued(&app, &session_id, "reject this", true);

    // Act
    let steered = app.steer_queued_messages(&session_id);
    let queued = queued_texts(&app, &session_id);
    *cancel_token.lock().expect("cancel token") = running_turn_token;
    channel.release.cancel();

    // Assert: the prompt was not taken, so it runs as the next turn instead
    // of being rejected and discarded as stopped.
    assert!(!steered);
    assert_eq!(queued, ["reject this"]);
    wait_for_message(
        &mut app,
        &session_id,
        SessionMessageKind::UserPrompt,
        "reject this",
    )
    .await;
    wait_for_status(&mut app, &session_id, Status::Review).await;
}

#[tokio::test]
async fn steering_keeps_prompts_queued_without_a_live_worker() {
    // Arrange: the turn is still marked running, but its worker is gone.
    let dir = tempdir().expect("temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app.create_session().await.expect("session");
    push_queued(&app, &session_id, "queued reply", true);
    app.sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("handles")
        .steerable_turn
        .store(true, Ordering::Release);

    // Act
    let steered = app.steer_queued_messages(&session_id);
    let missing = app.steer_queued_messages("missing-session");

    // Assert
    assert!(!steered);
    assert!(!missing);
    assert_eq!(queued_texts(&app, &session_id), ["queued reply"]);
}
