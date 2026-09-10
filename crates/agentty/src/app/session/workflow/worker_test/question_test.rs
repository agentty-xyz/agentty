use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use ag_agent::{AgentRequestKind, MockAgentChannel};
use ag_protocol::AgentResponse;
use tokio::sync::{Notify, mpsc};

use super::super::super::post_turn::build_assistant_message_content;
use super::super::super::turn::run_channel_turn;
use super::super::{
    ScheduledSessionCommand, SessionCommand, SessionWorkerHandle, SessionWorkerService,
};
use super::support::{auto_commit_one_shot_client, default_turn_metadata, queue_test_context};
use crate::app::AppEvent;
use crate::domain::question::QuestionItem;
use crate::domain::session::{SessionId, Status};
use crate::domain::turn_prompt::TurnPrompt;

#[test]
/// Ensures assistant message content falls back to question text when no
/// answers are present.
fn test_build_assistant_message_content_falls_back_to_question_text() {
    // Arrange
    let response = AgentResponse {
        answer: String::new(),
        questions: vec![QuestionItem::new("Should I apply the patch?")],
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let message_content = build_assistant_message_content(&response);

    // Assert
    assert_eq!(
        message_content,
        Some("Should I apply the patch?\n\n".to_string())
    );
}

#[test]
fn test_agent_response_questions_returns_only_question_messages() {
    // Arrange
    let agent_response = AgentResponse {
        answer: "Implemented the feature.".to_string(),
        questions: vec![
            QuestionItem::new("Need a target branch?"),
            QuestionItem::new("Need migration notes?"),
        ],
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let items = agent_response.question_items();

    // Assert
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].text, "Need a target branch?");
    assert_eq!(items[1].text, "Need migration notes?");
}

#[test]
fn test_agent_response_questions_preserves_ordered_list_as_single_question_text() {
    // Arrange
    let numbered_questions = "1) Is this repository intentionally incomplete (docs-only), or \
                              should it include the referenced dotfiles tree (for\nexample \
                              `.config/` and `lua/`)?\n2) Should I propose and apply a docs-only \
                              cleanup now (aligning setup steps to the current files), or keep \
                              docs\nas-is and treat missing files as a known gap?\n3) Do you want \
                              keyd instructions rewritten to the safer `/etc/keyd/default.conf` \
                              path with existence checks and\nrollback notes?";
    let agent_response = AgentResponse {
        answer: String::new(),
        questions: vec![QuestionItem::new(numbered_questions)],
        review_comment_outcomes: Vec::new(),
        subtasks: Vec::new(),
        verification_verdicts: Vec::new(),
    };

    // Act
    let items = agent_response.question_items();

    // Assert
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text, numbered_questions);
}

#[tokio::test]
async fn test_worker_wakeup_resumes_buffered_action_after_question_cancel() {
    // Arrange
    let (mut context, _db, _queue_handle, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Question).await;
    let status = Arc::clone(&context.status);
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let wakeup = Arc::new(Notify::new());
    let mut worker_service = SessionWorkerService::new();
    worker_service.workers.insert(
        SessionId::from("sess1"),
        SessionWorkerHandle {
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            sender: command_tx.clone(),
            wakeup: Arc::clone(&wakeup),
        },
    );
    SessionWorkerService::spawn_session_worker(
        context,
        auto_commit_one_shot_client(),
        Arc::clone(&wakeup),
        command_rx,
    );
    command_tx
        .send(ScheduledSessionCommand::queued(
            SessionCommand::Rebase {
                base_branch: "main".to_string(),
                operation_id: "already-resolved-rebase".to_string(),
            },
            0,
        ))
        .expect("failed to queue rebase");
    tokio::task::yield_now().await;

    // Act
    *status.lock().expect("status lock") = Status::Review;
    worker_service.wake_session_worker("sess1");
    let resolved_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv()).await;

    // Assert
    assert!(matches!(
        resolved_event,
        Ok(Some(AppEvent::SessionQueuedSyncResolved { session_id }))
            if session_id == "sess1"
    ));
}

#[tokio::test]
async fn test_non_session_turn_keeps_questions_and_status_before_provider_work() {
    // Arrange
    let started = Arc::new(Notify::new());
    let started_for_agent = Arc::clone(&started);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(move |_, request, _| {
            assert_eq!(request.request_kind, AgentRequestKind::FocusedReview);
            started_for_agent.notify_one();

            Box::pin(std::future::pending())
        });
    let (context, db, _queue, _directory) =
        queue_test_context(channel, VecDeque::new(), Status::Question).await;
    db.sessions()
        .update_session_questions("sess1", "retained questions")
        .await
        .expect("questions");

    // Act
    let turn = run_channel_turn(
        &context,
        auto_commit_one_shot_client(),
        default_turn_metadata(),
        AgentRequestKind::FocusedReview,
        None,
        TurnPrompt::from_text("review this change".to_string()),
    );
    tokio::pin!(turn);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = started.notified() => {}
            result = &mut turn => panic!("turn ended before provider work: {result:?}"),
        }
    })
    .await
    .expect("provider must start");

    // Assert
    assert_eq!(*context.status.lock().expect("status"), Status::Question);
    let session = db
        .sessions()
        .load_session("sess1")
        .await
        .expect("load")
        .expect("row");
    assert_eq!(session.questions.as_deref(), Some("retained questions"));
}
