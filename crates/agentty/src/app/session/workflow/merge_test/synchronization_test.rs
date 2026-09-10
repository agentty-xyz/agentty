use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::MockOneShotClient;
use ag_forge as forge;
use ag_git as git;
use ag_git::{GitClient, GitError};
use ag_session::SessionRole;
use mockall::Sequence;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::super::super::StatusTransition;
use super::super::super::worker::has_unfinished_rebase_operation;
use super::super::{
    FinalizeRebaseInput, MergeStartRestoreContext, MockSyncAssistClient, REBASE_ASSIST_POLICY,
    RealSyncAssistClient, RebaseAssistInput, RebaseAssistLoopInput, RebaseAssistMode,
    RebaseAssistWorkspace, RebasePlan, SessionMergeService, SyncSessionStartError,
};
use super::support::{
    assert_managed_merge_metadata, blocking_auto_push_git_client, build_merge_task_input_for_test,
    build_rebase_assist_input_for_test, build_sync_rebase_input_for_test,
    collect_published_branch_sync_statuses, create_passthrough_mock_fs_client,
    empty_review_request_client, empty_transcript,
    insert_published_rebase_session_with_review_request, linked_github_review_request,
    metadata_sync_git_client, metadata_sync_one_shot_client, metadata_sync_review_request_client,
    session_operation_row, successful_sync_conflict_git_client, successful_sync_conflict_outcome,
    test_fs_client, test_one_shot_client, test_session_agent,
};
use crate::app::session::workflow::merge::SyncAssistClient;
use crate::app::session::{Clock, SessionError};
use crate::app::sync::SyncMainEventContext;
use crate::app::{AppEvent, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::{PublishedBranchSyncStatus, Status};
use crate::infra::db::AppRepositories;
use crate::infra::fs;

/// Verifies sync assistance aborts when the conflicted file fingerprint
/// repeats across attempts without any file changes.
#[tokio::test]
async fn test_run_sync_rebase_assist_loop_aborts_for_unchanged_conflict_files() {
    // Arrange
    let temp_dir = tempdir().expect("create temp dir");
    let conflict_file = temp_dir.path().join("src/lib.rs");
    std::fs::create_dir_all(
        conflict_file
            .parent()
            .expect("conflict file should have a parent directory"),
    )
    .expect("create conflict directory");
    std::fs::write(&conflict_file, "<<<<<<< HEAD\none\n=======\ntwo\n>>>>>>>")
        .expect("write conflict file");

    let fingerprint_fs_client = create_passthrough_mock_fs_client();
    let fingerprint = SessionManager::conflicted_file_fingerprint(
        &fingerprint_fs_client,
        temp_dir.path(),
        &["src/lib.rs".to_string()],
    )
    .await;

    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_list_conflicted_files()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_, _| Box::pin(async { Ok(vec![]) }));
    mock_git_client
        .expect_stage_all()
        .times(REBASE_ASSIST_POLICY.max_attempts - 1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_has_unmerged_paths()
        .times(REBASE_ASSIST_POLICY.max_attempts - 1)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));

    let mut mock_sync_assist_client = MockSyncAssistClient::new();
    mock_sync_assist_client
        .expect_resolve_rebase_conflicts()
        .times(REBASE_ASSIST_POLICY.max_attempts - 1)
        .returning(|_, _, _| Box::pin(async { Ok(()) }));

    let input = build_sync_rebase_input_for_test(
        temp_dir.path().to_path_buf(),
        Arc::new(mock_git_client),
        Arc::new(mock_sync_assist_client),
    );

    // Act
    let result = SessionManager::run_sync_rebase_assist_loop(input, fingerprint).await;

    // Assert
    assert_eq!(
        result.map_err(|error| error.to_string()),
        Err("Sync rebase assistance made no progress: conflicted files did not change".to_string())
    );
}

#[tokio::test]
async fn test_execute_rebase_workflow_aborts_when_assist_loop_fails() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
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
        .withf(|_, base_branch| base_branch == "origin/main")
        .returning(|_, _| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_head_commit_message()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(Some("Existing session commit".to_string())) }));
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|_, base_branch, _, _| base_branch == "origin/main")
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_head_short_hash()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Box::pin(async { Err(GitError::OutputParse("state query failed".to_string())) })
        });
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (_temp_dir, mut input) =
        build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;
    input.rebase_plan = RebasePlan::target("origin/main".to_string());

    // Act
    let result = SessionManager::execute_rebase_workflow(input).await;

    // Assert
    let error = result.expect_err("rebase workflow should fail");
    assert!(
        error
            .to_string()
            .contains("Failed to sync: state query failed"),
        "workflow error should include assist-loop failure reason"
    );
}

#[tokio::test]
async fn test_ensure_merge_target_clean_blocks_dirty_main_checkout() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    db.sessions()
        .insert_session("session-123", "gpt-5.6-sol", "main", "Merging", project_id)
        .await
        .expect("failed to insert merge session row");
    let status = Arc::new(Mutex::new(Status::Merging));
    let session_update_versions = Arc::default();
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    let status_transition = StatusTransition::from_parts(
        app_event_tx,
        Arc::new(crate::infra::clock::RealClock),
        db.clone(),
        "session-123",
        Arc::clone(&session_update_versions),
        Arc::clone(&status),
    );
    let restore_context = MergeStartRestoreContext {
        db: &db,
        session_id: "session-123",
        status_transition: &status_transition,
    };

    // Act
    let result = SessionMergeService::ensure_merge_target_clean(
        &mock_git_client,
        PathBuf::from("/tmp/project"),
        "main",
        &restore_context,
    )
    .await;

    // Assert
    let error = result.expect_err("dirty merge target should block merge");
    assert_eq!(
        error.to_string(),
        "Merge cannot run while `main` has uncommitted changes.\nCommit or stash changes in \
         `main`, then try again."
    );
    assert_eq!(
        *status.lock().expect("status lock poisoned"),
        Status::Review
    );
}

