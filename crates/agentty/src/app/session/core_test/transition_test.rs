use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent::{
    AgentRequestKind, AppServerClient, AppServerTurnResponse, MockAgentBackend, MockAgentChannel,
    MockAppServerClient, MockOneShotClient, TurnResult,
};
use ag_git as git;
use ag_protocol::AgentResponse;
use tempfile::tempdir;

use super::super::{
    SessionDefaults, SessionManager, remote_branch_name_from_upstream_ref, session_folder,
};
use super::support::{
    add_manual_session_with_status, allow_detect_git_info, create_and_start_session,
    create_passthrough_mock_fs_client, expect_pre_commit_hook_ready, install_mock_git_client,
    new_test_app, new_test_app_with_db, new_test_app_with_db_and_app_server, new_test_app_with_git,
    new_test_app_with_git_and_db, session_replay_text, wait_for_output_contains,
    wait_for_path_absent, wait_for_status,
};
use crate::app::session::workflow::task::SessionTaskService;
use crate::app::{SessionState, Tab};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::selection::SelectionState;
use crate::domain::session::{SESSION_DATA_DIR, Status};
use crate::domain::session_message::SessionTranscript;
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs::FsClient;

#[tokio::test]
async fn test_spawn_session_task_auto_commits_changes() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;
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
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
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
        .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
    expect_pre_commit_hook_ready(&mut mock_git_client);
    mock_git_client
        .expect_diff()
        .times(3)
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Create a session that writes a file so commit_all has something to commit
    let mut mock = MockAgentBackend::new();
    mock.expect_build_command().returning(|request| {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(
                "echo auto-content > auto-committed.txt; printf '{\"answer\":\"Auto commit \
                 done\",\"questions\":[]}'",
            )
            .current_dir(request.folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(cmd)
    });
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.sessions
        .reply_with_backend(
            &app.services,
            &session_id,
            "AutoCommit",
            Arc::new(mock),
            AgentModel::ClaudeSonnet5,
        )
        .await;

    // Act — wait for agent to finish and auto-commit
    wait_for_status(&mut app, &session_id, Status::Review).await;
    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();

    // Assert — commit completion details are transient workflow notice
    // state, not persisted transcript output.
    let session = &app.sessions.sessions()[0];
    let output = session_replay_text(session);
    let workflow_notice = session
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
        .map(|message| message.body.text());
    assert!(
        !output.contains("[Commit] committed with hash"),
        "commit completion should not be persisted, got: {output}"
    );
    assert_eq!(
        workflow_notice,
        Some("[Commit] committed with hash `abc1234`")
    );
}

