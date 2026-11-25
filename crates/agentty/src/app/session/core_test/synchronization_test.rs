//! Session manager synchronization tests covering rebase, merge, and
//! main-branch sync queueing and validation.

use std::sync::{Arc, Mutex};

use ag_contracts::{MockAgentChannel, TurnResult};
use ag_git as git;
use ag_protocol::{AgentResponse, parse_agent_response_strict};
use ag_telemetry::KeyValue;
use mockall::predicate::eq;
use opentelemetry::trace::SpanId;
use tempfile::tempdir;
use tokio::sync::Notify;

use super::super::{SessionManager, session_folder};
use super::support::{
    add_manual_session, allow_detect_git_info, assert_sync_waits_without_canceling_turn,
    create_default_mock_git_client, create_mock_git_client_for_successful_noop_merges,
    create_passthrough_mock_fs_client, install_mock_git_client, new_test_app,
    new_test_app_with_git, new_test_app_with_git_and_db, refresh_with_session_table_unavailable,
    session_replay_text, session_status_or_done, test_loading_review, test_session_manager,
    wait_for_all_sessions_done, wait_for_first_merge_to_complete_before_second_starts,
    wait_for_output_contains, wait_for_output_contains_after_events,
    wait_for_second_merge_to_start, wait_for_status_with_retries,
};
use crate::app::test_support::{SyncSessionStartError, TestSessionRunFactory};
use crate::app::{App, AppEvent};
use crate::domain::agent::AgentModel;
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::TransientMessageSlot;
use crate::infra::db::AppRepositories;
use crate::infra::fs::FsClient;
use crate::presentation::app_mode::AppMode;
use crate::test_support::telemetry::{
    assert_span_attributes, capture_spans, finished_span, wait_for_finished_span,
};

/// Verifies sync requested during a running turn stays on the existing
/// worker, does not cancel that turn, and runs before later queued chat.
#[tokio::test]
async fn test_running_turn_finishes_before_queued_sync_and_later_chat() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), database).await;
    let release_first_turn = Arc::new(Notify::new());
    let release_first_turn_for_channel = Arc::clone(&release_first_turn);
    let turn_count = Arc::new(Mutex::new(0usize));
    let turn_count_for_channel = Arc::clone(&turn_count);
    let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .times(2)
        .returning(move |_, request, _| {
            let turn_index = {
                let mut turn_count = turn_count_for_channel
                    .lock()
                    .expect("turn count lock should not be poisoned");
                let turn_index = *turn_count;
                *turn_count += 1;

                turn_index
            };
            let release_first_turn = Arc::clone(&release_first_turn_for_channel);
            let turn_started_tx = turn_started_tx.clone();

            Box::pin(async move {
                turn_started_tx
                    .send(turn_index)
                    .expect("turn start receiver should remain available");
                if turn_index == 0 {
                    assert_eq!(request.prompt.text, "Initial running turn");
                    release_first_turn.notified().await;

                    return Ok(TurnResult {
                        assistant_message: AgentResponse::plain("Initial turn completed"),
                        context_reset: false,
                        harness_report: None,
                        provider_conversation_id: None,
                        usage: ag_contracts::TurnUsage::default(),
                    });
                }

                assert_eq!(request.prompt.text, "Queued after sync");

                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("Queued turn completed"),
                    context_reset: false,
                    harness_report: None,
                    provider_conversation_id: None,
                    usage: ag_contracts::TurnUsage::default(),
                })
            })
        });
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let channels = TestSessionRunFactory::install(&mut app.services);
    channels.register(&session_id, Arc::new(mock_channel));
    app.sessions
        .reply(&app.services, &session_id, "Initial running turn")
        .await;
    assert_eq!(turn_started_rx.recv().await, Some(0));
    app.sessions.sync_from_handles();
    refresh_with_session_table_unavailable(&mut app, &pool).await;

    // Act
    app.rebase_session(&session_id)
        .await
        .expect("running sync should queue on the active worker");
    app.enqueue_message(&session_id, "Queued after sync")
        .expect("later chat message should queue");

    // Assert
    assert_sync_waits_without_canceling_turn(&mut app, &session_id);

    // Act
    release_first_turn.notify_one();
    assert_eq!(turn_started_rx.recv().await, Some(1));
    wait_for_output_contains_after_events(&mut app, &session_id, "Queued turn completed", 300)
        .await;

    // Assert
    app.sessions.sync_from_handles();
    let transcript = session_replay_text(&app.sessions.sessions()[0]);
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .is_none()
    );
    let initial_answer_index = transcript
        .find("Initial turn completed")
        .expect("missing completed initial turn");
    let sync_completion_index = transcript
        .find("[Sync] Successfully synced")
        .expect("missing queued sync completion");
    let queued_prompt_index = transcript
        .find("Queued after sync")
        .expect("missing later queued prompt");
    assert!(initial_answer_index < sync_completion_index);
    assert!(sync_completion_index < queued_prompt_index);
}