#[test]
fn test_rebase_assist_prompt_includes_branch_and_files() {
    // Arrange
    let base_branch = "main";
    let conflicted_files = vec!["src/lib.rs".to_string(), "README.md".to_string()];

    // Act
    let prompt = SessionManager::rebase_assist_prompt(
        base_branch,
        &conflicted_files,
        RebaseAssistWorkspace::SessionWorktree,
    )
    .expect("rebase assist prompt should render");
    let main_checkout_prompt = SessionManager::rebase_assist_prompt(
        base_branch,
        &conflicted_files,
        RebaseAssistWorkspace::MainCheckout,
    )
    .expect("main-checkout rebase assist prompt should render");

    // Assert
    assert!(prompt.contains("rebasing onto `main`"));
    assert!(prompt.contains("- src/lib.rs"));
    assert!(prompt.contains("- README.md"));
    assert!(prompt.contains("session's isolated git worktree"));
    assert!(main_checkout_prompt.contains("user's main repository checkout"));
    assert!(main_checkout_prompt.contains("change only the conflicted files"));
    assert!(prompt.contains("Remove every conflict marker"));
    assert!(prompt.contains("inspect the commits involved"));
    assert!(prompt.contains("understand their intent"));
    assert!(prompt.contains("intended behavior from both sides"));
    assert!(prompt.contains("Limit git inspection to read-only commands"));
    assert!(prompt.contains("Never run mutating commands"));
    assert!(prompt.contains("do not create commits"));
    assert!(prompt.contains("repository-defined quality checks"));
    assert!(prompt.contains("affected"));
    assert!(prompt.contains("dependencies or dependents"));
    assert!(prompt.contains("full repository\n  test/check suite"));
    assert!(prompt.contains("Return the required protocol JSON object"));
}

#[tokio::test]
async fn test_rebase_assist_input_clone() {
    // Arrange
    let (tx, _rx) = mpsc::unbounded_channel();
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let input = RebaseAssistInput {
        app_event_tx: tx,
        assist_mode: RebaseAssistMode::OneShot,
        child_pid: Arc::new(Mutex::new(None)),
        db,
        folder: temp_dir.path().to_path_buf(),
        fs_client: test_fs_client(),
        git_client: Arc::new(git::RealGitClient),
        id: "session-123".into(),
        one_shot_client: test_one_shot_client(),
        transcript: empty_transcript(),
        rebase_plan: RebasePlan::target("origin/main".to_string()),
        session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        session_update_versions: Arc::default(),
    };

    // Act
    let cloned_input = input.clone();

    // Assert
    assert_eq!(input.id, cloned_input.id);
    assert_eq!(input.folder, cloned_input.folder);
    assert_eq!(input.rebase_plan, cloned_input.rebase_plan);
    assert_eq!(input.session_agent, cloned_input.session_agent);
}

/// Verifies merged-session cleanup surfaces branch deletion failures after
/// the worktree itself has already been removed.
#[tokio::test]
async fn test_cleanup_merged_session_worktree_reports_delete_branch_failure() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let folder = temp_dir.path().join("session-worktree");
    let repo_root = temp_dir.path().join("repo-root");
    let source_branch = "wt/session-123".to_string();
    let mut mock_git_client = git::MockGitClient::new();
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Err(GitError::OutputParse("delete failed".to_string())) })
        });
    mock_fs_client.expect_remove_dir_all().times(0);

    // Act
    let result = SessionManager::cleanup_merged_session_worktree(
        folder,
        Arc::new(mock_fs_client),
        Arc::new(mock_git_client),
        source_branch,
        Some(repo_root),
    )
    .await;

    // Assert
    let error = result.expect_err("cleanup should fail on branch deletion error");
    assert_eq!(error.to_string(), "delete failed");
}