#[tokio::test]
async fn test_spawn_session_task_skips_commit_when_nothing_to_commit() {
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
                    .create_dir_all(worktree_path.clone())
                    .await
                    .map_err(|error| {
                        git::GitError::OutputParse(format!(
                            "Failed to create mock worktree: {error}"
                        ))
                    })?;
                fs_client
                    .create_dir_all(worktree_path.join(SESSION_DATA_DIR))
                    .await
                    .map_err(|error| {
                        git::GitError::OutputParse(format!(
                            "Failed to create mock worktree data dir: {error}"
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
        .expect_diff()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Agent that produces no file changes
    let mut mock = MockAgentBackend::new();
    mock.expect_build_command().returning(|request| {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf '{\"answer\":\"no-changes\",\"questions\":[]}'")
            .current_dir(request.folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(cmd)
    });
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.sessions
        .reply_with_backend(
            &app.services,
            &session_id,
            "NoChanges",
            Arc::new(mock),
            AgentModel::ClaudeOpus5,
        )
        .await;

    // Act — wait for agent to finish
    wait_for_status(&mut app, &session_id, Status::Review).await;
    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();

    // Assert — no-op commit output is visible as transient workflow state.
    let session = &app.sessions.sessions()[0];
    let output = session_replay_text(session);
    let workflow_notice = session
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
        .map(|message| message.body.text());
    assert!(
        !output.contains("[Commit] No changes to commit."),
        "no-op commit output should not be persisted when nothing to commit"
    );
    assert_eq!(workflow_notice, Some("[Commit] No changes to commit."));
    assert!(
        !output.contains("[Commit Error]"),
        "should not contain commit error when nothing to commit"
    );
}

#[tokio::test]
/// Ensures canceling a review session persists `Canceled` status and
/// defers removal of its dedicated worktree checkout.
async fn test_cancel_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let session_folder = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session")
        .folder
        .clone();
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);

    // Act
    app.sessions
        .cancel_session(&app.services, &session_id)
        .await
        .expect("failed to cancel session");

    // Assert
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session");
    assert_eq!(session.status, Status::Canceled);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(db_session.status, "Canceled");
    wait_for_path_absent(&session_folder).await;
}

#[tokio::test]
/// Ensures canceling a parent session stops and cancels every nonterminal
/// stacked descendant.
async fn test_cancel_session_cascades_to_stacked_descendants() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    crate::test_support::set_session_status_for_test(&mut app, &child_session_id, Status::Review);
    let grandchild_session_id = app
        .create_stacked_draft_session(&child_session_id)
        .await
        .expect("failed to create nested stacked draft session");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &grandchild_session_id,
        Status::Queued,
    );
    app.services
        .db()
        .operations()
        .insert_session_operation("grandchild-operation", &grandchild_session_id, "rebase")
        .await
        .expect("failed to insert grandchild operation");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);

    // Act
    app.sessions
        .cancel_session(&app.services, &parent_session_id)
        .await
        .expect("failed to cancel parent session");

    // Assert
    app.sessions.sync_from_handles();
    let parent_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("missing parent session");
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    let grandchild_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == grandchild_session_id)
        .expect("missing grandchild session");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Canceled);
    assert_eq!(grandchild_session.status, Status::Canceled);
    let grandchild_handles = app
        .sessions
        .session_handles_or_err(&grandchild_session_id)
        .expect("missing grandchild session handles");
    assert!(
        grandchild_handles
            .cancel_token
            .lock()
            .expect("grandchild cancel token lock")
            .is_cancelled()
    );
    assert!(
        app.services
            .db()
            .operations()
            .is_cancel_requested_for_operation("grandchild-operation")
            .await
            .expect("failed to load grandchild operation cancellation")
    );

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_parent_session = db_sessions
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("missing persisted parent session");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    let db_grandchild_session = db_sessions
        .iter()
        .find(|session| session.id == grandchild_session_id)
        .expect("missing persisted grandchild session");
    assert_eq!(db_parent_session.status, "Canceled");
    assert_eq!(db_child_session.status, "Canceled");
    assert_eq!(db_grandchild_session.status, "Canceled");
}

#[tokio::test]
/// Ensures a stack cascade preserves a descendant that is already terminal.
async fn test_cancel_session_preserves_terminal_stacked_descendant() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    crate::test_support::set_session_status_for_test(&mut app, &child_session_id, Status::Done);
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&child_session_id, "Done", 0)
        .await
        .expect("failed to persist terminal child status");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);

    // Act
    app.sessions
        .cancel_session(&app.services, &parent_session_id)
        .await
        .expect("failed to cancel parent session");

    // Assert
    app.sessions.sync_from_handles();
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing terminal child session");
    assert_eq!(child_session.status, Status::Done);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted terminal child session");
    assert_eq!(db_child_session.status, "Done");
}

#[tokio::test]
/// Ensures parent cancellation reports a descendant cascade failure instead
/// of returning success after the parent status was already persisted.
async fn test_cancel_session_reports_stacked_descendant_failure() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    app.sessions
        .session_handles_mut()
        .remove(child_session_id.as_str());

    // Act
    let result = app
        .sessions
        .cancel_session(&app.services, &parent_session_id)
        .await;

    // Assert
    let error = result.expect_err("descendant cancellation failure should be reported");
    assert!(error.to_string().contains(child_session_id.as_str()));
    app.sessions.sync_from_handles();
    let parent_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("missing parent session");
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Draft);
}

