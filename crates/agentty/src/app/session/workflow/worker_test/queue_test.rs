use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent::{AgentError, MockAgentChannel, TurnResult};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::AgentResponse;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::{
    ScheduledSessionCommand, ScheduledSessionWork, SessionCommand, SessionWorkerContext,
    SessionWorkerService, TurnMetadata,
};
use super::support::{
    apply_worker_turn_result, auto_commit_one_shot_client, empty_transcript,
    preparation_test_worker_context, queue_helper_context, queue_saved_stacked_prompt,
    queue_test_context, queued_message, resume_command,
};
use crate::app::AppEvent;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::Status;
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::infra::personality::RealPersonalityCatalogClient;

#[tokio::test]
/// Verifies that the scheduler clears every queued prompt once the
/// running queued turn returns `StoppedByUser`, matching the `Ctrl+C`
/// expectation that cancellation drops pending follow-ups together with
/// the active turn.
async fn test_process_queued_message_clears_queue_when_user_stops_running_turn() {
    // Arrange
    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .times(1)
        .returning(|_, _, _| {
            Box::pin(async {
                Err(AgentError::InterruptedByUser(
                    "[Stopped] Session interrupted by user.".to_string(),
                ))
            })
        });
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));
    let queued = VecDeque::from([
        queued_message(0, "queued first"),
        queued_message(1, "queued second"),
    ]);
    let (context, _db, queue_handle, _base_dir) =
        queue_test_context(mock_channel, queued, Status::InProgress).await;

    // Act
    let one_shot_client = auto_commit_one_shot_client();
    let message = context
        .pop_queued_message()
        .expect("queued message should be available");
    let turn_result =
        SessionWorkerService::process_queued_message(&context, &one_shot_client, message).await;
    SessionWorkerService::clear_queued_messages_after_stop(&context, turn_result.as_ref());

    // Assert — first prompt was dispatched, the stopped result propagated,
    // and the remaining queued prompt was cleared.
    assert!(matches!(
        turn_result,
        Some(Err(SessionError::StoppedByUser(_)))
    ));
    let queue = queue_handle.lock().expect("queue lock");
    assert!(queue.is_empty(), "queue should be cleared on Ctrl+C");
}

#[tokio::test]
/// Verifies completed turns leave auto-push idle while queued follow-up
/// messages are waiting to run.
async fn test_apply_turn_result_skips_background_push_while_messages_are_queued() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .never();
    let context = SessionWorkerContext {
        app_event_tx,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().join("sess1"),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(mock_git_client),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::from([queued_message(
            0,
            "queued follow-up",
        )]))),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = Ok(TurnResult {
        assistant_message: AgentResponse {
            answer: "Implemented the change.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    });

    // Act
    let turn_metadata = TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: Vec::new(),
        session_agent,
    };
    let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
        .await
        .expect("turn result should succeed");
    let mut emitted_sync_event = false;
    while let Ok(event) = app_event_rx.try_recv() {
        if matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }) {
            emitted_sync_event = true;
        }
    }

    // Assert
    assert_eq!(status, Status::Review);
    assert!(
        !emitted_sync_event,
        "queued follow-up messages should suppress post-turn auto-push events"
    );
}

#[tokio::test]
async fn test_pop_queued_message_returns_messages_in_submission_order() {
    // Arrange
    let queue = Arc::new(Mutex::new(VecDeque::from([
        queued_message(0, "first"),
        queued_message(1, "second"),
    ])));
    let context = queue_helper_context(Arc::clone(&queue)).await;

    // Act
    let first_pop = context.pop_queued_message();
    let second_pop = context.pop_queued_message();
    let empty_pop = context.pop_queued_message();

    // Assert
    assert_eq!(first_pop.expect("first prompt").transcript_text(), "first");
    assert_eq!(
        second_pop.expect("second prompt").transcript_text(),
        "second"
    );
    assert!(empty_pop.is_none());
    assert!(queue.lock().expect("queue lock").is_empty());
}

#[tokio::test]
async fn test_clear_queued_messages_drops_all_pending_prompts() {
    // Arrange
    let queue = Arc::new(Mutex::new(VecDeque::from([
        queued_message(0, "alpha"),
        queued_message(1, "beta"),
    ])));
    let context = queue_helper_context(Arc::clone(&queue)).await;

    // Act
    context.clear_queued_messages();

    // Assert
    assert!(queue.lock().expect("queue lock").is_empty());
}

#[tokio::test]
async fn test_clear_queued_messages_updates_shared_queue_state() {
    // Arrange
    let queue = Arc::new(Mutex::new(VecDeque::from([queued_message(
        0,
        "queued reply",
    )])));
    let context = queue_helper_context(Arc::clone(&queue)).await;

    // Act
    let has_queued_before_clear = !queue.lock().expect("queue lock").is_empty();
    context.clear_queued_messages();
    let has_queued_after_clear = !queue.lock().expect("queue lock").is_empty();

    // Assert
    assert!(has_queued_before_clear);
    assert!(!has_queued_after_clear);
}

