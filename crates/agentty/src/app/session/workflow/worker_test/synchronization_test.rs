use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent as agent;
use ag_agent::{MockAgentChannel, PermissionMode, TurnResult};
use ag_forge as forge;
use ag_git::MockGitClient;
use ag_protocol::AgentResponse;
use mockall::Sequence;
use tempfile::tempdir;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::super::{
    CREATE_REVIEW_REQUEST_OPERATION_KIND, REBASE_OPERATION_KIND, ScheduledSessionCommand,
    SessionCommand, SessionWorkerContext, SessionWorkerService, TurnMetadata,
};
use super::support::{
    apply_worker_turn_result, auto_commit_git_client, auto_commit_git_client_with_push_failure,
    auto_commit_one_shot_client, blocking_stack_metadata_git_client, empty_transcript,
    insert_in_progress_session_with_review_request, insert_in_progress_test_session,
    mock_successful_conflict_rebase_git_client, queue_helper_context, queue_test_context,
    queued_review_request_command, rebase_assist_worker_harness, review_metadata_sync_client,
    seed_existing_session_rebase_metadata, seed_recovery_test_operation, successful_turn_result,
    transcript_text, write_rebase_conflict_file,
};
use crate::app::AppEvent;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::{PublishedBranchSyncStatus, Status};
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::infra::personality::RealPersonalityCatalogClient;

#[tokio::test]
/// Verifies completed turns keep a linked open PR/MR title and
/// description aligned with the latest session commit message.
async fn test_apply_turn_result_syncs_linked_review_request_metadata_after_commit() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let folder = base_dir.path().join("sess1");
    let commit_message = "Refine review metadata sync\n\n- Update the linked review request body.";
    let mut sequence = Sequence::new();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let context = SessionWorkerContext {
        app_event_tx,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder,
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(auto_commit_git_client(commit_message, &mut sequence)),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(review_metadata_sync_client(
            base_dir.path(),
            &mut sequence,
        )),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let turn_result = successful_turn_result("Implemented the change.");

    // Act
    let status = apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                crate::domain::agent::AgentModel::Gemini38Flash,
            ),
        },
        Ok(turn_result),
    )
    .await
    .expect("turn result should succeed");
    let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
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
    .expect("timed out waiting for sync events");
    let review_request = db
        .reviews()
        .load_session_review_request("sess1")
        .await
        .expect("failed to load review request")
        .expect("review request should remain linked");

    // Assert
    assert_eq!(status, Status::Review);
    assert_eq!(
        sync_events,
        vec![
            PublishedBranchSyncStatus::InProgress,
            PublishedBranchSyncStatus::Succeeded,
        ]
    );
    assert_eq!(review_request.title, "Old title");
}

#[tokio::test]
/// Verifies failed post-turn auto-push skips linked PR/MR metadata sync.
async fn test_apply_turn_result_skips_review_request_metadata_sync_when_auto_push_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_session_with_review_request(&db).await;
    let commit_message = "Refine review metadata sync\n\n- Update the linked review request body.";
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
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
        git_client: Arc::new(auto_commit_git_client_with_push_failure(commit_message)),
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

    // Act
    let status = apply_worker_turn_result(
        &context,
        TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                crate::domain::agent::AgentModel::Gemini38Flash,
            ),
        },
        Ok(successful_turn_result("Implemented the change.")),
    )
    .await
    .expect("turn result should succeed");
    let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
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
    .expect("timed out waiting for sync events");
    let review_request = db
        .reviews()
        .load_session_review_request("sess1")
        .await
        .expect("failed to load review request")
        .expect("review request should remain linked");

    // Assert
    assert_eq!(status, Status::Review);
    assert_eq!(
        sync_events,
        vec![
            PublishedBranchSyncStatus::InProgress,
            PublishedBranchSyncStatus::Failed,
        ]
    );
    assert_eq!(review_request.title, "Old title");
}

#[tokio::test]
/// Verifies completed turns leave auto-push idle while queued sync will
/// publish the branch after rebasing.
async fn test_apply_turn_result_skips_background_push_while_sync_is_queued() {
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
    db.operations()
        .insert_session_operation("queued-sync", "sess1", "rebase")
        .await
        .expect("failed to insert queued sync operation");
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
        db,
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
        "queued sync should suppress post-turn auto-push events"
    );
}