#[tokio::test]
async fn test_cleanup_merged_session_worktree_removes_the_directory_git_leaves_behind() {
    crate::test_support::enable_tracing_log_fallback();
    for git_removes_directory in [true, false] {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let worktree_folder = dir.path().join("merged-worktree");
        let branch_name = "wt/cleanup123";
        std::fs::create_dir_all(&worktree_folder).expect("failed to create worktree folder");
        let repo_root = dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .returning(move |worktree_path| {
                Box::pin(async move {
                    if git_removes_directory {
                        let fs_client = create_passthrough_mock_fs_client();
                        let _ = fs_client.remove_dir_all(worktree_path).await;
                    }

                    Ok(())
                })
            });
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .withf(|_, branch| branch == "wt/cleanup123")
            .returning(|_, _| Box::pin(async { Ok(()) }));

        // Act
        let result = SessionManager::cleanup_merged_session_worktree(
            worktree_folder.clone(),
            Arc::new(create_passthrough_mock_fs_client()),
            Arc::new(mock_git_client),
            branch_name.to_string(),
            repo_root,
        )
        .await;

        // Assert
        assert!(result.is_ok(), "cleanup should succeed: {:?}", result.err());
        assert!(
            !worktree_folder.exists(),
            "worktree should be removed after cleanup"
        );
    }
}

#[test]
fn test_parse_merge_commit_message_response_with_protocol_message() {
    // Arrange
    let content = r#"{"answer":"Title\n\n- Detail","questions":[]}"#;

    // Act
    let parsed = parse_agent_response_strict(content)
        .ok()
        .map(|response| response.to_answer_display_text())
        .filter(|answer_text| !answer_text.trim().is_empty());

    // Assert
    assert!(parsed.is_some());
    assert_eq!(parsed.as_deref(), Some("Title\n\n- Detail"));
}

#[test]
fn test_parse_merge_commit_message_response_rejects_non_protocol_json() {
    // Arrange
    let content = r#"{"title":"Title","description":"- Detail"}"#;

    // Act
    let parsed = parse_agent_response_strict(content)
        .ok()
        .map(|response| response.to_answer_display_text())
        .filter(|answer_text| !answer_text.trim().is_empty());

    // Assert
    assert!(parsed.is_none());
}

#[tokio::test]
async fn test_rebase_session_no_git() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "manual01", "Test");

    // Act
    let result = app.rebase_session("manual01").await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("No git worktree")
    );
}

#[tokio::test]
async fn test_rebase_session_requires_review_status() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("must be in review")
    );
}

#[tokio::test]
async fn test_rebase_session_accepts_in_progress_status_before_worktree_validation() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "manual01", "Test");
    crate::test_support::set_session_status_for_test(&mut app, "manual01", Status::InProgress);

    // Act
    let result = app.rebase_session("manual01").await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("No git worktree")
    );
}

#[tokio::test]
async fn test_rebase_session_invalid_id() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;

    // Act
    let result = app.rebase_session("missing").await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("Session not found")
    );
}