#[tokio::test]
/// Verifies that a successful rebase triggers an auto-push and reports the
/// successful sync state when the session has a previously published
/// upstream branch.
async fn test_finalize_rebase_task_triggers_auto_push_for_published_branch() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess-rebase",
            "gemini-3.8-flash",
            "main",
            "Rebasing",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_published_upstream_ref(
            "sess-rebase",
            Some("origin/wt/sess-rebase".to_string()),
        )
        .await
        .expect("failed to set published upstream ref");

    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let branch_operation_lock = Arc::new(tokio::sync::Mutex::new(()));
    let temp_dir = tempdir().expect("failed to create temp dir");
    let folder = temp_dir.path().join("sess-rebase");
    let session_update_versions = Arc::default();
    let status = Arc::new(Mutex::new(Status::Rebasing));
    let transcript = empty_transcript();
    let clock: Arc<dyn Clock> = Arc::new(crate::infra::clock::RealClock);
    let review_request_client = empty_review_request_client();
    let status_transition = StatusTransition::from_parts(
        app_event_tx.clone(),
        Arc::clone(&clock),
        db.clone(),
        "sess-rebase",
        Arc::clone(&session_update_versions),
        Arc::clone(&status),
    );
    let push_started = Arc::new(tokio::sync::Notify::new());
    let release_push = Arc::new(tokio::sync::Notify::new());
    let git_client: Arc<dyn GitClient> = Arc::new(blocking_auto_push_git_client(
        Arc::clone(&push_started),
        Arc::clone(&release_push),
    ));

    // Act
    SessionManager::finalize_rebase_task(FinalizeRebaseInput {
        app_event_tx: &app_event_tx,
        branch_operation_guard: Arc::clone(&branch_operation_lock).lock_owned().await,
        clock: &clock,
        db: &db,
        folder: &folder,
        git_client: &git_client,
        id: "sess-rebase",
        one_shot_client: &test_one_shot_client(),
        rebase_result: Ok("Successfully synced wt/sess-rebase onto main".to_string()),
        review_request_client: &review_request_client,
        session_agent: test_session_agent(),
        session_update_versions: &session_update_versions,
        status_transition: &status_transition,
        transcript: &transcript,
    })
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), push_started.notified())
        .await
        .expect("timed out waiting for auto-push to start");

    // Assert
    assert!(
        branch_operation_lock.try_lock().is_err(),
        "post-rebase auto-push should retain branch-operation ownership"
    );
    release_push.notify_one();
    let sync_events = tokio::time::timeout(std::time::Duration::from_secs(2), async {
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

    assert_eq!(sync_events[0].0, "sess-rebase");
    assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
    assert_eq!(sync_events[1].0, "sess-rebase");
    assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
    assert_eq!(sync_events[0].1, sync_events[1].1);

    let transcript_text = transcript
        .lock()
        .expect("transcript lock poisoned")
        .replay_text()
        .unwrap_or_default();
    assert!(transcript_text.contains("[Sync] Successfully synced"));
}

#[tokio::test]
/// Verifies post-rebase auto-push reconciles live PR/MR metadata without a
/// persisted baseline.
async fn test_finalize_rebase_task_syncs_review_request_metadata_after_auto_push() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_published_rebase_session_with_review_request(&db).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let temp_dir = tempdir().expect("failed to create temp dir");
    let folder = temp_dir.path().join("sess-rebase");
    let session_update_versions = Arc::default();
    let status = Arc::new(Mutex::new(Status::Rebasing));
    let transcript = empty_transcript();
    let clock: Arc<dyn Clock> = Arc::new(crate::infra::clock::RealClock);
    let status_transition = StatusTransition::from_parts(
        app_event_tx.clone(),
        Arc::clone(&clock),
        db.clone(),
        "sess-rebase",
        Arc::clone(&session_update_versions),
        Arc::clone(&status),
    );
    let git_client: Arc<dyn GitClient> = Arc::new(metadata_sync_git_client());
    let review_request_client: Arc<dyn forge::ReviewRequestClient> =
        Arc::new(metadata_sync_review_request_client(folder.clone()));
    let branch_operation_guard = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;

    // Act
    SessionManager::finalize_rebase_task(FinalizeRebaseInput {
        app_event_tx: &app_event_tx,
        branch_operation_guard,
        clock: &clock,
        db: &db,
        folder: &folder,
        git_client: &git_client,
        id: "sess-rebase",
        one_shot_client: &metadata_sync_one_shot_client(),
        rebase_result: Ok("Successfully synced wt/sess-rebase onto main".to_string()),
        review_request_client: &review_request_client,
        session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        session_update_versions: &session_update_versions,
        status_transition: &status_transition,
        transcript: &transcript,
    })
    .await;
    let sync_events = collect_published_branch_sync_statuses(&mut app_event_rx).await;
    let review_request = db
        .reviews()
        .load_session_review_request("sess-rebase")
        .await
        .expect("failed to load review request")
        .expect("review request should remain linked");

    // Assert
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
/// Verifies post-rebase metadata lookup failures remain visible after the
/// branch itself pushes successfully.
async fn test_finalize_rebase_task_warns_when_commit_message_lookup_fails() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_published_rebase_session_with_review_request(&db).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let temp_dir = tempdir().expect("failed to create temp dir");
    let folder = temp_dir.path().join("sess-rebase");
    let session_update_versions = Arc::default();
    let status = Arc::new(Mutex::new(Status::Rebasing));
    let transcript = empty_transcript();
    let clock: Arc<dyn Clock> = Arc::new(crate::infra::clock::RealClock);
    let status_transition = StatusTransition::from_parts(
        app_event_tx.clone(),
        Arc::clone(&clock),
        db.clone(),
        "sess-rebase",
        Arc::clone(&session_update_versions),
        Arc::clone(&status),
    );
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess-reb".to_string()) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/wt/sess-rebase".to_string()) }));
    mock_git_client
        .expect_head_commit_message()
        .once()
        .returning(|_| {
            Box::pin(async {
                Err(git::GitError::OutputParse(
                    "commit message unavailable".to_string(),
                ))
            })
        });
    let git_client: Arc<dyn GitClient> = Arc::new(mock_git_client);
    let review_request_client = empty_review_request_client();
    let branch_operation_guard = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;

    // Act
    SessionManager::finalize_rebase_task(FinalizeRebaseInput {
        app_event_tx: &app_event_tx,
        branch_operation_guard,
        clock: &clock,
        db: &db,
        folder: &folder,
        git_client: &git_client,
        id: "sess-rebase",
        one_shot_client: &test_one_shot_client(),
        rebase_result: Ok("Successfully synced wt/sess-rebase onto main".to_string()),
        review_request_client: &review_request_client,
        session_agent: test_session_agent(),
        session_update_versions: &session_update_versions,
        status_transition: &status_transition,
        transcript: &transcript,
    })
    .await;
    let sync_events = collect_published_branch_sync_statuses(&mut app_event_rx).await;
    let transcript_text = transcript
        .lock()
        .expect("transcript lock poisoned")
        .replay_text()
        .unwrap_or_default();

    // Assert
    assert_eq!(
        sync_events,
        vec![
            PublishedBranchSyncStatus::InProgress,
            PublishedBranchSyncStatus::Succeeded,
        ]
    );
    assert!(transcript_text.contains("[Review Request Sync Warning]"));
    assert!(transcript_text.contains("commit message unavailable"));
}