#[tokio::test]
async fn test_repeat_staged_start_preserves_queued_first_prompt_acceptance() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_git_test_app().await;
    let parent_id = app.create_session().await.expect("parent");
    crate::test_support::set_session_status_for_test(&mut app, &parent_id, Status::Review);
    let (child_id, mut receiver) = queue_saved_stacked_prompt(&mut app, &parent_id).await;
    let original = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&child_id)
        .await
        .expect("load")
        .expect("preparation");

    // Act: repeat the public start action and the direct retry while
    // the first command remains queued and the session remains Draft.
    let repeated_start = app.start_staged_session(&child_id).await;
    let repeated_retry = app.retry_workspace_preparation(&child_id).await;
    let after_retry = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&child_id)
        .await
        .expect("load")
        .expect("preparation");

    // Assert
    assert!(repeated_start.is_err());
    assert!(
        repeated_retry
            .expect_err("queued retry")
            .to_string()
            .contains("already queued")
    );
    assert_eq!(
        original.state,
        crate::infra::db::SessionPreparationState::Ready
    );
    assert!(original.prompt.is_some());
    assert_eq!(after_retry.state, original.state);
    assert_eq!(after_retry.prompt, original.prompt);
    assert_eq!(after_retry.error, original.error);
    assert!(!app.sessions.can_start_staged_session(&child_id));

    // Act: the original worker can still atomically accept its saved
    // prompt after both rejected repeats.
    let mut command = receiver.try_recv().expect("original command");
    command
        .ready_rx
        .take()
        .expect("gate")
        .await
        .expect("released");
    let context = preparation_test_worker_context(&app, &child_id);
    let accepted = SessionWorkerService::begin_preparation_prompt(
        &context,
        command.command.operation_id(),
        "saved child prompt",
    )
    .await
    .expect("accept original");
    let after_acceptance = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&child_id)
        .await
        .expect("load")
        .expect("preparation");

    // Assert
    assert!(accepted);
    assert!(after_acceptance.prompt.is_none());
    assert!(
        receiver.try_recv().is_err(),
        "only the original turn was queued"
    );
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session_messages(&child_id)
            .await
            .expect("messages")
            .len(),
        1
    );
}

#[tokio::test]
async fn test_next_scheduled_work_pauses_queued_work_for_question() {
    // Arrange
    let queued = VecDeque::from([queued_message(1, "queued reply")]);
    let (context, _db, queue_handle, _base_dir) =
        queue_test_context(MockAgentChannel::new(), queued, Status::Question).await;
    let mut pending_commands = VecDeque::from([ScheduledSessionCommand::queued(
        SessionCommand::Rebase {
            base_branch: "main".to_string(),
            operation_id: "queued-rebase".to_string(),
        },
        0,
    )]);

    // Act
    let paused_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
    pending_commands.push_back(ScheduledSessionCommand::queued(
        resume_command("question-answer"),
        2,
    ));
    let answer_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);

    // Assert
    assert!(paused_work.is_none());
    assert!(matches!(
        answer_work,
        Some(ScheduledSessionWork::Command(command))
            if command.queued_order == Some(2)
                && matches!(
                    &command.command,
                    SessionCommand::Run { operation_id, .. }
                        if operation_id == "question-answer"
                )
    ));
    assert_eq!(pending_commands.len(), 1);
    assert_eq!(queue_handle.lock().expect("queue lock").len(), 1);
}

#[tokio::test]
async fn test_queued_saved_child_reserves_stack_until_worker_rejects_acceptance() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let parent_id = app.create_session().await.expect("parent");
    crate::test_support::set_session_status_for_test(&mut app, &parent_id, Status::Review);

    // Act: pause the worker after foreground enqueue, before acceptance.
    let (child_id, receiver) = queue_saved_stacked_prompt(&mut app, &parent_id).await;

    // Assert: the child is still Draft but the parent cannot claim branch
    // work.
    assert_eq!(
        app.sessions
            .session_for_id(&child_id)
            .expect("child")
            .status,
        Status::Draft
    );
    assert!(!app.sessions.can_reply_to_session_in_stack(&parent_id));
    assert!(!app.sessions.can_rebase_session_branch_in_stack(&parent_id));
    assert!(!app.sessions.can_merge_session_branch_in_stack(&parent_id));
    assert!(!app.sessions.can_mutate_session_branch_in_stack(&parent_id));
    assert!(!app.sessions.can_start_staged_session(&child_id));
    assert!(
        !app.sessions
            .reply(&app.services, &parent_id, "competing turn")
            .await
    );

    // Act: let the real worker reject the durable transfer.
    sqlx::query(
        "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
         NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject acceptance");
    SessionWorkerService::spawn_session_worker(
        preparation_test_worker_context(&app, &child_id),
        auto_commit_one_shot_client(),
        Arc::default(),
        receiver,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while app
            .sessions
            .worker_service
            .has_preparation_reservation(&child_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker releases reservation");

    // Assert: rejection releases branch work without consuming the prompt.
    assert!(app.sessions.can_reply_to_session_in_stack(&parent_id));
    assert!(app.sessions.can_start_staged_session(&child_id));
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&child_id)
        .await
        .expect("load")
        .expect("preparation");
    assert_eq!(
        preparation.state,
        crate::infra::db::SessionPreparationState::Failed
    );
    assert!(preparation.prompt.is_some());
    assert!(
        app.sessions
            .session_for_id(&child_id)
            .expect("child")
            .is_draft_session()
    );
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session_messages(&child_id)
            .await
            .expect("messages"),
        []
    );
}