/// Verifies session sync queues without waiting for an active branch
/// operation, then runs after that operation releases ownership.
#[tokio::test]
async fn test_rebase_session_queues_while_branch_operation_is_busy() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
    mock_git_client
        .expect_find_git_repo_root()
        .times(0..)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_create_worktree()
        .times(1)
        .returning(|_, worktree_path, _, _| {
            Box::pin(async move {
                let fs_client = create_passthrough_mock_fs_client();
                fs_client
                    .create_dir_all(worktree_path)
                    .await
                    .map_err(|error| {
                        git::GitError::OutputParse(format!(
                            "Failed to create mock worktree: {error}"
                        ))
                    })?;

                Ok(())
            })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    install_mock_git_client(&mut app, mock_git_client);

    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.sessions.sessions_mut()[0].status = Status::Review;
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str())
        && let Ok(mut session_status) = handles.status.lock()
    {
        *session_status = Status::Review;
    }
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;

    // Act
    let start_result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.rebase_session(&session_id),
    )
    .await;

    // Assert
    let result = start_result.expect("queueing sync should not wait for the branch operation");
    assert!(result.is_ok(), "sync should queue: {:?}", result.err());
    assert!(branch_operation_lock.try_lock().is_err());

    // Act, Assert
    drop(existing_operation_guard);
    wait_for_output_contains(&mut app, &session_id, "[Sync] Successfully synced", 200).await;
}

#[tokio::test]
async fn test_rebase_session_keeps_focused_review_without_conflicts() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let db = app.services.db().clone();
    db.sessions()
        .update_session_focused_review(
            &session_id,
            Some(crate::domain::review::FocusedReviewStatus::Partial),
            Some("111".to_string()),
            Some("Pre-rebase finding.\n\nPartial review: interrupted".to_string()),
        )
        .await
        .expect("failed to seed persisted focused review");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: None,
    };
    app.set_review_ready_output(
        &session_id,
        111,
        "Pre-rebase finding.\n\nPartial review: interrupted".to_string(),
    );

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
    assert_eq!(
        app.review_view_state(&session_id).1,
        Some("Pre-rebase finding.\n\nPartial review: interrupted")
    );
    wait_for_output_contains(&mut app, &session_id, "[Sync] Successfully synced", 200).await;
    assert_eq!(
        app.review_view_state(&session_id).1,
        Some("Pre-rebase finding.\n\nPartial review: interrupted")
    );
    assert!(matches!(app.mode, AppMode::View { .. }));
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_git_client(Arc::new(create_default_mock_git_client(
            dir.path().to_path_buf(),
        )));
    let restarted_app = App::new_with_clients(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Some("main".to_string()),
        db,
        clients,
    )
    .await
    .expect("failed to build app after recovery");

    assert_eq!(
        restarted_app.review_view_state(&session_id).1,
        Some("Pre-rebase finding.\n\nPartial review: interrupted")
    );
}

/// Verifies unavailable persistence rejects admission without clearing the
/// displayed review.
#[tokio::test]
async fn test_rebase_session_admission_failure_does_not_clear_review() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    db.sessions()
        .update_session_focused_review(
            &session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("111".to_string()),
            Some("old persisted focused review".to_string()),
        )
        .await
        .expect("failed to seed persisted focused review");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::AgentReview);
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: None,
    };
    app.review_cache
        .insert(session_id.clone().into(), test_loading_review(777));
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client.expect_rebase_start().times(0);
    install_mock_git_client(&mut app, mock_git_client);
    pool.close().await;

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    assert!(result.is_err(), "persistence failure should reject sync");
    assert!(app.review_cache.contains_key(session_id.as_str()));
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(
        session_status_or_done(&app, &session_id),
        Status::AgentReview
    );
}

#[tokio::test]
/// Verifies rebase commits pending worktree changes before starting.
async fn test_rebase_session_auto_commits_uncommitted_changes() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
    mock_git_client
        .expect_find_git_repo_root()
        .times(0..)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_create_worktree()
        .times(1)
        .returning(|_, worktree_path, _, _| {
            Box::pin(async move {
                let fs_client = create_passthrough_mock_fs_client();
                fs_client
                    .create_dir_all(worktree_path)
                    .await
                    .map_err(|error| {
                        git::GitError::OutputParse(format!(
                            "Failed to create mock worktree: {error}"
                        ))
                    })?;

                Ok(())
            })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_head_commit_message()
        .times(1)
        .returning(|_| Box::pin(async { Ok(Some("Existing session commit".to_string())) }));
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .returning(|_| Box::pin(async { Ok("cafe123".to_string()) }));
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    install_mock_git_client(&mut app, mock_git_client);

    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let session_folder = app.sessions.sessions()[0].folder.clone();
    app.sessions.sessions_mut()[0].status = Status::Review;
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str())
        && let Ok(mut session_status) = handles.status.lock()
    {
        *session_status = Status::Review;
    }

    // Create an uncommitted change in the session worktree
    std::fs::write(session_folder.join("dirty.txt"), "uncommitted content")
        .expect("failed to write dirty file");

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "rebase should succeed: {:?}", result.err());
    wait_for_output_contains(&mut app, &session_id, "[Sync] Successfully synced", 200).await;
    // The commit call itself is verified by mock expectations; output can
    // be refreshed from persisted state before the commit line is observed
    // in this integration test under full-suite runtime contention.
    app.refresh_sessions_now().await;
}