#[tokio::test]
/// Verifies that a successful rebase does not trigger auto-push when the
/// session has no published upstream branch.
async fn test_finalize_rebase_task_skips_auto_push_without_published_branch() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess-no-push",
            "gemini-3.8-flash",
            "main",
            "Rebasing",
            project_id,
        )
        .await
        .expect("failed to insert session");

    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let temp_dir = tempdir().expect("failed to create temp dir");
    let folder = temp_dir.path().join("sess-no-push");
    let session_update_versions = Arc::default();
    let status = Arc::new(Mutex::new(Status::Rebasing));
    let transcript = empty_transcript();
    let clock: Arc<dyn Clock> = Arc::new(crate::infra::clock::RealClock);
    let review_request_client = empty_review_request_client();
    let status_transition = StatusTransition::from_parts(
        app_event_tx.clone(),
        Arc::clone(&clock),
        db.clone(),
        "sess-no-push",
        Arc::clone(&session_update_versions),
        Arc::clone(&status),
    );
    let git_client: Arc<dyn GitClient> = Arc::new(git::MockGitClient::new());
    let branch_operation_guard = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;

    // Act
    SessionManager::finalize_rebase_task(FinalizeRebaseInput {
        app_event_tx: &app_event_tx,
        branch_operation_guard,
        clock: &clock,
        db: &db,
        folder: &folder,
        git_client: &git_client,
        id: "sess-no-push",
        one_shot_client: &test_one_shot_client(),
        rebase_result: Ok("Successfully synced wt/sess-no-push onto main".to_string()),
        review_request_client: &review_request_client,
        session_agent: test_session_agent(),
        session_update_versions: &session_update_versions,
        status_transition: &status_transition,
        transcript: &transcript,
    })
    .await;

    // Assert — no PublishedBranchSyncUpdated events should be emitted,
    // but the stack-sync fan-out event should still be available.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut sync_event_count = 0;
    let mut stack_sync_event_count = 0;
    while let Ok(event) = app_event_rx.try_recv() {
        if matches!(&event, AppEvent::PublishedBranchSyncUpdated { .. }) {
            sync_event_count += 1;
        }
        if matches!(&event, AppEvent::StackedParentSyncCompleted { .. }) {
            stack_sync_event_count += 1;
        }
    }
    assert_eq!(
        sync_event_count, 0,
        "should not emit sync events without published branch"
    );
    assert_eq!(
        stack_sync_event_count, 1,
        "should emit one stacked parent sync completion event"
    );

    let transcript_text = transcript
        .lock()
        .expect("transcript lock poisoned")
        .replay_text()
        .unwrap_or_default();
    assert!(transcript_text.contains("[Sync] Successfully synced"));
}

#[tokio::test]
async fn test_run_rebase_assist_loop_core_aborts_on_early_error() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_list_conflicted_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Box::pin(async {
                Err(GitError::OutputParse(
                    "failed to list conflicts".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_assist_loop_core(
        RebaseAssistLoopInput::Session(Box::new(input)),
        None,
    )
    .await;

    // Assert
    let error = result.expect_err("assist loop should fail");
    assert_eq!(error.to_string(), "failed to list conflicts");
}

#[tokio::test]
async fn test_run_rebase_assist_loop_core_aborts_when_pre_commit_hook_fails() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_list_conflicted_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
    mock_git_client
        .expect_run_pre_commit_hook()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Box::pin(async {
                Err(GitError::CommandFailed {
                    command: "git hook run pre-commit".to_string(),
                    stderr: "resolved conflict rejected".to_string(),
                })
            })
        });
    mock_git_client.expect_rebase_continue().times(0);
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_assist_loop_core(
        RebaseAssistLoopInput::Session(Box::new(input)),
        None,
    )
    .await;

    // Assert
    let error = result.expect_err("hook failure should stop assisted rebase");
    assert_eq!(
        error.to_string(),
        "Pre-commit hook rejected resolved rebase conflicts: git hook run pre-commit: resolved \
         conflict rejected"
    );
}

/// Verifies session rebase assistance stops when the same conflict detail
/// repeats after the initial conflict state.
#[tokio::test]
async fn test_run_rebase_assist_loop_core_stops_on_repeated_conflict_detail() {
    // Arrange
    let repeated_detail = "CONFLICT (content): Merge conflict in src/lib.rs".to_string();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_list_conflicted_files()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
    mock_git_client
        .expect_run_pre_commit_hook()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_rebase_continue()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning({
            let repeated_detail = repeated_detail.clone();

            move |_| {
                let repeated_detail = repeated_detail.clone();

                Box::pin(async move {
                    Ok(git::RebaseStepResult::Conflict {
                        detail: repeated_detail,
                    })
                })
            }
        });
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_assist_loop_core(
        RebaseAssistLoopInput::Session(Box::new(input)),
        Some(repeated_detail.clone()),
    )
    .await;

    // Assert
    let error = result.expect_err("assist loop should stop on repeated conflict detail");
    assert_eq!(
        error.to_string(),
        format!(
            "Rebase assistance made no progress: repeated identical conflict state. Last detail: \
             {repeated_detail}"
        )
    );
}

