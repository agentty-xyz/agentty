use std::collections::VecDeque;
use std::future::pending;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{MockAgentChannel, TurnResult};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::{AgentResponse, ReviewCommentOutcome, ReviewCommentResolution};
use ag_worker::SessionRunClient;
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
    auto_commit_git_client_with_push_failure, auto_commit_run_client, dirty_auto_commit_git_client,
    empty_transcript, expect_safe_auto_push_state, fixed_review_turn_result, github_forge_remote,
    insert_in_progress_session_with_review_request, linked_github_review_request,
    push_descendant_that_reverted_fix, queue_test_context, queued_message, resume_command,
    review_resolution_client, review_resolution_git_client, successful_turn_result,
};
use crate::app::AppEvent;
use crate::app::branch_publish::BranchPublishTaskSession;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::question::QuestionItem;
use crate::domain::session::{PublishedBranchSyncStatus, Status};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{
    AppRepositories, NewSessionReviewCommentResolution, SessionMessageRow, SessionOperationRow,
};
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
    let run_client = auto_commit_run_client();
    let message = context
        .pop_queued_message()
        .expect("queued message should be available");
    let turn_result =
        SessionWorkerService::process_queued_message(&context, &run_client, message).await;
    let (turn_started_session_id, sync_events) =
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            let mut turn_started_session_id = None;
            while sync_events.len() < 2 || turn_started_session_id.is_none() {
                let event = app_event_rx.recv().await.expect("missing app event");
                match event {
                    AppEvent::SessionTurnStarted { session_id, .. } => {
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
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        session_run: SessionRunClient::from_channel(
            "sess1".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
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
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        session_run: SessionRunClient::from_channel(
            "sess1".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: base_dir.path().join("sess1"),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::from([queued_message(
            0,
            "Make another change",
        )]))),
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
    // The awaited result must include forge completion despite queued chat.
    assert_eq!(
        context
            .db
            .reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("operations"),
        Vec::<crate::infra::db::SessionReviewCommentResolutionRow>::new()
    );
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
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        session_run: SessionRunClient::from_channel(
            "sess1".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
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
        },
        context_reset: false,
        harness_report: None,
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
async fn test_failed_push_retains_but_does_not_resolve_review_fix_undone_by_descendant() {
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
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        session_run: SessionRunClient::from_channel(
            "sess1".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
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
    assert_eq!(remaining_operations, pending_operations);
    assert_eq!(
        notice.trim(),
        "[Review Comments Warning] Retained 1 saved review thread update(s) because the pushed \
         branch tip no longer exactly matches the reported fix commit. Continue the session to \
         re-evaluate them."
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
    let command_result =
        SessionWorkerService::process_session_command(&context, &auto_commit_run_client(), command)
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
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        session_run: SessionRunClient::from_channel(
            "sess1".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
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
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        session_run: SessionRunClient::from_channel(
            "sess1".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
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
    let result =
        SessionWorkerService::execute_session_command(&context, &auto_commit_run_client(), command)
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

#[tokio::test]
async fn skipped_review_comment_command_resolves_its_waiting_row() {
    // Arrange
    let (context, database, _queue, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Review).await;
    let mut command = resume_command("skipped-review-comments");
    if let SessionCommand::Run { turn_metadata, .. } = &mut command {
        turn_metadata.review_comment_thread_ids = vec!["thread-42".to_string()];
    }
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let context = SessionWorkerContext {
        app_event_tx: event_tx,
        ..context
    };
    database
        .operations()
        .insert_session_operation("skipped-review-comments", &context.session_id, "reply")
        .await
        .expect("queued review operation");
    database
        .operations()
        .request_cancel_for_session_operations(&context.session_id)
        .await
        .expect("cancel waiting review operation");

    // Act
    let result =
        SessionWorkerService::process_session_command(&context, &auto_commit_run_client(), command)
            .await;
    let messages = database
        .sessions()
        .load_session_messages(&context.session_id)
        .await
        .expect("persisted messages");

    // Assert
    assert!(result.is_none());
    assert_eq!(messages, [] as [SessionMessageRow; 0]);
    assert_eq!(
        context
            .transcript
            .lock()
            .expect("transcript lock")
            .messages(),
        []
    );
    assert_eq!(
        event_rx.try_recv().expect("resolution event"),
        AppEvent::SessionReviewCommentResolutionUpdated {
            comment_count: Some(0),
            session_id: context.session_id
        }
    );
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn review_targets_survive_clarification_and_restore_the_follow_up_allowlist() {
    // Arrange
    let (context, db, _, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.reviews()
        .update_session_review_request("sess1", Some(linked_github_review_request()))
        .await
        .expect("link review");
    let mut metadata = TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: vec!["thread-42".to_string()],
        session_agent: context.session_agent,
    };
    let mut prompt = TurnPrompt::from_text("Address the selected comment".to_string());
    SessionWorkerService::prepare_review_comment_turn(&context, &mut metadata, &mut prompt)
        .await
        .expect("persist targets before agent execution");
    let mut result = successful_turn_result("A decision is needed.");
    result.assistant_message.questions = vec![QuestionItem::new("Which format should this use?")];

    // Act
    let status = apply_worker_turn_result(&context, metadata, Ok(result))
        .await
        .expect("question turn");
    let mut follow_up = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: context.session_agent,
    };
    let mut answer = TurnPrompt::from_text("Use JSON".to_string());
    SessionWorkerService::prepare_review_comment_turn(&context, &mut follow_up, &mut answer)
        .await
        .expect("restore targets");

    // Assert
    assert_eq!(status, Status::Question);
    assert_eq!(follow_up.review_comment_thread_ids, vec!["thread-42"]);
    assert!(answer.text.starts_with("Use JSON"));
    assert!(answer.text.contains("[\"thread-42\"]"));
    assert!(answer.text.contains("review_comment_outcomes"));
    let transcript = context
        .transcript
        .lock()
        .expect("transcript")
        .replay_text()
        .expect("text");
    assert!(!transcript.contains("No review replies were posted"));
    assert_eq!(
        db.reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("operations"),
        Vec::<crate::infra::db::SessionReviewCommentResolutionRow>::new()
    );
}

#[tokio::test]
async fn review_continuation_merges_targets_without_duplicates_and_rejects_missing_links() {
    // Arrange
    let (context, db, _, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Review).await;
    let mut metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: vec!["thread-42".to_string()],
        session_agent: context.session_agent,
    };
    let mut prompt = TurnPrompt::from_text("Review".to_string());

    // Act
    let missing =
        SessionWorkerService::prepare_review_comment_turn(&context, &mut metadata, &mut prompt)
            .await;
    db.reviews()
        .update_session_review_request("sess1", Some(linked_github_review_request()))
        .await
        .expect("link review");
    SessionWorkerService::prepare_review_comment_turn(&context, &mut metadata, &mut prompt)
        .await
        .expect("initial turn");
    SessionWorkerService::prepare_review_comment_turn(&context, &mut metadata, &mut prompt)
        .await
        .expect("retry turn");

    // Assert
    assert!(missing.is_err());
    assert_eq!(metadata.review_comment_thread_ids, vec!["thread-42"]);
}

#[tokio::test]
async fn review_target_persistence_failure_finishes_the_turn_with_a_visible_error() {
    // Arrange
    let (context, _db, _, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    let mut command = resume_command("missing-review-link");
    if let SessionCommand::Run { turn_metadata, .. } = &mut command {
        turn_metadata.review_comment_thread_ids = vec!["thread-42".to_string()];
    }

    // Act
    let result =
        SessionWorkerService::execute_session_command(&context, &auto_commit_run_client(), command)
            .await;

    // Assert
    assert!(result.is_err());
    assert_eq!(*context.status.lock().expect("status"), Status::Review);
    assert!(
        context
            .transcript
            .lock()
            .expect("transcript")
            .replay_text()
            .expect("text")
            .contains("the session no longer has an open linked review request")
    );
}

#[tokio::test]
async fn next_turn_waits_for_previous_branch_effects() {
    // Arrange
    let mut channel = MockAgentChannel::new();
    channel.expect_run_turn().once().returning(|_, _, _| {
        Box::pin(async { Ok(successful_turn_result("Continued after publishing.")) })
    });
    let (context, _db, _, _directory) =
        queue_test_context(channel, VecDeque::new(), Status::InProgress).await;
    let guard = Arc::clone(&context.branch_operation_lock)
        .lock_owned()
        .await;
    let run_client = auto_commit_run_client();
    let mut turn = Box::pin(SessionWorkerService::execute_session_command(
        &context,
        &run_client,
        resume_command("next-turn"),
    ));

    // Act
    let blocked = tokio::time::timeout(Duration::from_millis(20), &mut turn).await;
    drop(guard);
    let result = turn.await;

    // Assert
    assert!(blocked.is_err());
    assert!(result.is_ok());
}

#[tokio::test]
async fn posted_reply_survives_revalidation_commit_failure_and_successful_retry() {
    // Arrange
    let (mut context, db, _, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.reviews()
        .update_session_review_request("sess1", Some(linked_github_review_request()))
        .await
        .expect("link review");
    context.review_request_client = Arc::new(reply_once_resolve_on_retry_client());
    let targets = vec!["thread-42".to_string()];
    let session_agent = context.session_agent;
    let metadata = || TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: targets.clone(),
        session_agent,
    };
    context.git_client = Arc::new(review_resolution_git_client(&mut Sequence::new()));
    db.reviews()
        .prepare_session_review_comment_turn("sess1", &targets)
        .await
        .expect("first turn");

    // Act: the reply succeeds, but resolving the thread fails.
    apply_worker_turn_result(&context, metadata(), Ok(fixed_review_turn_result()))
        .await
        .expect("first outcome");
    let original = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("reply awaiting resolution");
    assert_eq!(original.len(), 1);
    assert!(original[0].is_posting);
    let mut failed_commit = MockGitClient::new();
    failed_commit
        .expect_is_worktree_clean()
        .times(4)
        .returning(|_| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("commit failed".to_string())) })
        });
    failed_commit
        .expect_push_current_branch_to_remote_branch()
        .never();
    context.git_client = Arc::new(failed_commit);
    db.reviews()
        .prepare_session_review_comment_turn("sess1", &targets)
        .await
        .expect("revalidate");
    apply_worker_turn_result(&context, metadata(), Ok(fixed_review_turn_result()))
        .await
        .expect("failed commit remains recoverable");
    let retained = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("retained posting token");

    // Assert: failed commit must not erase the evidence used to recognize the
    // reply.
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].reply_token, original[0].reply_token);
    assert!(retained[0].is_posting);
    assert_eq!(retained[0].commit_hash, None);
    assert_later_push_skips_review_operations(&context).await;

    // Act: an identical revalidation succeeds and recognizes the already-posted
    // reply.
    context.git_client = Arc::new(review_resolution_git_client(&mut Sequence::new()));
    db.reviews()
        .prepare_session_review_comment_turn("sess1", &targets)
        .await
        .expect("retry turn");
    apply_worker_turn_result(&context, metadata(), Ok(fixed_review_turn_result()))
        .await
        .expect("successful retry");

    // Assert: the forge mock permits only one reply across all three turns.
    assert_eq!(
        db.reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("completed operations"),
        Vec::new()
    );
    assert_eq!(
        db.reviews()
            .load_session_review_comment_targets("sess1")
            .await
            .expect("completed targets"),
        Vec::<String>::new()
    );
    assert!(
        context
            .transcript
            .lock()
            .expect("transcript")
            .replay_text()
            .expect("notices")
            .contains("Replied to 1 review thread(s) and resolved 1 fixed thread(s)")
    );
}

/// Models a reply visible on the forge while resolution initially fails.
fn reply_once_resolve_on_retry_client() -> forge::MockReviewRequestClient {
    let posted_reply = Arc::new(Mutex::new(None::<String>));
    let mut client = forge::MockReviewRequestClient::new();
    client
        .expect_detect_remote()
        .times(2)
        .returning(|_| Ok(github_forge_remote()));
    client
        .expect_fetch_review_comment_snapshot()
        .times(2)
        .returning({
            let posted_reply = Arc::clone(&posted_reply);
            move |_, _| {
                let comments = posted_reply
                    .lock()
                    .expect("posted reply")
                    .iter()
                    .map(|body| forge::ReviewComment {
                        author: "agent".to_string(),
                        authored_by_current_user: true,
                        body: body.clone(),
                    })
                    .collect();
                Box::pin(async move {
                    Ok(forge::ReviewCommentSnapshot {
                        pr_level_comments: Vec::new(),
                        threads: vec![forge::ReviewCommentThread {
                            anchor_side: forge::ReviewCommentAnchorSide::New,
                            comments,
                            id: "thread-42".to_string(),
                            is_outdated: Some(false),
                            is_resolved: false,
                            line: Some(1),
                            path: "src/lib.rs".to_string(),
                            start_line: None,
                        }],
                    })
                })
            }
        });
    client
        .expect_reply_to_thread()
        .once()
        .returning(move |_, _, _, body| {
            *posted_reply.lock().expect("posted reply") = Some(body);
            Box::pin(async { Ok(()) })
        });
    let mut sequence = Sequence::new();
    client
        .expect_resolve_thread()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_, _, _| {
            Box::pin(async {
                Err(forge::ReviewRequestError::OperationFailed {
                    forge_kind: forge::ForgeKind::GitHub,
                    message: "resolution failed".to_string(),
                })
            })
        });
    client
        .expect_resolve_thread()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_, _, _| Box::pin(async { Ok(()) }));

    client
}

