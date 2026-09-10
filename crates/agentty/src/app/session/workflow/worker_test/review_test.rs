use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent::{MockAgentChannel, TurnResult};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::{AgentResponse, ReviewCommentOutcome, ReviewCommentResolution};
use mockall::Sequence;
use tempfile::tempdir;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::super::super::post_turn::status_update_after_turn_result;
use super::super::{
    CREATE_REVIEW_REQUEST_OPERATION_KIND, SKIPPED_CREATE_REVIEW_REQUEST_REASON, SessionCommand,
    SessionWorkerContext, SessionWorkerService, TurnMetadata, has_unfinished_branch_operation,
};
use super::support::{
    apply_worker_turn_result, assert_later_push_skips_review_operations,
    auto_commit_git_client_with_push_failure, auto_commit_one_shot_client,
    dirty_auto_commit_git_client, empty_transcript, expect_safe_auto_push_state,
    fixed_review_turn_result, insert_in_progress_session_with_review_request,
    push_descendant_that_reverted_fix, queue_test_context, queued_message,
    review_resolution_client, review_resolution_git_client, successful_turn_result,
};
use crate::app::AppEvent;
use crate::app::branch_publish::BranchPublishTaskSession;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::{PublishedBranchSyncStatus, Status};
use crate::infra::db::{AppRepositories, SessionOperationRow};
use crate::infra::fs;
use crate::infra::personality::RealPersonalityCatalogClient;

#[tokio::test]
/// Verifies the last queued follow-up turn reloads the persisted
/// published branch and starts auto-push after the queue has drained.
async fn test_process_queued_message_auto_pushes_after_last_published_branch_follow_up() {
    // Arrange
    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .times(1)
        .withf(|session_id, request, _events| {
            session_id == "sess1" && request.prompt.text == "queued reply"
        })
        .returning(|_, _, _| Box::pin(async { Ok(successful_turn_result("Queued done.")) }));
    let queued = VecDeque::from([queued_message(0, "queued reply")]);
    let (mut context, db, queue_handle, base_dir) =
        queue_test_context(mock_channel, queued, Status::InProgress).await;
    db.sessions()
        .update_session_published_upstream_ref("sess1", Some("origin/wt/session-id".to_string()))
        .await
        .expect("failed to persist published upstream ref");

    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;

    let mut mock_git_client = MockGitClient::new();
    let main_repo_root = base_dir.path().join("main");
    mock_git_client
        .expect_detect_git_info()
        .times(2)
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .times(1)
        .returning({
            let main_repo_root = main_repo_root.clone();

            move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(Some(main_repo_root)) })
            }
        });
    mock_git_client
        .expect_tracked_worktree_status()
        .times(2)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_in_progress_operation()
        .times(1)
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(1)
        .withf(|_folder, remote_branch_name| remote_branch_name == "wt/session-id")
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
    context.git_client = Arc::new(mock_git_client);

    // Act
    let one_shot_client = auto_commit_one_shot_client();
    let message = context
        .pop_queued_message()
        .expect("queued message should be available");
    let turn_result =
        SessionWorkerService::process_queued_message(&context, &one_shot_client, message).await;
    let (turn_started_session_id, sync_events) =
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            let mut turn_started_session_id = None;
            while sync_events.len() < 2 || turn_started_session_id.is_none() {
                let event = app_event_rx.recv().await.expect("missing app event");
                match event {
                    AppEvent::SessionTurnStarted { session_id } => {
                        turn_started_session_id = Some(session_id);
                    }
                    AppEvent::PublishedBranchSyncUpdated {
                        session_id,
                        sync_operation_id,
                        sync_status,
                        ..
                    } => sync_events.push((session_id, sync_operation_id, sync_status)),
                    _ => {}
                }
            }

            (turn_started_session_id, sync_events)
        })
        .await
        .expect("timed out waiting for sync events");

    // Assert
    assert!(matches!(turn_result, Some(Ok(()))));
    assert!(queue_handle.lock().expect("queue lock").is_empty());
    assert_eq!(turn_started_session_id.as_deref(), Some("sess1"));
    assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
    assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
    assert_eq!(sync_events[0].0, "sess1");
    assert_eq!(sync_events[1].0, "sess1");
    assert_eq!(sync_events[0].1, sync_events[1].1);
}