/// Verifies the first `git rebase` conflict detail seeds no-progress
/// tracking for the session rebase loop.
#[tokio::test]
async fn test_run_rebase_assist_loop_tracks_initial_rebase_conflict_detail() {
    // Arrange
    let repeated_detail = "CONFLICT (content): Merge conflict in src/lib.rs".to_string();
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning({
            let repeated_detail = repeated_detail.clone();

            move |_, _| {
                let repeated_detail = repeated_detail.clone();

                Box::pin(async move {
                    Ok(git::RebaseStepResult::Conflict {
                        detail: repeated_detail,
                    })
                })
            }
        });
    for _ in 0..REBASE_ASSIST_POLICY.max_attempts {
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_run_pre_commit_hook()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_rebase_continue()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let repeated_detail = repeated_detail.clone();

                move |_| {
                    let repeated_detail = repeated_detail.clone();

                    Box::pin(async move {
                        Ok(git::RebaseStepResult::Conflict {
                            detail: repeated_detail,
                        })
                    })
                }
            });
    }
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_assist_loop(input).await;

    // Assert
    let error = result.expect_err("assist loop should stop on repeated conflict detail");
    assert_eq!(
        error.to_string(),
        format!(
            "Rebase assistance made no progress: repeated identical conflict state. Last detail: \
             {repeated_detail}"
        )
    );
}

/// Verifies session rebase assistance surfaces the final conflict detail
/// when every retry hits a distinct conflict state until the retry budget
/// is exhausted.
#[tokio::test]
async fn test_run_rebase_assist_loop_core_reports_retry_exhaustion_detail() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    for detail in [
        "CONFLICT (content): Merge conflict in src/lib.rs",
        "CONFLICT (content): Merge conflict in src/main.rs",
        "CONFLICT (content): Merge conflict in README.md",
    ] {
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_run_pre_commit_hook()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_rebase_continue()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let detail = detail.to_string();

                move |_| {
                    let detail = detail.clone();

                    Box::pin(async move { Ok(git::RebaseStepResult::Conflict { detail }) })
                }
            });
    }
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_assist_loop_core(
        RebaseAssistLoopInput::Session(Box::new(input)),
        None,
    )
    .await;

    // Assert
    let error = result.expect_err("assist loop should report the final retry conflict");
    assert_eq!(
        error.to_string(),
        "Rebase still has conflicts after assistance: CONFLICT (content): Merge conflict in \
         README.md"
    );
}