#[tokio::test]
async fn stopped_follow_up_finishes_while_previous_branch_effects_keep_the_lock() {
    // Arrange
    let (mut context, db, _, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.reviews()
        .update_session_review_request("sess1", Some(linked_github_review_request()))
        .await
        .expect("link review");
    let operation = NewSessionReviewCommentResolution {
        commit_hash: Some("published-tip".to_string()),
        reply: "Previously validated reply".to_string(),
        reply_token: "existing-token".to_string(),
        resolution: "fixed".to_string(),
        review_request_display_id: "#42".to_string(),
        thread_id: "thread-42".to_string(),
    };
    db.reviews()
        .insert_session_review_comment_resolutions("sess1", &[operation])
        .await
        .expect("pending operation");
    let mut git = MockGitClient::new();
    git.expect_main_checkout_working_tree().never();
    git.expect_tracked_worktree_status().never();
    git.expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    context.git_client = Arc::new(git);
    let guard = Arc::clone(&context.branch_operation_lock)
        .lock_owned()
        .await;
    let run_client = auto_commit_run_client();
    let mut turn = Box::pin(SessionWorkerService::execute_session_command(
        &context,
        &run_client,
        resume_command("canceled-follow-up"),
    ));

    // Act
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut turn)
            .await
            .is_err()
    );
    context.cancel_token.lock().expect("fresh token").cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), &mut turn)
        .await
        .expect("cancellation must finish without acquiring the held lock");

    // Assert
    assert!(matches!(result, Err(SessionError::StoppedByUser(_))));
    assert!(context.branch_operation_lock.try_lock().is_err());
    assert_eq!(*context.status.lock().expect("status"), Status::InProgress);
    let operations = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("retained operation");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].commit_hash.as_deref(), Some("published-tip"));
    assert_eq!(operations[0].reply_token, "existing-token");
    drop(guard);
}