#[tokio::test]
/// Verifies completed turns auto-push already-published session branches
/// in the background and report sync progress through app events.
async fn test_apply_turn_result_starts_background_push_for_published_branch() {
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
    expect_safe_auto_push_state(&mut mock_git_client);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(|folder, remote_branch_name| {
            folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
        })
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
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
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = Ok(successful_turn_result("Implemented the change."));

    // Act
    let turn_metadata = TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: Vec::new(),
        session_agent,
    };
    let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
        .await
        .expect("turn result should succeed");
    let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
        let mut sync_events = Vec::new();
        while sync_events.len() < 2 {
            let event = app_event_rx.recv().await.expect("missing app event");
            if let AppEvent::PublishedBranchSyncUpdated {
                session_id,
                sync_operation_id,
                sync_status,
                ..
            } = event
            {
                sync_events.push((session_id, sync_operation_id, sync_status));
            }
        }

        sync_events
    })
    .await
    .expect("timed out waiting for sync events");

    // Assert
    assert_eq!(status, Status::Review);
    assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
    assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
    assert_eq!(sync_events[0].0, "sess1");
    assert_eq!(sync_events[1].0, "sess1");
    assert_eq!(sync_events[0].1, sync_events[1].1);
}

#[tokio::test]
/// Verifies fixed, allowlisted outcomes are replied to and resolved only
/// after the completed turn reaches the published branch.
async fn test_apply_turn_result_resolves_fixed_review_threads_after_push() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let folder = base_dir.path().join("sess1");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut sequence = Sequence::new();
    let mock_git_client = review_resolution_git_client(&mut sequence);
    let review_request_client = review_resolution_client(folder.clone(), &mut sequence);
    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: base_dir.path().join("sess1"),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(review_request_client),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = fixed_review_turn_result();

    // Act
    let status = apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: vec!["thread-42".to_string()],
            session_agent,
        },
        Ok(turn_result),
    )
    .await
    .expect("turn result should succeed");
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut completed = false;
        while !completed {
            let event = app_event_rx.recv().await.expect("missing app event");
            completed = matches!(
                event,
                AppEvent::PublishedBranchSyncUpdated {
                    sync_status: PublishedBranchSyncStatus::Succeeded,
                    ..
                }
            );
        }
    })
    .await
    .expect("timed out waiting for completed branch sync");
    let notice = transcript
        .lock()
        .expect("transcript lock should be available")
        .messages()
        .last()
        .expect("resolution notice should be appended")
        .content
        .clone();
    let unfinished_operations = context
        .db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("failed to load completed review-comment operations");

    // Assert
    assert_eq!(status, Status::Review);
    assert_eq!(unfinished_operations, Vec::new());
    assert_eq!(
        notice.trim(),
        "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
    );
}

#[tokio::test]
async fn test_apply_turn_result_rejects_incomplete_review_comment_outcome_batch() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut git_client = MockGitClient::new();
    git_client
        .expect_is_worktree_clean()
        .once()
        .returning(|_| Box::pin(async { Ok(true) }));
    git_client
        .expect_push_current_branch_to_remote_branch()
        .never();
    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: base_dir.path().join("sess1"),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = TurnResult {
        assistant_message: AgentResponse {
            answer: "Implemented one change.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: vec![ReviewCommentOutcome {
                reply: "Added the first validation.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-1".to_string(),
            }],
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    };

    // Act
    let status = apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: vec!["thread-1".to_string(), "thread-2".to_string()],
            session_agent,
        },
        Ok(turn_result),
    )
    .await
    .expect("turn result should succeed");
    let transcript_text = transcript
        .lock()
        .expect("transcript lock should be available")
        .replay_text()
        .expect("validation warning should be persisted");

    // Assert
    assert_eq!(status, Status::Review);
    assert!(
        transcript_text.contains("exactly one valid outcome for 1 of 2 selected review thread(s)")
    );
    assert!(transcript_text.contains("No review replies were posted or threads resolved"));
}