#[tokio::test]
/// Ensures canceling a Codex review session shuts down its app-server
/// runtime.
async fn test_cancel_session_triggers_app_server_shutdown() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut mock_app_server = MockAppServerClient::new();
    mock_app_server
        .expect_run_turn()
        .times(1)
        .returning(|_, _| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    mock_app_server
        .expect_shutdown_session()
        .times(1)
        .returning(move |session_id| {
            let shutdown_tx = shutdown_tx.clone();
            Box::pin(async move {
                let _ = shutdown_tx.send(session_id);
            })
        });
    let app_server_client: Arc<dyn AppServerClient> = Arc::new(mock_app_server);
    let mut app = new_test_app_with_db_and_app_server(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Some("main".to_string()),
        db,
        app_server_client,
    )
    .await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set app-server model");

    // Act
    app.sessions
        .reply(&app.services, &session_id, "Start")
        .await;
    wait_for_status(&mut app, &session_id, Status::Review).await;
    app.cancel_session(&session_id)
        .await
        .expect("failed to cancel session");
    app.process_pending_app_events().await;
    wait_for_status(&mut app, &session_id, Status::Canceled).await;
    let shutdown_session_id =
        tokio::time::timeout(std::time::Duration::from_secs(1), shutdown_rx.recv())
            .await
            .expect("timed out waiting for app-server shutdown")
            .expect("missing shutdown session id");

    // Assert
    assert_eq!(shutdown_session_id, session_id);
}

#[tokio::test]
async fn test_cancel_session_invalid_id() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let app = new_test_app(dir.path().to_path_buf()).await;

    // Act
    let result = app.sessions.cancel_session(&app.services, "missing").await;

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
/// Verifies end-to-end session execution for start and resume turns using
/// a single `MockAgentChannel`. The first turn must use
/// `AgentRequestKind::SessionStart` and produce output without
/// `--resume`; the second must use `AgentRequestKind::SessionResume` and
/// produce output with `--resume latest`.
async fn test_spawn_integration() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

    // One channel handles both turns; a counter distinguishes them so the
    // correct final response text is returned and mode assertions are made
    // per turn.
    let turn_count = Arc::new(Mutex::new(0usize));
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut mock_channel = MockAgentChannel::new();
    let turn_count_capture = Arc::clone(&turn_count);
    let done_capture = done_tx.clone();
    mock_channel
        .expect_run_turn()
        .returning(move |_, req, _event_tx| {
            let turn_index = {
                let mut count = turn_count_capture.lock().expect("lock poisoned");
                let current = *count;
                *count += 1;
                current
            };
            let delta_text = if turn_index == 0 {
                assert!(
                    matches!(req.request_kind, AgentRequestKind::SessionStart),
                    "expected AgentRequestKind::SessionStart on first turn"
                );
                format!("--prompt {}\n", req.prompt)
            } else {
                assert!(
                    matches!(req.request_kind, AgentRequestKind::SessionResume),
                    "expected AgentRequestKind::SessionResume on second turn"
                );
                format!("--prompt {} --resume latest\n", req.prompt)
            };
            let done = done_capture.clone();
            Box::pin(async move {
                let _ = done.send(());
                Ok(TurnResult {
                    assistant_message: AgentResponse::plain(&delta_text),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    // Act — create and start session (start command)
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.sessions
        .worker_service
        .test_agent_channels
        .insert(session_id.clone().into(), Arc::new(mock_channel));
    app.sessions
        .reply(&app.services, &session_id, "SpawnInit")
        .await;
    done_rx.recv().await.expect("first turn completion signal");
    wait_for_status(&mut app, &session_id, Status::Review).await;
    wait_for_output_contains(&mut app, &session_id, "SpawnInit", 200).await;

    // Assert
    {
        app.sessions.sync_from_handles();
        let session = &app.sessions.sessions()[0];
        let output = session_replay_text(session);
        assert!(output.contains("--prompt"));
        assert!(output.contains("SpawnInit"));
        assert!(!output.contains("--resume"));
        assert_eq!(session.status, Status::Review);
    }

    // Act — reply (resume command)
    let session_id = app.sessions.sessions()[0].id.clone();
    app.sessions
        .reply(&app.services, &session_id, "SpawnReply")
        .await;
    done_rx.recv().await.expect("second turn completion signal");
    wait_for_output_contains(&mut app, &session_id, "--resume", 200).await;
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Assert
    {
        app.sessions.sync_from_handles();
        let session = &app.sessions.sessions()[0];
        let output = session_replay_text(session);
        assert!(output.contains("SpawnReply"));
        assert!(output.contains("--resume"));
        assert!(output.contains("latest"));
        assert_eq!(session.status, Status::Review);
    }
}

#[tokio::test]
async fn test_clear_title_generation_task_if_matches_ignores_stale_generation() {
    // Arrange
    let state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );
    let mut session_manager = SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    );
    let session_id = "session-id".to_string();
    let task = tokio::spawn(async {});
    session_manager.replace_title_generation_task(&session_id, 2, task);

    // Act
    session_manager.clear_title_generation_task_if_matches(&session_id, 1);

    // Assert
    assert_eq!(
        session_manager.workflow_state.title_generation_tasks.len(),
        1
    );
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .contains_key(session_id.as_str())
    );
}

#[tokio::test]
async fn test_navigation_recovery() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "A").await;

    // Act & Assert — next recovers from None
    app.sessions.select_session_index(None);
    app.next();
    assert_eq!(app.sessions.selected_session_index(), Some(0));

    // Act & Assert — previous recovers from None
    app.sessions.select_session_index(None);
    app.previous();
    assert_eq!(app.sessions.selected_session_index(), Some(0));
}