#[tokio::test]
async fn stopped_review_drops_blocked_reply_before_releasing_branch_lock() {
    // Arrange, Act, Assert
    assert_stopped_review_call_retains_operation(true).await;
}

#[tokio::test]
async fn stopped_review_drops_blocked_resolution_before_releasing_branch_lock() {
    // Arrange, Act, Assert
    assert_stopped_review_call_retains_operation(false).await;
}

/// Stops a remote call that may already have exposed its durable reply token.
async fn assert_stopped_review_call_retains_operation(block_reply: bool) {
    // Arrange
    let (mut context, db, _, _directory) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.reviews()
        .update_session_review_request("sess1", Some(linked_github_review_request()))
        .await
        .expect("link review");
    db.reviews()
        .prepare_session_review_comment_turn("sess1", &["thread-42".to_string()])
        .await
        .expect("review turn");
    context.git_client = Arc::new(review_resolution_git_client(&mut Sequence::new()));
    let (entered_tx, entered_rx) = oneshot::channel();
    let (dropped_tx, dropped_rx) = oneshot::channel::<()>();
    let posted_body = Arc::new(Mutex::new(String::new()));
    context.review_request_client = Arc::new(blocked_review_call_client(
        block_reply,
        entered_tx,
        dropped_tx,
        Arc::clone(&posted_body),
    ));
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;
    let metadata = TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: vec!["thread-42".to_string()],
        session_agent: context.session_agent,
    };
    let mut turn = Box::pin(apply_worker_turn_result(
        &context,
        metadata,
        Ok(fixed_review_turn_result()),
    ));

    // Act
    tokio::select! {
        result = &mut turn => panic!("turn finished before the blocked forge call: {result:?}"),
        started = entered_rx => started.expect("forge call entered"),
    }
    assert!(context.branch_operation_lock.try_lock().is_err());
    context.cancel_token.lock().expect("token").cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), &mut turn)
        .await
        .expect("stopped forge task must complete");

    // Assert
    assert!(matches!(result, Err(SessionError::StoppedByUser(_))));
    assert!(
        dropped_rx.await.is_err(),
        "remote future must be dropped, not detached"
    );
    assert!(context.branch_operation_lock.try_lock().is_ok());
    let operations = db
        .reviews()
        .load_session_review_comment_resolutions("sess1")
        .await
        .expect("retained operation");
    assert_eq!(operations.len(), 1);
    assert!(operations[0].is_posting);
    assert!(operations[0].commit_hash.is_some());
    assert!(
        posted_body
            .lock()
            .expect("body")
            .contains(&operations[0].reply_token)
    );
    assert_eq!(
        db.reviews()
            .load_session_review_comment_targets("sess1")
            .await
            .expect("targets"),
        vec!["thread-42"]
    );
    let sync_states = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::PublishedBranchSyncUpdated { sync_status, .. } => Some(sync_status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sync_states,
        vec![
            PublishedBranchSyncStatus::InProgress,
            PublishedBranchSyncStatus::Failed
        ]
    );
}