/// Verifies stale `InProgress` state cannot create a new worker and start
/// session sync without an active turn owner.
#[tokio::test]
async fn test_rebase_in_progress_requires_existing_session_worker() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

    // Act
    let result = app.rebase_session(&session_id).await;

    // Assert
    let error = result.expect_err("sync should reject stale in-progress state");
    assert!(
        error
            .to_string()
            .contains("active session worker is unavailable")
    );
    let unfinished_operations = app
        .services
        .db()
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("failed to load session operations");
    assert!(
        unfinished_operations
            .iter()
            .all(|operation| operation.kind != "rebase")
    );
}

#[test]
fn test_finish_published_branch_sync_reports_unloaded_handle_update() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.start_published_branch_sync("session-id", "sync-id".to_string());
    session_manager.state_mut().replace_sessions(Vec::new());

    // Act
    let finished = session_manager.finish_published_branch_sync(
        "session-id",
        "sync-id",
        Some("[Branch Push] Auto-pushed published branch after completed turn."),
    );

    // Assert
    assert!(finished);
    let transcript = session_manager
        .state()
        .handle("session-id")
        .expect("session handles should remain loaded")
        .transcript
        .lock()
        .expect("session transcript lock should succeed");
    assert_eq!(
        transcript
            .messages()
            .last()
            .map(|message| (message.kind, message.content.as_str())),
        Some((
            SessionMessageKind::WorkflowNotice,
            "[Branch Push] Auto-pushed published branch after completed turn.",
        ))
    );
}

#[tokio::test]
async fn test_merge_session_no_git() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "manual01", "Test");

    // Act
    let result = app.merge_session("manual01").await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("No git worktree")
    );
}

#[tokio::test]
async fn test_merge_session_invalid_id() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;

    // Act
    let result = app.merge_session("missing").await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("Session not found")
    );
}

#[tokio::test]
async fn test_merge_session_removes_worktree_and_branch_after_success() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create merge session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let session_folder = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing created session")
        .folder
        .clone();
    let mock_git = create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    let result = app.merge_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "merge should enqueue successfully");
    wait_for_status_with_retries(&mut app, &session_id, Status::Done, 200, false).await;

    app.sessions.sync_from_handles();
    let merged_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing merged session");
    assert!(!session_replay_text(merged_session).contains("[Merge Error]"));
    assert!(!session_folder.exists(), "worktree should be removed");
}

/// Verifies a merge's phases nest beneath one content-free `session.merge`
/// root instead of starting unparented traces.
#[tokio::test]
async fn test_merge_session_traces_rebase_squash_and_cleanup_under_one_root() {
    // Arrange
    let (_provider_guard, exporter) = capture_spans().await;
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create merge session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let mock_git = create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    app.merge_session(&session_id)
        .await
        .expect("merge should enqueue successfully");
    let root = wait_for_finished_span(&exporter, "session.merge", SpanId::INVALID).await;

    // Assert
    assert_span_attributes(
        &root,
        "completed",
        &[KeyValue::new("agentty.session.id", session_id)],
    );
    let root_id = root.span_context.span_id();
    assert_span_attributes(
        &finished_span(&exporter, "rebase.step", root_id),
        "completed",
        &[KeyValue::new("agentty.git.rebase.result", "clean")],
    );
    assert_span_attributes(
        &finished_span(&exporter, "merge.squash", root_id),
        "completed",
        &[],
    );
    assert_span_attributes(
        &finished_span(&exporter, "workspace.cleanup", root_id),
        "completed",
        &[],
    );
}