#[tokio::test]
async fn test_run_rebase_start_recovers_stale_rebase_state_and_retries() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| {
            Box::pin(async {
                Err(GitError::OutputParse(
                    "fatal: It seems that there is already a rebase-merge directory".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_start(&input).await;

    // Assert
    let step_result = result.expect("rebase start should succeed");
    assert_eq!(step_result, git::RebaseStepResult::Completed);
}

#[tokio::test]
async fn test_run_rebase_start_uses_resolved_rebase_target() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_rebase_start()
        .once()
        .withf(|_, rebase_target| rebase_target == "origin/main")
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    let (_temp_dir, mut input) =
        build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;
    input.rebase_plan = RebasePlan::target("origin/main".to_string());

    // Act
    let result = SessionManager::run_rebase_start(&input).await;

    // Assert
    let step_result = result.expect("rebase start should succeed");
    assert_eq!(step_result, git::RebaseStepResult::Completed);
}

#[tokio::test]
async fn test_run_rebase_start_uses_onto_plan() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client.expect_rebase_start().times(0);
    mock_git_client
        .expect_rebase_onto_start()
        .once()
        .withf(|_, new_base, old_base| new_base == "main" && old_base == "parent-tip")
        .returning(|_, _, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    let (_temp_dir, mut input) =
        build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;
    input.rebase_plan = RebasePlan::Onto {
        new_base: "main".to_string(),
        old_base: "parent-tip".to_string(),
    };

    // Act
    let result = SessionManager::run_rebase_start(&input).await;

    // Assert
    let step_result = result.expect("rebase start should succeed");
    assert_eq!(step_result, git::RebaseStepResult::Completed);
}

#[tokio::test]
async fn test_run_rebase_start_reports_cleanup_failure_for_stale_rebase_state() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| {
            Box::pin(async {
                Err(GitError::OutputParse(
                    "fatal: It seems that there is already a rebase-merge directory".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Err(GitError::OutputParse("abort failed".to_string())) }));
    let (_temp_dir, input) = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

    // Act
    let result = SessionManager::run_rebase_start(&input).await;

    // Assert
    let error = result.expect_err("cleanup failure should stop retry flow");
    assert!(
        error
            .to_string()
            .contains("Cleanup with `git rebase --abort` failed: abort failed"),
        "error should include abort failure detail"
    );
}

#[tokio::test]
/// Ensures [`SessionError`] from the sync assist client propagates through
/// `run_sync_rebase_assist_agent` with an operation-specific context
/// prefix.
async fn test_sync_rebase_assist_agent_adds_context_to_workflow_error() {
    // Arrange
    let mut mock_sync_assist_client = MockSyncAssistClient::new();
    mock_sync_assist_client
        .expect_resolve_rebase_conflicts()
        .times(1)
        .returning(|_, _, _| {
            Box::pin(async {
                Err(SessionError::Workflow(
                    "agent backend unavailable".to_string(),
                ))
            })
        });
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let input = build_sync_rebase_input_for_test(
        temp_dir.path().to_path_buf(),
        Arc::new(git::MockGitClient::new()),
        Arc::new(mock_sync_assist_client),
    );

    // Act
    let result =
        SessionManager::run_sync_rebase_assist_agent(&input, &["src/lib.rs".to_string()]).await;

    // Assert
    let error = result.expect_err("assist failure should propagate");
    assert!(
        matches!(error, SessionError::Workflow(_)),
        "expected SessionError::Workflow, got: {error:?}"
    );
    assert_eq!(
        error.to_string(),
        "Sync rebase assistance failed: agent backend unavailable"
    );
}

/// Verifies sync conflict checks keep the loop in assistance mode when
/// staged files still contain conflict markers.
#[tokio::test]
async fn test_stage_and_check_for_sync_conflicts_detects_remaining_markers() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_stage_all()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_has_unmerged_paths()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));

    let mut mock_sync_assist_client = MockSyncAssistClient::new();
    mock_sync_assist_client
        .expect_resolve_rebase_conflicts()
        .times(0);
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let assist_input = RebaseAssistLoopInput::Project(build_sync_rebase_input_for_test(
        temp_dir.path().to_path_buf(),
        Arc::new(mock_git_client),
        Arc::new(mock_sync_assist_client),
    ));

    // Act
    let still_has_conflicts = assist_input
        .stage_and_check_for_conflicts(&["src/lib.rs".to_string()])
        .await;

    // Assert
    let has_conflicts = still_has_conflicts.expect("stage_and_check should succeed");
    assert!(has_conflicts);
}

#[test]
fn test_has_unfinished_rebase_operation_matches_session_rebase() {
    // Arrange
    let operations = vec![
        session_operation_row("op-1", "session-a", "reply"),
        session_operation_row("op-2", "session-b", "rebase"),
        session_operation_row("op-3", "session-a", "rebase"),
    ];

    // Act
    let has_rebase = has_unfinished_rebase_operation(&operations, "session-a");

    // Assert
    assert!(has_rebase);
}

#[test]
fn test_has_unfinished_rebase_operation_ignores_other_sessions_and_kinds() {
    // Arrange
    let operations = vec![
        session_operation_row("op-1", "session-a", "reply"),
        session_operation_row("op-2", "session-b", "rebase"),
    ];

    // Act
    let has_rebase = has_unfinished_rebase_operation(&operations, "session-a");

    // Assert
    assert!(!has_rebase);
}

/// Verifies sync assistance merges tracked conflicts with staged conflict
/// marker files and returns a sorted unique list.
#[tokio::test]
async fn test_load_sync_conflicted_files_merges_and_sorts_results() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_list_conflicted_files()
        .times(1)
        .returning(|_| {
            Box::pin(async { Ok(vec!["src/b.rs".to_string(), "src/c.rs".to_string()]) })
        });
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Ok(vec!["src/a.rs".to_string(), "src/c.rs".to_string()]) })
        });

    let mut mock_sync_assist_client = MockSyncAssistClient::new();
    mock_sync_assist_client
        .expect_resolve_rebase_conflicts()
        .times(0);
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let assist_input = RebaseAssistLoopInput::Project(build_sync_rebase_input_for_test(
        temp_dir.path().to_path_buf(),
        Arc::new(mock_git_client),
        Arc::new(mock_sync_assist_client),
    ));

    // Act
    let conflicted_files = assist_input.load_conflicted_files(&[]).await;

    // Assert
    let files = conflicted_files.expect("load_conflicted_files should succeed");
    assert_eq!(
        files,
        vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string(),
        ]
    );
}