#[tokio::test]
async fn test_queued_rebase_validation_failure_persists_error_before_resolving_row() {
    // Arrange
    let mut context = queue_helper_context(Arc::new(Mutex::new(VecDeque::new()))).await;
    context.session_id = "sess1".into();
    context.folder = PathBuf::from("missing-session-worktree");
    *context.status.lock().expect("status lock") = Status::Review;
    insert_in_progress_test_session(&context.db).await;
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().once().return_const(false);
    context.fs_client = Arc::new(fs_client);
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;

    // Act
    let error = SessionWorkerService::run_rebase_command(
        &context,
        auto_commit_one_shot_client(),
        "main".to_string(),
    )
    .await
    .expect_err("missing worktree should reject queued sync");
    let persisted_messages = context
        .db
        .sessions()
        .load_session_messages("sess1")
        .await
        .expect("failed to load persisted session messages");
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert!(error.to_string().contains("Session isolation violation"));
    assert!(transcript_text(&context.transcript).contains("[Sync Error]"));
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(persisted_messages[0].kind, "workflow_notice");
    assert!(persisted_messages[0].content.contains("[Sync Error]"));
    assert!(matches!(
        events.as_slice(),
        [
            AppEvent::SessionUpdated { session_id, .. },
            AppEvent::SessionQueuedSyncResolved {
                session_id: resolved_session_id,
            },
        ] if session_id == "sess1" && resolved_session_id == "sess1"
    ));
    assert_eq!(*context.status.lock().expect("status lock"), Status::Review);
}

#[tokio::test]
/// Verifies session rebase conflict assistance runs through the existing
/// session channel, preserving provider conversation identifiers while
/// Agentty owns staging and `git rebase --continue`.
async fn test_run_rebase_command_uses_existing_session_channel_for_conflicts() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    write_rebase_conflict_file(base_dir.path());
    let db = AppRepositories::in_memory().await.expect("db should open");
    seed_existing_session_rebase_metadata(&db, None).await;
    db.sessions()
        .update_session_permission_mode("sess1", PermissionMode::ReadOnly)
        .await
        .expect("failed to set chat permission mode");
    let harness = rebase_assist_worker_harness(
        base_dir.path().to_path_buf(),
        db,
        mock_successful_conflict_rebase_git_client,
    );

    // Act
    SessionWorkerService::run_rebase_command(
        &harness.context,
        auto_commit_one_shot_client(),
        "main".to_string(),
    )
    .await
    .expect("rebase command should complete");
    let provider_conversation_id = harness
        .db
        .sessions()
        .get_session_provider_conversation_id("sess1")
        .await
        .expect("failed to load provider conversation id");
    let instruction_conversation_id = harness
        .db
        .sessions()
        .get_session_instruction_conversation_id("sess1")
        .await
        .expect("failed to load instruction conversation id");
    let output_text = transcript_text(&harness.context.transcript);
    let final_status = *harness.status.lock().expect("status lock");

    // Assert
    assert_eq!(provider_conversation_id.as_deref(), Some("thread-after"));
    assert_eq!(
        instruction_conversation_id,
        agent::normalize_instruction_conversation_id(Some("thread-after"))
    );
    assert_eq!(final_status, Status::Review);
    assert!(output_text.contains("[Sync Assist] Attempt 1/3. Resolving conflicts in:"));
    assert!(output_text.contains("- src/lib.rs"));
    assert!(output_text.contains("Resolved conflicts inside existing session."));
    assert!(output_text.contains("[Sync] Successfully synced wt/sess1 onto main"));
}

#[tokio::test]
async fn test_skipped_rebase_command_resolves_queued_sync() {
    // Arrange
    let (mut context, db, _queue, _base_dir) =
        queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
    db.operations()
        .insert_session_operation("op-rebase", &context.session_id, REBASE_OPERATION_KIND)
        .await
        .expect("rebase operation should be inserted");
    db.operations()
        .request_cancel_for_session_operations(&context.session_id)
        .await
        .expect("rebase operation should be canceled");
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;
    let command = SessionCommand::Rebase {
        base_branch: "main".to_string(),
        operation_id: "op-rebase".to_string(),
    };

    // Act
    let command_result = SessionWorkerService::process_session_command(
        &context,
        &auto_commit_one_shot_client(),
        command,
    )
    .await;
    let app_event = app_event_rx
        .recv()
        .await
        .expect("skipped rebase should resolve its queued row");

    // Assert
    assert!(command_result.is_none());
    assert!(matches!(
        app_event,
        AppEvent::SessionQueuedSyncResolved { session_id } if session_id == "sess1"
    ));
    assert!(app_event_rx.try_recv().is_err());
}

#[tokio::test]
/// Verifies a removed worktree does not trap startup in recovery.
async fn test_restart_recovery_completes_for_missing_rebase_worktree() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    seed_recovery_test_operation(&db, Status::Rebasing, REBASE_OPERATION_KIND).await;

    // Act
    SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(ag_git::RealGitClient),
        300,
    )
    .await
    .expect("missing worktree should not prevent recovery");
    let sessions = db
        .sessions()
        .load_sessions()
        .await
        .expect("sessions should load");
    let unfinished = db
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations should load");

    // Assert
    assert_eq!(sessions[0].status, "Review");
    assert!(unfinished.is_empty());
}