#[tokio::test]
async fn test_merge_session_marks_done_when_changes_are_already_in_base() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create merge session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let mock_git = create_mock_git_client_for_successful_noop_merges(1, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    let result = app.merge_session(&session_id).await;

    // Assert
    assert!(result.is_ok(), "merge should enqueue successfully");
    wait_for_status_with_retries(&mut app, &session_id, Status::Done, 200, false).await;

    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session after merge");
    assert!(!session_replay_text(session).contains("[Merge Error]"));
}

#[tokio::test]
async fn test_merge_session_queue_processes_sessions_in_fifo_order() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let first_session_id = app
        .create_session()
        .await
        .expect("failed to create first queue session");
    let second_session_id = app
        .create_session()
        .await
        .expect("failed to create second queue session");
    crate::test_support::set_session_status_for_test(&mut app, &first_session_id, Status::Review);
    crate::test_support::set_session_status_for_test(&mut app, &second_session_id, Status::Review);
    let mock_git = create_mock_git_client_for_successful_noop_merges(2, dir.path().to_path_buf());
    app.sessions.git_client = Arc::new(mock_git);

    // Act
    let first_merge_result = app.merge_session(&first_session_id).await;
    let second_merge_result = app.merge_session(&second_session_id).await;

    // Assert
    assert!(
        first_merge_result.is_ok(),
        "first merge request should succeed: {:?}",
        first_merge_result.err()
    );
    assert!(
        second_merge_result.is_ok(),
        "second merge request should enqueue: {:?}",
        second_merge_result.err()
    );

    wait_for_first_merge_to_complete_before_second_starts(
        &mut app,
        &first_session_id,
        &second_session_id,
    )
    .await;
    wait_for_second_merge_to_start(&mut app, &second_session_id).await;

    assert!(
        session_status_or_done(&app, &first_session_id) == Status::Done,
        "first merge should be complete before second starts"
    );

    wait_for_all_sessions_done(&mut app, &first_session_id, &second_session_id).await;

    app.sessions.sync_from_handles();
    let first_status = session_status_or_done(&app, &first_session_id);
    let second_status = session_status_or_done(&app, &second_session_id);
    assert_eq!(first_status, Status::Done);
    assert_eq!(second_status, Status::Done);
}

/// Verifies a project switch neither advances the merge queue while its merge
/// runs nor keeps the next queued merge from starting in its own project's
/// checkout once that merge finishes.
#[tokio::test]
async fn test_merge_queue_waits_across_project_switch_and_merges_in_session_project() {
    // Arrange
    let first_project_dir = tempdir().expect("failed to create first project dir");
    let second_project_dir = tempdir().expect("failed to create second project dir");
    std::fs::create_dir_all(second_project_dir.path().join(".git"))
        .expect("failed to mark second repository");
    let mut app = new_test_app_with_git(first_project_dir.path()).await;
    let first_project_id = app.active_project_id();
    let first_project_path = app.projects.working_dir().to_path_buf();
    let second_project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let merge_statuses = [
        ("merging-session", Status::Merging),
        ("queued-session", Status::Queued),
    ];
    for (session_id, _) in merge_statuses {
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id,
                "gemini-3.8-flash",
                "main",
                &Status::Review.to_string(),
                first_project_id,
            )
            .await
            .expect("failed to insert session");
        std::fs::create_dir_all(session_folder(app.services.base_path(), session_id))
            .expect("failed to create session folder");
    }
    app.refresh_sessions_now().await;
    for (session_id, status) in merge_statuses {
        crate::test_support::set_session_status_for_test(&mut app, session_id, status);
    }
    app.merge_queue.set_active("merging-session".into());
    app.merge_queue.enqueue("queued-session".into());
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch to the second project");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .with(eq(first_project_path.clone()))
        .times(1)
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_is_worktree_clean()
        .with(eq(first_project_path))
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    app.sessions.git_client = Arc::new(mock_git_client);

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "merging-session".into(),
        version: 1,
    })
    .await;
    let waits_for_running_merge = app.merge_queue.active_session_id() == Some("merging-session")
        && app.merge_queue.is_queued_or_active("queued-session");
    crate::test_support::set_session_status_for_test(&mut app, "merging-session", Status::Done);
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "merging-session".into(),
        version: 2,
    })
    .await;

    // Assert
    assert!(waits_for_running_merge);
    assert!(!app.merge_queue.has_work());
    let queued_handles = app
        .sessions
        .session_handles_or_err("queued-session")
        .expect("queued session handles should outlive the project switch");
    assert_eq!(
        *queued_handles.status.lock().expect("status lock poisoned"),
        Status::Review
    );
    assert!(
        queued_handles
            .transcript
            .lock()
            .expect("transcript lock poisoned")
            .messages()
            .iter()
            .any(|message| message.content.contains("`main` has uncommitted changes"))
    );
}