#[tokio::test]
async fn test_navigation_follows_grouped_order_and_skips_group_headers() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session_with_status(&mut app, dir.path(), "archive-1", "Archive 1", Status::Done);
    add_manual_session_with_status(&mut app, dir.path(), "active-1", "Active 1", Status::Review);
    add_manual_session_with_status(&mut app, dir.path(), "queued-1", "Queued 1", Status::Queued);
    add_manual_session_with_status(
        &mut app,
        dir.path(),
        "archive-2",
        "Archive 2",
        Status::Canceled,
    );
    add_manual_session_with_status(&mut app, dir.path(), "merge-1", "Merge 1", Status::Merging);
    add_manual_session_with_status(&mut app, dir.path(), "active-2", "Active 2", Status::Draft);
    app.sessions.select_session_index(Some(3));

    // Act & Assert
    app.next();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("queued-1")
    );

    app.next();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("merge-1")
    );

    app.next();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("active-1")
    );

    app.next();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("active-2")
    );

    app.next();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("archive-1")
    );

    app.next();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("archive-2")
    );

    app.previous();
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some("archive-1")
    );
}

#[tokio::test]
/// Ensures canceling a running session requests operation cancellation,
/// signals the active turn token, clears queued prompts, and persists the
/// terminal `Canceled` status.
async fn test_cancel_running_session_stops_turn_and_cancels_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    app.services
        .db()
        .operations()
        .insert_session_operation("operation-id", &session_id, "reply")
        .await
        .expect("failed to insert operation");
    app.sessions
        .enqueue_message(&app.services, &session_id, "queued reply")
        .expect("failed to enqueue message");

    // Act
    app.sessions
        .cancel_session(&app.services, &session_id)
        .await
        .expect("failed to cancel session");

    // Assert
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session");
    assert_eq!(session.status, Status::Canceled);
    let handles = app
        .sessions
        .session_handles_or_err(&session_id)
        .expect("missing session handles");
    assert!(
        handles
            .cancel_token
            .lock()
            .expect("cancel token lock")
            .is_cancelled()
    );
    assert!(
        handles
            .queued_messages
            .lock()
            .expect("queue lock")
            .is_empty()
    );
    assert!(
        app.services
            .db()
            .operations()
            .is_cancel_requested_for_operation("operation-id")
            .await
            .expect("failed to load operation cancel status")
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(db_session.status, "Canceled");
}

#[tokio::test]
async fn test_navigation() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "A").await;
    create_and_start_session(&mut app, "B").await;

    // Act & Assert (Next)
    app.sessions.select_session_index(Some(0));
    app.next();
    assert_eq!(app.sessions.selected_session_index(), Some(1));
    app.next();
    assert_eq!(app.sessions.selected_session_index(), Some(0)); // Loop back

    // Act & Assert (Previous)
    app.previous();
    assert_eq!(app.sessions.selected_session_index(), Some(1)); // Loop back
    app.previous();
    assert_eq!(app.sessions.selected_session_index(), Some(0));
}

#[tokio::test]
async fn test_selected_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Test").await;

    // Act & Assert
    assert!(app.selected_session().is_some());

    app.sessions.select_session_index(None);
    assert!(app.selected_session().is_none());
}

// -- remote_branch_name_from_upstream_ref tests --------------------------

#[test]
fn test_remote_branch_name_strips_remote_prefix() {
    // Arrange / Act
    let branch = remote_branch_name_from_upstream_ref("origin/wt/abc12345");

    // Assert
    assert_eq!(branch, "wt/abc12345");
}