#[tokio::test]
/// Verifies recovery leaves the operation unfinished when stale rebase
/// cleanup fails.
async fn test_fail_unfinished_operations_from_previous_run_returns_rebase_cleanup_error() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    seed_recovery_test_operation(&db, Status::Rebasing, REBASE_OPERATION_KIND).await;
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_rebase_in_progress()
        .once()
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client.expect_abort_rebase().once().returning(|_| {
        Box::pin(async { Err(ag_git::GitError::OutputParse("abort failed".to_string())) })
    });

    // Act
    let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(mock_git_client),
        300,
    )
    .await;
    let operation_is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(matches!(result, Err(SessionError::Git(_))));
    assert!(operation_is_unfinished);
}

#[tokio::test]
/// Verifies restart recovery aborts stale rebase metadata for interrupted
/// rebase operations before restoring review state.
async fn test_fail_unfinished_operations_from_previous_run_aborts_interrupted_rebase() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("sess1", "gemini-3.8-flash", "main", "Rebasing", project_id)
        .await
        .expect("failed to insert session");
    db.operations()
        .insert_session_operation("op-1", "sess1", "rebase")
        .await
        .expect("failed to insert session operation");
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_rebase_in_progress()
        .once()
        .withf(|repo_path| repo_path.ends_with("sess1"))
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_abort_rebase()
        .once()
        .withf(|repo_path| repo_path.ends_with("sess1"))
        .returning(|_| Box::pin(async { Ok(()) }));

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

    // Assert
    assert_eq!(sessions[0].status, "Review");
}

#[tokio::test]
/// Verifies a review request queued behind rebase cannot begin after the
/// raw Git command but before metadata persistence and finalization end.
async fn test_queued_review_request_waits_for_full_rebase_finalization() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    write_rebase_conflict_file(base_dir.path());
    let db = AppRepositories::in_memory().await.expect("db should open");
    seed_existing_session_rebase_metadata(&db, Some("parent-session")).await;
    db.operations()
        .insert_session_operation("op-rebase", "sess1", REBASE_OPERATION_KIND)
        .await
        .expect("failed to insert rebase operation");
    db.operations()
        .insert_session_operation(
            "op-review-request",
            "sess1",
            CREATE_REVIEW_REQUEST_OPERATION_KIND,
        )
        .await
        .expect("failed to insert review-request operation");

    let metadata_persistence_started = Arc::new(tokio::sync::Notify::new());
    let release_metadata_persistence = Arc::new(tokio::sync::Notify::new());
    let mut harness = rebase_assist_worker_harness(base_dir.path().to_path_buf(), db.clone(), {
        let metadata_persistence_started = Arc::clone(&metadata_persistence_started);
        let release_metadata_persistence = Arc::clone(&release_metadata_persistence);

        move |main_checkout_root| {
            blocking_stack_metadata_git_client(
                main_checkout_root,
                metadata_persistence_started,
                release_metadata_persistence,
            )
        }
    });
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    harness.context.app_event_tx = app_event_tx;
    let transcript = Arc::clone(&harness.context.transcript);
    let status = Arc::clone(&harness.status);
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    SessionWorkerService::spawn_session_worker(
        harness.context,
        auto_commit_one_shot_client(),
        Arc::new(Notify::new()),
        command_rx,
    );
    command_tx
        .send(ScheduledSessionCommand::immediate(SessionCommand::Rebase {
            base_branch: "main".to_string(),
            operation_id: "op-rebase".to_string(),
        }))
        .expect("failed to queue rebase");
    command_tx
        .send(ScheduledSessionCommand::queued(
            queued_review_request_command(base_dir.path().to_path_buf()),
            0,
        ))
        .expect("failed to queue review request");

    // Act
    tokio::time::timeout(
        Duration::from_secs(1),
        metadata_persistence_started.notified(),
    )
    .await
    .expect("rebase should reach metadata persistence");
    let events_before_release =
        std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    release_metadata_persistence.notify_one();
    let publish_started = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = app_event_rx.recv().await.expect("missing app event");
            if matches!(event, AppEvent::BranchPublishActionStarted { .. }) {
                break;
            }
        }
    })
    .await;

    // Assert
    assert!(
        events_before_release
            .iter()
            .all(|event| !matches!(event, AppEvent::BranchPublishActionStarted { .. }))
    );
    assert_eq!(*status.lock().expect("status lock"), Status::Review);
    assert!(transcript_text(&transcript).contains("[Sync] Successfully synced wt/sess1 onto main"));
    publish_started.expect("review request should start after rebase finalization");
    assert_eq!(
        db.sessions()
            .get_session_stack_base_commit_hash("sess1")
            .await
            .expect("failed to load stack-base hash")
            .as_deref(),
        Some("parent-tip")
    );
}