#[tokio::test]
async fn test_sync_main_uses_active_project_branch_from_context() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    app.projects.update_active_project_context(
        app.active_project_id(),
        app.projects.project_name().to_string(),
        Some("develop".to_string()),
        None,
        dir.path().to_path_buf(),
    );
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));

    // Act
    let result = SessionManager::sync_main_for_project(
        app.projects.git_branch().map(str::to_string),
        app.projects.working_dir().to_path_buf(),
        None,
        Arc::new(mock_git_client),
        AgentModel::Gemini38Flash,
        Arc::new(ag_worker::MockRunClient::new()),
    )
    .await;

    // Assert
    assert_eq!(
        result,
        Err(SyncSessionStartError::MainHasUncommittedChanges {
            default_branch: "develop".to_string(),
        })
    );
}

#[tokio::test]
async fn test_sync_main_requires_clean_selected_project_branch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let app = new_test_app_with_git(dir.path()).await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));

    // Act
    let result = SessionManager::sync_main_for_project(
        app.projects.git_branch().map(str::to_string),
        app.projects.working_dir().to_path_buf(),
        None,
        Arc::new(mock_git_client),
        AgentModel::Gemini38Flash,
        Arc::new(ag_worker::MockRunClient::new()),
    )
    .await;

    // Assert
    assert_eq!(
        result,
        Err(SyncSessionStartError::MainHasUncommittedChanges {
            default_branch: "main".to_string(),
        })
    );
}

#[tokio::test]
async fn test_sync_main_returns_error_without_upstream_remote() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let app = new_test_app_with_git(dir.path()).await;

    // Act
    let result = SessionManager::sync_main_for_project(
        app.projects.git_branch().map(str::to_string),
        app.projects.working_dir().to_path_buf(),
        None,
        app.services.git_client(),
        AgentModel::Gemini38Flash,
        Arc::new(ag_worker::MockRunClient::new()),
    )
    .await;

    // Assert
    assert!(matches!(result, Err(SyncSessionStartError::Other(_))));
}

#[tokio::test]
/// Verifies `sync_main_for_project` pushes local commits to `origin`.
async fn test_sync_main_pushes_local_commits_to_remote() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    let mut ahead_behind_calls = 0_u8;
    mock_git_client
        .expect_get_ahead_behind()
        .times(2)
        .returning(move |_| {
            ahead_behind_calls = ahead_behind_calls.saturating_add(1);
            let value = if ahead_behind_calls == 1 {
                (1, 2)
            } else {
                (0, 0)
            };

            Box::pin(async move { Ok(value) })
        });
    mock_git_client
        .expect_list_upstream_commit_titles()
        .times(1)
        .returning(|_| Box::pin(async { Ok(vec!["remote fix".to_string()]) }));
    mock_git_client
        .expect_pull_rebase()
        .times(1)
        .returning(|_| Box::pin(async { Ok(git::PullRebaseResult::Completed) }));
    mock_git_client
        .expect_list_local_commit_titles()
        .times(1)
        .returning(|_| Box::pin(async { Ok(vec!["local work".to_string()]) }));
    mock_git_client
        .expect_push_current_branch()
        .times(1)
        .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));

    // Act
    let result = SessionManager::sync_main_for_project(
        Some("main".to_string()),
        dir.path().to_path_buf(),
        None,
        Arc::new(mock_git_client),
        AgentModel::Gemini38Flash,
        Arc::new(ag_worker::MockRunClient::new()),
    )
    .await;

    // Assert
    let outcome = result.expect("sync should succeed");
    assert_eq!(outcome.pulled_commits, Some(2));
    assert_eq!(outcome.pushed_commits, Some(0));
    assert_eq!(outcome.pulled_commit_titles, vec!["remote fix".to_string()]);
    assert_eq!(outcome.pushed_commit_titles, vec!["local work".to_string()]);
    assert_eq!(
        outcome.resolved_conflict_files,
        [] as [std::string::String; 0]
    );
}