#[tokio::test]
async fn test_failed_push_discards_review_fix_undone_by_descendant() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut git_client = auto_commit_git_client_with_push_failure("Fix the review comment");
    git_client
        .expect_head_hash()
        .once()
        .returning(|_| Box::pin(async { Ok("fix-commit".to_string()) }));
    let transcript = empty_transcript();
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
        git_client: Arc::new(git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = fixed_review_turn_result();

    // Act
    apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: vec!["thread-42".to_string()],
            session_agent,
        },
        Ok(turn_result),
    )
    .await
    .expect("turn result should succeed");
    let first_push_events = tokio::time::timeout(Duration::from_secs(1), async {
        let mut sync_events = Vec::new();
        while sync_events.len() < 2 {
            let event = app_event_rx.recv().await.expect("missing app event");
            if let AppEvent::PublishedBranchSyncUpdated { sync_status, .. } = event {
                sync_events.push(sync_status);
            }
        }

        sync_events
    })
    .await
    .expect("timed out waiting for failed push");
    let pending_operations = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("failed to load pending review operation");
    push_descendant_that_reverted_fix(&context).await;
    let remaining_operations = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("failed to load discarded review operation");
    let notice = transcript
        .lock()
        .expect("transcript lock should be available")
        .messages()
        .last()
        .expect("stale-operation notice should be appended")
        .content
        .clone();

    // Assert
    assert_eq!(
        first_push_events,
        vec![
            PublishedBranchSyncStatus::InProgress,
            PublishedBranchSyncStatus::Failed,
        ]
    );
    assert_eq!(pending_operations.len(), 1);
    assert_eq!(
        pending_operations[0].commit_hash.as_deref(),
        Some("fix-commit")
    );
    assert_eq!(remaining_operations, Vec::new());
    assert_eq!(
        notice.trim(),
        "[Review Comments Warning] Discarded 1 saved review thread update(s) because the pushed \
         branch tip no longer exactly matches the reported fix commit. Reopen review comments to \
         retry."
    );
}

#[test]
fn test_status_update_after_turn_result_falls_back_to_review_for_errors() {
    // Arrange
    let result = Err(SessionError::Workflow("backend failed".to_string()));

    // Act
    let status_update = status_update_after_turn_result(&result);

    // Assert
    assert_eq!(status_update, Some(Status::Review));
}

#[tokio::test]
/// Verifies restart recovery marks unfinished operations failed and
/// restores affected sessions to `Review`.
async fn test_fail_unfinished_operations_from_previous_run_restores_session_review_status() {
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
    db.sessions()
        .update_session_status_with_timing_at("sess1", "InProgress", 0)
        .await
        .expect("failed to open in-progress timing window");
    db.operations()
        .insert_session_operation("op-1", "sess1", "reply")
        .await
        .expect("failed to insert session operation");
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_is_rebase_in_progress().times(0);
    mock_git_client.expect_abort_rebase().times(0);

    // Act
    SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(mock_git_client),
        300,
    )
    .await
    .expect("restart recovery should complete");
    let sessions = db
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let operation_is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].status, "Review");
    assert_eq!(sessions[0].in_progress_started_at, None);
    assert_eq!(sessions[0].in_progress_total_seconds, 300);
    assert!(!operation_is_unfinished);
}

#[test]
fn test_unfinished_branch_operation_includes_review_request_creation() {
    // Arrange
    let operations = vec![SessionOperationRow {
        cancel_requested: false,
        finished_at: None,
        heartbeat_at: None,
        id: "op-review-request".to_string(),
        kind: CREATE_REVIEW_REQUEST_OPERATION_KIND.to_string(),
        last_error: None,
        queued_at: 0,
        session_id: "sess1".to_string(),
        started_at: None,
        status: "queued".to_string(),
    }];

    // Act
    let has_branch_operation = has_unfinished_branch_operation(&operations, "sess1");
    let other_session_has_branch_operation = has_unfinished_branch_operation(&operations, "sess2");

    // Assert
    assert!(has_branch_operation);
    assert!(!other_session_has_branch_operation);
}

#[tokio::test]
async fn test_skipped_review_request_command_answers_programmatic_caller() {
    // Arrange
    let (mut context, db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Review).await;
    db.operations()
        .insert_session_operation(
            "op-review-request",
            &context.session_id,
            CREATE_REVIEW_REQUEST_OPERATION_KIND,
        )
        .await
        .expect("review-request operation should be inserted");
    db.operations()
        .request_cancel_for_session_operations(&context.session_id)
        .await
        .expect("review-request operation should be canceled");
    let (response_tx, response_rx) = oneshot::channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;
    let command = SessionCommand::CreateReviewRequest {
        branch_publish_session: BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder: context.folder.clone(),
            id: context.session_id.clone(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Review,
        },
        operation_id: "op-review-request".to_string(),
        remote_branch_name: None,
        response: Some(Arc::new(Mutex::new(Some(response_tx)))),
    };

    // Act
    let command_result = SessionWorkerService::process_session_command(
        &context,
        &auto_commit_one_shot_client(),
        command,
    )
    .await;
    let response = response_rx
        .await
        .expect("skipped review-request response should be delivered");
    let app_event = app_event_rx
        .recv()
        .await
        .expect("skipped review-request should resolve its queued row");

    // Assert
    assert!(command_result.is_none());
    assert_eq!(
        response,
        Err(ag_session::SessionError::Operation(
            SKIPPED_CREATE_REVIEW_REQUEST_REASON.to_string()
        ))
    );
    assert!(matches!(
        app_event,
        AppEvent::BranchPublishActionResolved { session_id } if session_id == "sess1"
    ));
    assert!(app_event_rx.try_recv().is_err());
}