#[tokio::test]
async fn test_resolve_session_rebase_target_keeps_local_base_for_unpublished_session() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let project_path = temp_dir.path().to_string_lossy().to_string();
    let project_id = db
        .projects()
        .upsert_project(&project_path, Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("sess-local", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client.expect_fetch_remote().times(0);
    let folder = temp_dir.path().join("sess-local");

    // Act
    let rebase_target = SessionManager::resolve_session_rebase_target(
        &db,
        &mock_git_client,
        &folder,
        "sess-local",
        "main",
    )
    .await
    .expect("failed to resolve local rebase target");

    // Assert
    assert_eq!(rebase_target, "main");
}

#[tokio::test]
async fn test_resolve_session_rebase_target_fetches_remote_base_for_published_session() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let project_path = temp_dir.path().to_string_lossy().to_string();
    let project_id = db
        .projects()
        .upsert_project(&project_path, Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("sess-remote", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_published_upstream_ref(
            "sess-remote",
            Some("origin/wt/sess-remote".to_string()),
        )
        .await
        .expect("failed to set published upstream");
    let folder = temp_dir.path().join("sess-remote");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_fetch_remote()
        .once()
        .withf(|repo_path| repo_path.ends_with("sess-remote"))
        .returning(|_| Box::pin(async { Ok(()) }));

    // Act
    let rebase_target = SessionManager::resolve_session_rebase_target(
        &db,
        &mock_git_client,
        &folder,
        "sess-remote",
        "main",
    )
    .await
    .expect("failed to resolve remote rebase target");

    // Assert
    assert_eq!(rebase_target, "origin/main");
}

#[tokio::test]
async fn test_resolve_session_rebase_plan_uses_recorded_stack_base() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let project_path = temp_dir.path().to_string_lossy().to_string();
    let project_id = db
        .projects()
        .upsert_project(&project_path, Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("child-session", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_stack_base_commit_hash("child-session", Some("parent-tip".to_string()))
        .await
        .expect("failed to set stack base hash");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client.expect_fetch_remote().times(0);
    let folder = temp_dir.path().join("child-session");

    // Act
    let rebase_plan = SessionManager::resolve_session_rebase_plan(
        &db,
        &mock_git_client,
        &folder,
        "child-session",
        "main",
    )
    .await
    .expect("failed to resolve rebase plan");

    // Assert
    assert_eq!(
        rebase_plan,
        RebasePlan::Onto {
            new_base: "main".to_string(),
            old_base: "parent-tip".to_string(),
        }
    );
}

#[tokio::test]
async fn test_resolve_session_rebase_target_reports_published_fetch_failure() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let project_path = temp_dir.path().to_string_lossy().to_string();
    let project_id = db
        .projects()
        .upsert_project(&project_path, Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("sess-fetch", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_published_upstream_ref(
            "sess-fetch",
            Some("origin/wt/sess-fetch".to_string()),
        )
        .await
        .expect("failed to set published upstream");
    let folder = temp_dir.path().join("sess-fetch");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_fetch_remote()
        .once()
        .returning(|_| Box::pin(async { Err(GitError::OutputParse("fetch failed".into())) }));

    // Act
    let result = SessionManager::resolve_session_rebase_target(
        &db,
        &mock_git_client,
        &folder,
        "sess-fetch",
        "main",
    )
    .await;

    // Assert
    let error = result.expect_err("fetch failure should stop published rebase");
    assert!(
        error
            .to_string()
            .contains("Failed to fetch `origin` before rebasing published session branch"),
        "error should name the published upstream remote"
    );
}

#[tokio::test]
async fn test_execute_merge_workflow_reuses_session_head_commit_message() {
    // Arrange
    let expected_merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    let canonical_commit_message = "Refine merge flow\n\n- Reuse the session commit body";
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock_git_client
        .expect_head_hash()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok("parent-tip".to_string()) }));
    mock_git_client
        .expect_squash_merge_diff()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _, _| Box::pin(async { Ok("diff --git a/file b/file".to_string()) }));
    mock_git_client
        .expect_head_commit_message()
        .times(1)
        .in_sequence(&mut sequence)
        .returning({
            let canonical_commit_message = canonical_commit_message.to_string();

            move |_| {
                let canonical_commit_message = canonical_commit_message.clone();

                Box::pin(async move { Ok(Some(canonical_commit_message)) })
            }
        });
    mock_git_client
        .expect_squash_merge()
        .times(1)
        .in_sequence(&mut sequence)
        .returning({
            let canonical_commit_message = canonical_commit_message.to_string();

            move |_, _, _, commit_message| {
                let canonical_commit_message = canonical_commit_message.clone();

                Box::pin(async move {
                    assert_eq!(commit_message, canonical_commit_message);

                    Ok(git::SquashMergeOutcome::Committed)
                })
            }
        });
    mock_git_client
        .expect_head_hash()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            let expected_merged_commit_hash = expected_merged_commit_hash.to_string();

            Box::pin(async move { Ok(expected_merged_commit_hash) })
        });
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    let (_temp_dir, mut input) = build_merge_task_input_for_test(Arc::new(mock_git_client)).await;
    input.archive_diff = true;
    let project_id = input
        .db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    input
        .db
        .sessions()
        .insert_session("session-123", "gpt-5.6-sol", "main", "Merging", project_id)
        .await
        .expect("failed to insert merge session row");
    let db = input.db.clone();

    // Act
    let result = SessionManager::execute_merge_workflow(input).await;

    // Assert
    let message = result.expect("merge workflow should succeed");
    assert_eq!(message, "Successfully merged wt/session-123 into main");
    assert_managed_merge_metadata(&db, expected_merged_commit_hash).await;
}

#[tokio::test]
async fn test_execute_merge_workflow_skips_commit_creation_for_empty_squash_diff() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock_git_client
        .expect_head_hash()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok("parent-tip".to_string()) }));
    mock_git_client
        .expect_squash_merge_diff()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _, _| Box::pin(async { Ok("   ".to_string()) }));
    mock_git_client.expect_head_commit_message().times(0);
    mock_git_client.expect_squash_merge().times(0);
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    let (_temp_dir, input) = build_merge_task_input_for_test(Arc::new(mock_git_client)).await;
    let project_id = input
        .db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    input
        .db
        .sessions()
        .insert_session("session-123", "gpt-5.6-sol", "main", "Merging", project_id)
        .await
        .expect("failed to insert merge session row");
    let db = input.db.clone();

    // Act
    let result = SessionManager::execute_merge_workflow(input).await;

    // Assert
    let message = result.expect("merge workflow should succeed for empty diff");
    assert_eq!(
        message,
        "Session changes from wt/session-123 are already present in main"
    );
    let merged_commit_hash = db
        .sessions()
        .load_session_merged_commit_hash("session-123")
        .await
        .expect("failed to load merged commit hash");
    assert_eq!(merged_commit_hash, None);
    let archived_diff = db
        .sessions()
        .load_session_archived_diff("session-123")
        .await
        .expect("failed to load archived diff");
    assert_eq!(archived_diff, None);
}