#[tokio::test]
async fn test_working_dir_getter() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let app = new_test_app(dir.path().to_path_buf()).await;

    // Act
    let working_dir = app.working_dir();

    // Assert
    assert_eq!(working_dir, Path::new("/tmp/test"));
}

// --- session_folder / session_branch ---

#[test]
fn test_session_folder_uses_first_8_chars() {
    // Arrange
    let base = Path::new("/home/user/.agentty/wt");
    let session_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

    // Act
    let folder = session_folder(base, session_id);

    // Assert
    assert_eq!(folder, PathBuf::from("/home/user/.agentty/wt/a1b2c3d4"));
}

#[tokio::test]
async fn test_commit_changes_reuses_existing_session_commit_message_in_tests() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = mockall::Sequence::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_head_commit_message()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(Some("Refine session work".to_string())) }));
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .withf(|_, base_branch, commit_message, strategy| {
            base_branch == "main"
                && commit_message == "Refine session work"
                && *strategy == git::SingleCommitMessageStrategy::Replace
        })
        .in_sequence(&mut sequence)
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok("def5678".to_string()) }));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(|request| {
            assert!(request.prompt.contains("Refine session work"));

            Ok(ag_agent::OneShotSubmission {
                response: AgentResponse::plain("Refine session work"),
                stats: ag_agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: ag_agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    // Act
    let outcome = SessionTaskService::commit_session_changes(
        &mock_git_client,
        &session_folder,
        "main",
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
            ReasoningLevel::Low,
            SpeedMode::Normal,
        ),
        &one_shot_client,
        false,
        &Mutex::new(SessionTranscript::default()),
    )
    .await
    .expect("failed to commit existing session message");

    // Assert
    assert_eq!(outcome.commit_hash, "def5678");
    assert_eq!(outcome.commit_message, "Refine session work");
}

#[tokio::test]
async fn test_new_app_empty() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");

    // Act
    let app = new_test_app(dir.path().to_path_buf()).await;

    // Assert
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.sessions.selected_session_index(), None);
}

#[tokio::test]
async fn test_git_branch_getter_with_branch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let working_dir = PathBuf::from("/tmp/test");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let app = new_test_app_with_db(
        dir.path().to_path_buf(),
        working_dir,
        Some("main".to_string()),
        db,
    )
    .await;

    // Act
    let branch = app.git_branch();

    // Assert
    assert_eq!(branch, Some("main"));
}

#[tokio::test]
async fn test_git_branch_getter_without_branch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let app = new_test_app(dir.path().to_path_buf()).await;

    // Act
    let branch = app.git_branch();

    // Assert
    assert_eq!(branch, None);
}

#[tokio::test]
async fn test_replace_title_generation_task_aborts_superseded_task() {
    // Arrange
    let state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );
    let mut session_manager = SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    );
    let session_id = "session-id".to_string();
    let first_task_aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first_task_flag = Arc::clone(&first_task_aborted);
    let first_task = tokio::spawn(async move {
        struct AbortFlagGuard(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for AbortFlagGuard {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let _abort_flag_guard = AbortFlagGuard(first_task_flag);
        std::future::pending::<()>().await;
    });
    let second_task = tokio::spawn(async {});

    // Act
    session_manager.replace_title_generation_task(&session_id, 1, first_task);
    tokio::task::yield_now().await;
    session_manager.replace_title_generation_task(&session_id, 2, second_task);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !first_task_aborted.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("superseded task should abort promptly");

    // Assert
    assert_eq!(
        session_manager.workflow_state.title_generation_tasks.len(),
        1
    );
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .contains_key(session_id.as_str())
    );
}

#[tokio::test]
async fn test_navigation_empty() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;

    // Act & Assert
    app.next();
    assert_eq!(app.sessions.selected_session_index(), None);

    app.previous();
    assert_eq!(app.sessions.selected_session_index(), None);
}

#[tokio::test]
async fn test_reply() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Act
    app.reply(&session_id, "Reply").await;

    // Assert
    app.sessions.sync_from_handles();
    let session = &app.sessions.sessions()[0];
    let output = session_replay_text(session);
    let activity_timestamps = app
        .services
        .db()
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");
    assert!(output.contains("Reply"));
    assert_eq!(activity_timestamps.len(), 1);
}

#[tokio::test]
async fn test_next_tab() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;

    // Act & Assert
    assert_eq!(app.tabs.current(), Tab::Projects);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Sessions);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Settings);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Projects);
}