#[tokio::test]
async fn test_commit_failure_discards_review_operations_before_later_push() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut git_client = MockGitClient::new();
    git_client
        .expect_is_worktree_clean()
        .times(4)
        .returning(|_| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("commit failed".to_string())) })
        });
    git_client
        .expect_push_current_branch_to_remote_branch()
        .never();
    let mut review_request_client = forge::MockReviewRequestClient::new();
    review_request_client.expect_detect_remote().never();
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .never();
    review_request_client.expect_reply_to_thread().never();
    review_request_client.expect_resolve_thread().never();
    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: base_dir.path().join("sess1"),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(review_request_client),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = TurnResult {
        assistant_message: AgentResponse {
            answer: "Implemented the change.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: vec![ReviewCommentOutcome {
                reply: "Added the missing validation.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-42".to_string(),
            }],
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    };

    // Act
    let status = apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: vec!["thread-42".to_string()],
            session_agent,
        },
        Ok(turn_result),
    )
    .await
    .expect("turn result should succeed");
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    let transcript_text = transcript
        .lock()
        .expect("transcript lock should be available")
        .replay_text()
        .expect("commit failure notices should be persisted");
    let unfinished_operations = context
        .db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("failed to load discarded review-comment operation");

    // Act
    assert_later_push_skips_review_operations(&context).await;

    // Assert
    assert_eq!(status, Status::Review);
    assert_eq!(unfinished_operations, Vec::new());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }))
    );
    assert!(transcript_text.contains("[Commit Error]"));
    assert!(transcript_text.contains("repeated identical commit failure"));
    assert!(
        transcript_text
            .contains("could not commit the review-comment changes, so it did not push the branch")
    );
}

#[tokio::test]
async fn test_commit_binding_failure_retains_review_operation_for_fresh_retry() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let (app_event_tx, _) = mpsc::unbounded_channel();
    let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
    let mut git_client = dirty_auto_commit_git_client("Fix the review comment");
    git_client.expect_head_hash().once().returning(|_| {
        Box::pin(async {
            Err(ag_git::GitError::OutputParse(
                "commit binding interrupted".to_string(),
            ))
        })
    });
    git_client
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
        git_client: Arc::new(git_client),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let error = apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: vec!["thread-42".to_string()],
            session_agent,
        },
        Ok(fixed_review_turn_result()),
    )
    .await
    .expect_err("commit binding should fail");
    let pending_operations = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("failed to load binding-pending review operation");

    // Assert
    assert!(error.to_string().contains("commit binding interrupted"));
    assert_eq!(pending_operations.len(), 1);
    assert!(pending_operations[0].commit_hash.is_none());
}

#[tokio::test]
async fn test_create_review_request_command_waits_for_live_review_status() {
    // Arrange
    let (mut context, _db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Done).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;
    let (response_tx, response_rx) = oneshot::channel();
    let command = SessionCommand::CreateReviewRequest {
        branch_publish_session: BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder: context.folder.clone(),
            id: context.session_id.clone(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::InProgress,
        },
        operation_id: "op-review-request".to_string(),
        remote_branch_name: None,
        response: Some(Arc::new(Mutex::new(Some(response_tx)))),
    };

    // Act
    let result = SessionWorkerService::execute_session_command(
        &context,
        &auto_commit_one_shot_client(),
        command,
    )
    .await;
    let response = response_rx
        .await
        .expect("review-request response should be delivered");
    let started_event = app_event_rx
        .recv()
        .await
        .expect("publish-start event should be emitted");
    let completed_event = app_event_rx
        .recv()
        .await
        .expect("publish-complete event should be emitted");

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::Workflow(message))
            if message == "Session must be in review to publish the review request."
    ));
    assert_eq!(
        response,
        Err(ag_session::SessionError::Operation(
            "Session must be in review to publish the review request.".to_string()
        ))
    );
    assert!(matches!(
        started_event,
        AppEvent::BranchPublishActionStarted { session_id } if session_id == "sess1"
    ));
    assert!(matches!(
        completed_event,
        AppEvent::BranchPublishActionCompleted { result, session_id }
            if result.is_err() && session_id == "sess1"
    ));
}