#[tokio::test]
async fn test_merge_session_rejects_linked_review_request_before_workflow_start() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .review_request(Some(linked_github_review_request()))
            .build(),
    );

    // Act
    let result = app
        .sessions
        .merge_session("session-id", &app.projects, &app.services)
        .await;

    // Assert
    let error = result.expect_err("linked review request should block merge workflow");
    assert_eq!(
        error.to_string(),
        "Merge cannot run for linked review requests or while another stack session is active"
    );
}

#[tokio::test]
async fn test_merge_session_rejects_orchestrator_before_workflow_start() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .role(SessionRole::Orchestrator)
            .status(Status::Review)
            .build(),
    );

    // Act
    let result = app
        .sessions
        .merge_session("session-id", &app.projects, &app.services)
        .await;

    // Assert
    assert_eq!(
        result
            .expect_err("orchestrator merge should fail")
            .to_string(),
        "Orchestrator sessions do not own branch changes"
    );
}

#[test]
fn test_real_sync_assist_client_new_owns_one_shot_client() {
    // Arrange / Act
    let sync_assist_client = RealSyncAssistClient::new();

    // Assert
    assert_eq!(Arc::strong_count(&sync_assist_client.one_shot_client), 1);
}

#[tokio::test]
async fn test_real_sync_assist_client_submits_utility_prompt() {
    // Arrange
    let folder = PathBuf::from("/tmp/sync-assist");
    let expected_folder = folder.clone();
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(move |request| {
            assert_eq!(request.agent_kind, AgentKind::Claude);
            assert_eq!(request.folder, expected_folder);
            assert_eq!(request.model, AgentModel::ClaudeSonnet5);
            assert_eq!(request.prompt, "Resolve sync conflicts");
            assert_eq!(
                request.request_kind,
                ag_agent::AgentRequestKind::UtilityPrompt
            );

            Ok(agent::OneShotSubmission {
                response: ag_protocol::AgentResponse::plain("resolved"),
                stats: agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });
    let sync_assist_client = RealSyncAssistClient {
        one_shot_client: Arc::new(one_shot_client),
    };

    // Act
    let result = sync_assist_client
        .resolve_rebase_conflicts(
            folder,
            "Resolve sync conflicts".to_string(),
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        )
        .await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_sync_main_for_project_resolves_conflicts_with_assistance() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let working_dir = temp_dir.path().to_path_buf();
    let mock_git_client = successful_sync_conflict_git_client();
    let mut mock_sync_assist_client = MockSyncAssistClient::new();
    mock_sync_assist_client
        .expect_resolve_rebase_conflicts()
        .times(1)
        .returning(|_, _, _| Box::pin(async { Ok(()) }));

    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let operation = crate::app::sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 1,
        project_id: 1,
        project_name: "agentty".to_string(),
    };

    // Act
    let result = SessionManager::sync_main_for_project_with_assist_client(
        Some("main".to_string()),
        working_dir,
        Some(SyncMainEventContext {
            app_event_tx,
            operation: operation.clone(),
        }),
        test_fs_client(),
        Arc::new(mock_git_client),
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        Arc::new(mock_sync_assist_client),
    )
    .await;

    // Assert
    let status_event = app_event_rx
        .try_recv()
        .expect("sync conflict status event should be emitted");
    assert!(matches!(
        status_event,
        AppEvent::SyncMainConflictResolutionStarted {
            conflicted_files,
            operation: event_operation,
        } if conflicted_files == vec!["src/lib.rs".to_string()]
            && event_operation == operation
    ));
    assert!(app_event_rx.try_recv().is_err());
    assert_eq!(
        result,
        Ok(successful_sync_conflict_outcome()),
        "sync should succeed after assistance with summary details"
    );
}

#[tokio::test]
async fn test_sync_main_for_project_fails_after_max_assistance_attempts() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let working_dir = temp_dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(|folder| Box::pin(async move { Some(folder) }));
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_get_ahead_behind()
        .times(1)
        .returning(|_| Box::pin(async { Ok((0, 1)) }));
    mock_git_client
        .expect_list_upstream_commit_titles()
        .times(1)
        .returning(|_| Box::pin(async { Ok(vec!["Upstream patch".to_string()]) }));
    mock_git_client
        .expect_pull_rebase()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Ok(git::PullRebaseResult::Conflict {
                    detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                })
            })
        });
    mock_git_client
        .expect_list_conflicted_files()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_, _| Box::pin(async { Ok(vec![]) }));
    mock_git_client
        .expect_stage_all()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_has_unmerged_paths()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client.expect_rebase_continue().times(0);
    mock_git_client.expect_push_current_branch().times(0);
    mock_git_client
        .expect_abort_rebase()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));

    let mut mock_sync_assist_client = MockSyncAssistClient::new();
    mock_sync_assist_client
        .expect_resolve_rebase_conflicts()
        .times(REBASE_ASSIST_POLICY.max_attempts)
        .returning(|_, _, _| Box::pin(async { Ok(()) }));

    // Act
    let result = SessionManager::sync_main_for_project_with_assist_client(
        Some("main".to_string()),
        working_dir,
        None,
        test_fs_client(),
        Arc::new(mock_git_client),
        AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        Arc::new(mock_sync_assist_client),
    )
    .await;

    // Assert
    let error = result.expect_err("sync should fail when conflicts remain unresolved");
    assert!(matches!(error, SyncSessionStartError::Other(_)));
    assert!(
        error
            .detail_message()
            .contains("Conflicts remain unresolved after maximum assistance attempts"),
        "error detail should mention unresolved conflicts"
    );
}