/// Builds a forge boundary with one reply or resolution held open until
/// dropped.
fn blocked_review_call_client(
    block_reply: bool,
    entered_tx: oneshot::Sender<()>,
    dropped_tx: oneshot::Sender<()>,
    body_capture: Arc<Mutex<String>>,
) -> forge::MockReviewRequestClient {
    let mut forge = forge::MockReviewRequestClient::new();
    forge
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_forge_remote()));
    forge
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Ok(forge::ReviewCommentSnapshot {
                    pr_level_comments: Vec::new(),
                    threads: vec![forge::ReviewCommentThread {
                        anchor_side: forge::ReviewCommentAnchorSide::New,
                        comments: Vec::new(),
                        id: "thread-42".to_string(),
                        is_outdated: Some(false),
                        is_resolved: false,
                        line: Some(1),
                        path: "src/lib.rs".to_string(),
                        start_line: None,
                    }],
                })
            })
        });
    if block_reply {
        forge
            .expect_reply_to_thread()
            .once()
            .return_once(move |_, _, _, body| {
                Box::pin(async move {
                    *body_capture.lock().expect("body") = body;
                    let _drop_signal = dropped_tx;
                    entered_tx.send(()).expect("call started");
                    pending().await
                })
            });
        forge.expect_resolve_thread().never();
    } else {
        forge
            .expect_reply_to_thread()
            .once()
            .return_once(move |_, _, _, body| {
                Box::pin(async move {
                    *body_capture.lock().expect("body") = body;
                    Ok(())
                })
            });
        forge
            .expect_resolve_thread()
            .once()
            .return_once(move |_, _, _| {
                Box::pin(async move {
                    let _drop_signal = dropped_tx;
                    entered_tx.send(()).expect("call started");
                    pending().await
                })
            });
    }

    forge
}
