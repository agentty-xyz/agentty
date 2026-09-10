use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_forge as forge;
use app::branch_publish::detected_forge_kind_from_git_push_error;
use app::review::ReviewCacheEntry;
use app::sync;
use mockall::predicate::eq;
use session::{SyncMainOutcome, SyncSessionStartError};
use tempfile::tempdir;

use super::super::{App, SyncReviewRequestTaskResult};
use super::support::{
    apply_next_session_diff, assert_synchronous_fork_is_ready_with_history,
    insert_review_session_with_data_dir, insert_test_ready_review, install_mock_git_client,
    merged_review_request_status_update, new_test_app_with_database_pool,
    new_test_app_with_selected_session, persist_selected_session, session_creation_resources,
    successful_manual_sync, successful_manual_sync_completion, test_prompt_mode_snapshot,
    test_turn_applied_state,
};
use crate::app;
use crate::app::branch_publish::{BranchPublishActionUpdate, BranchPublishTaskSuccess};
use crate::app::core::event::{AppEvent, AppEventBatch};
use crate::app::test_support::diff_content_hash;
use crate::app::{AppError, session};
use crate::domain::agent::AgentModel;
use crate::domain::composer::PromptAttachment;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    ForgeKind, PublishedBranchSyncStatus, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    SESSION_DATA_DIR, SessionDiffState, SessionFollowUpTask, SessionHandles, SessionId,
    SessionStats, Status,
};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::TransientMessageSlot;
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

#[test]
fn sync_push_auth_error_detects_github_from_prompt_url() {
    // Arrange
    let detail =
        "Git push failed: fatal: could not read Password for 'https://github.com/team/project': \
         terminal prompts disabled\nConfigured remotes:\n  github.com";

    // Act
    let forge_kind = detected_forge_kind_from_git_push_error(detail);

    // Assert
    assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
}

#[test]
fn sync_push_auth_error_prefers_github_when_fallback_markers_are_ambiguous() {
    // Arrange
    let detail = "Git push failed: authentication failed. Configure remotes:\n  github.com";

    // Act
    let forge_kind = detected_forge_kind_from_git_push_error(detail);

    // Assert
    assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
}

#[tokio::test]
async fn merged_session_rejects_unlaunched_follow_up_task() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.status = Status::Merged;
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: None,
        position: 0,
        text: "Must wait for local merge integration.".to_string(),
    }];
    app.sessions.push_session(source_session);

    // Act
    let action = app.selected_follow_up_task_action("session-1");
    let reply_enqueued = app.reply("session-1", "must stay read-only").await;
    let result = app
        .launch_or_open_selected_follow_up_task("session-1")
        .await;

    // Assert
    assert_eq!(action, None);
    assert!(!reply_enqueued);
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Merged sessions cannot launch new follow-up tasks"
    ));
    assert_eq!(app.sessions.sessions().len(), 1);
}

#[tokio::test]
async fn superseded_project_sync_completions_do_not_reconcile_session_state() {
    // Arrange
    let (mut app, _pool, _base_dir) = new_test_app_with_database_pool().await;
    let project_id = app.active_project_id();
    let session_id = "superseded-sync-session";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gemini38Flash.as_str(),
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert review session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    fs::create_dir_all(
        app.services
            .base_path()
            .join(session_folder_name)
            .join(SESSION_DATA_DIR),
    )
    .expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    app.latest_project_sync_operation_ids.insert(project_id, 2);
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 2,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    let mut stale_completion = successful_manual_sync_completion(project_id, "main", 1);
    stale_completion.review_request_updates = vec![sync::SyncMainReviewUpdate {
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    }];

    // Act
    app.apply_app_events(AppEvent::SyncMainCompleted {
        completion: stale_completion.clone(),
    })
    .await;
    app.pending_project_sync_completions
        .insert(project_id, stale_completion);
    app.apply_pending_project_sync_completion().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Review);
    assert!(matches!(
        app.project_sync_status.as_ref(),
        Some(sync::ProjectSyncStatus {
            context,
            phase: sync::ProjectSyncPhase::Running,
        }) if context.operation_id == 2
    ));
}

#[tokio::test]
async fn externally_merged_helpers_ignore_missing_session_runtime_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("snapshot-only")
            .status(Status::Merged)
            .build(),
    );

    // Act
    let record_result = app
        .record_externally_merged_session("snapshot-only", None)
        .await;
    let missing_session_result = app
        .complete_externally_merged_session("missing", None)
        .await;
    let missing_handles_result = app
        .complete_externally_merged_session("snapshot-only", None)
        .await;

    // Assert
    assert_eq!(record_result, None);
    assert_eq!(missing_session_result, None);
    assert_eq!(missing_handles_result, None);
}

#[tokio::test]
async fn stale_project_sync_completion_does_not_replace_newer_deferred_completion() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let inactive_project_id = app.active_project_id() + 1;
    app.latest_project_sync_operation_ids
        .insert(inactive_project_id, 2);
    let mut newer_completion = successful_manual_sync_completion(inactive_project_id, "main", 2);
    newer_completion.operation.operation_id = 2;
    app.pending_project_sync_completions
        .insert(inactive_project_id, newer_completion);

    // Act
    app.apply_app_events(successful_manual_sync(inactive_project_id, "main", 1))
        .await;

    // Assert
    assert!(matches!(
        app.pending_project_sync_completions
            .get(&inactive_project_id),
        Some(sync::SyncMainCompletion { operation, .. })
            if operation.operation_id == 2
    ));
}

#[tokio::test]
async fn project_sync_completion_preserves_the_active_navigation_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.active_project_id();
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 0,
    };

    // Act
    app.apply_app_events(successful_manual_sync(project_id, "main", 2))
        .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectSwitcher {
            selected_option_index: 0
        }
    ));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            pulled_commits: Some(2),
            ..
        })
    ));
    assert!(app.project_sync_status_expires_at.is_some());
}

#[tokio::test]
async fn project_sync_terminal_status_expires_at_its_deadline() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.active_project_id();
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.apply_app_events(successful_manual_sync(project_id, "main", 2))
        .await;
    let expires_at = app
        .project_sync_status_expires_at
        .expect("terminal sync status should have an expiry");

    // Act
    app.expire_project_sync_status(
        expires_at
            .checked_sub(Duration::from_millis(1))
            .expect("sync status expiry should be after the monotonic clock origin"),
    );

    // Assert
    assert!(app.project_sync_status.is_some());

    // Act
    app.clear_redraw();
    app.expire_project_sync_status(expires_at);

    // Assert
    assert!(app.project_sync_status.is_none());
    assert!(app.project_sync_status_expires_at.is_none());
    assert!(app.needs_redraw());
}

#[tokio::test]
async fn project_sync_running_status_has_no_expiry() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::test_support::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(1)
        .returning(|_, _, _, _| {});
    app.sync_main_runner = Arc::new(sync_main_runner);

    // Act
    app.start_sync_main();
    let future = app.services.clock().now_instant() + Duration::from_secs(60);
    app.expire_project_sync_status(future);

    // Assert
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Running)
    ));
    assert!(app.project_sync_status_expires_at.is_none());
}

#[tokio::test]
async fn project_sync_completion_applies_captured_review_updates() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.active_project_id();
    let operation = sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 1,
        project_id,
        project_name: "agentty".to_string(),
    };
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: operation.clone(),
        phase: sync::ProjectSyncPhase::Running,
    });
    app.latest_project_sync_operation_ids.insert(project_id, 1);
    let mut completion = successful_manual_sync_completion(project_id, "main", 2);
    completion.review_request_updates = vec![sync::SyncMainReviewUpdate {
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
            summary: None,
        }),
        session_id: "missing-session".into(),
    }];

    // Act
    app.apply_app_events(AppEvent::SyncMainCompleted { completion })
        .await;

    // Assert
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete { .. })
    ));
}

#[tokio::test]
async fn project_sync_blocks_only_base_checkout_operations_for_its_project() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created before sync");
    let project_id = app.active_project_id();
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    let session = app
        .sessions
        .state_mut()
        .session_mut_for_id(&session_id)
        .expect("session should remain loaded");
    session.is_draft = true;
    session.status = Status::Draft;
    insert_test_ready_review(&mut app, &session_id);

    // Act
    let create_result = app.create_session().await;
    let draft_result = app.create_draft_session().await;
    let start_result = app.start_session(&session_id, "continue").await;
    let staged_start_result = app.start_staged_session(&session_id).await;
    let merge_result = app.merge_session(&session_id).await;
    let rebase_result = app.rebase_session(&session_id).await;
    let unrelated_project_result = app.ensure_project_checkout_available(project_id + 1);

    // Assert
    for result in [
        create_result.map(|_| ()),
        draft_result.map(|_| ()),
        start_result,
        staged_start_result,
        merge_result,
        rebase_result,
    ] {
        assert!(matches!(
            result,
            Err(AppError::Workflow(message))
                if message.contains("is synchronizing `main`")
        ));
    }
    assert!(unrelated_project_result.is_ok());
    assert!(app.review_cache.contains_key(session_id.as_str()));
}

#[tokio::test]
async fn project_sync_completion_reconciles_after_switching_back_to_its_project() {
    // Arrange
    let first_project_dir = tempdir().expect("failed to create first project dir");
    let second_project_dir = tempdir().expect("failed to create second project dir");
    let first_project_path = first_project_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&first_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        first_project_path.clone(),
        first_project_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id: first_project_id,
            project_name: "first".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch away from syncing project");

    // Act
    app.apply_app_events(successful_manual_sync(first_project_id, "main", 3))
        .await;
    let was_deferred = app
        .pending_project_sync_completions
        .contains_key(&first_project_id);
    app.switch_project(first_project_id)
        .await
        .expect("failed to switch back to synced project");

    // Assert
    assert!(was_deferred);
    assert!(
        !app.pending_project_sync_completions
            .contains_key(&first_project_id)
    );
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            pulled_commits: Some(3),
            ..
        })
    ));
}

#[tokio::test]
async fn project_sync_queues_other_project_and_coalesces_duplicate_request() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let first_project_id = app.active_project_id();
    let second_project_id = first_project_id + 1;
    let started_project_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_project_ids = Arc::clone(&started_project_ids);
    let mut sync_main_runner = crate::app::test_support::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(2)
        .returning(move |_, operation, _, _| {
            captured_project_ids
                .lock()
                .expect("started project ids lock should remain available")
                .push(operation.project_id);
        });
    app.sync_main_runner = Arc::new(sync_main_runner);

    app.start_sync_main();
    let mut second_project_context = app.sync_handle.context_snapshot();
    second_project_context.project_id = second_project_id;
    second_project_context.project_name = "second-project".to_string();
    app.sync_handle.publish_context(second_project_context);

    // Act
    app.start_sync_main();
    app.start_sync_main();
    let queued_request_count = app.pending_project_sync_requests.len();
    app.apply_app_events(successful_manual_sync(first_project_id, "main", 1))
        .await;

    // Assert
    assert_eq!(queued_request_count, 1);
    assert!(app.pending_project_sync_requests.is_empty());
    assert_eq!(
        *started_project_ids
            .lock()
            .expect("started project ids lock should remain available"),
        vec![first_project_id, second_project_id]
    );
    assert!(matches!(
        app.project_sync_status.as_ref(),
        Some(sync::ProjectSyncStatus {
            context,
            phase: sync::ProjectSyncPhase::Running,
        }) if context.project_id == second_project_id
    ));
}

#[tokio::test]
async fn project_sync_is_blocked_while_merge_work_is_pending() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::test_support::MockSyncMainRunner::new();
    sync_main_runner.expect_start_sync_main().times(0);
    app.sync_main_runner = Arc::new(sync_main_runner);
    app.merge_queue.enqueue("pending-merge".into());

    // Act
    app.start_sync_main();

    // Assert
    assert!(app.merge_queue.is_queued_or_active("pending-merge"));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Blocked { message })
            if message.contains("merge is active or queued")
    ));
    assert!(app.project_sync_status_expires_at.is_some());
}

#[tokio::test]
async fn project_sync_scheduler_keeps_requests_queued_while_slots_are_occupied() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::test_support::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(1)
        .returning(|_, _, _, _| {
            // Keep the first operation running so the queued request can be
            // inspected.
        });
    app.sync_main_runner = Arc::new(sync_main_runner);
    app.start_sync_main();
    let mut second_project_context = app.sync_handle.context_snapshot();
    second_project_context.project_id = app.active_project_id() + 1;
    second_project_context.project_name = "second-project".to_string();
    app.sync_handle.publish_context(second_project_context);
    app.start_sync_main();

    // Act
    app.start_next_project_sync_from_queue();
    app.resume_base_checkout_work().await;
    let queued_during_sync = app.pending_project_sync_requests.len();
    app.project_sync_status = None;
    app.merge_queue.set_active("active-merge".into());
    app.resume_base_checkout_work().await;

    // Assert
    assert_eq!(queued_during_sync, 1);
    assert_eq!(app.pending_project_sync_requests.len(), 1);
    assert!(app.merge_queue.has_active());
}

#[tokio::test]
async fn test_apply_review_request_status_update_merged_restacks_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-merged";
    let child_session_id = "session-child";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3.8-flash",
            "wt/session",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    app.services
        .db()
        .sessions()
        .update_session_prompt(child_session_id, "Ready to start")
        .await
        .expect("failed to stage child prompt");
    app.refresh_sessions_now().await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_main_repo_root()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::OutputParse(
                    "test repository root unavailable".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);

    let update = merged_review_request_status_update(session_id, "#9", "abc1234", "main");

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    let child_before_sync = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child before manual sync");
    let parent_status_before_sync = app
        .sessions
        .session_or_err(session_id)
        .expect("expected parent before manual sync")
        .status;
    let child_parent_before_sync = child_before_sync.parent_session_id.clone();
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;
    app.refresh_sessions_now().await;
    app.sessions
        .load_session_detail_into_state(app.services.db(), child_session_id)
        .await;

    // Assert
    assert_eq!(parent_status_before_sync, Status::Merged);
    assert_eq!(child_parent_before_sync.as_deref(), Some(session_id));
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child session to remain loaded");
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert!(child_session.can_start_staged_session());

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
        .expect("missing persisted child session");
    assert_eq!(db_child_session.parent_session_id, None);
    assert_eq!(db_child_session.base_branch, "main");
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn failed_synchronous_fork_rolls_back_snapshot_and_workspace() {
    // Arrange
    let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let source_id = app.create_session().await.expect("source");
    crate::test_support::set_session_status_for_test(&mut app, &source_id, Status::Review);
    for (kind, text) in [
        (SessionMessageKind::UserPrompt, "Source question"),
        (SessionMessageKind::AssistantAnswer, "Source answer"),
    ] {
        app.services
            .db()
            .sessions()
            .append_session_message(&source_id, kind, text)
            .await
            .expect("source history");
    }
    let original_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&source_id)
        .await
        .expect("source messages");
    let original_resources = session_creation_resources(directory.path()).await;
    sqlx::query(
        "CREATE TRIGGER reject_ready BEFORE UPDATE OF state ON session_preparation WHEN NEW.state \
         = 'ready' BEGIN SELECT RAISE(ABORT, 'preparation rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject after checkout");

    for _attempt in 0..2 {
        // Act
        let result = app.sessions.fork_session(&app.services, &source_id).await;

        // Assert
        assert!(
            result
                .expect_err("fork preparation failure")
                .to_string()
                .contains("preparation rejected")
        );
        let sessions = app
            .services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, source_id);
        let (preparations, messages): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM session_preparation), (SELECT COUNT(*) FROM \
             session_message)",
        )
        .fetch_one(&pool)
        .await
        .expect("remaining metadata");
        assert_eq!(preparations, 1);
        assert_eq!(messages, 2);
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_session_messages(&source_id)
                .await
                .expect("source retained"),
            original_messages
        );
        assert_eq!(
            session_creation_resources(directory.path()).await,
            original_resources
        );
    }

    // Act: a subsequent successful synchronous fork returns a ready snapshot.
    sqlx::query("DROP TRIGGER reject_ready")
        .execute(&pool)
        .await
        .expect("restore setup");
    assert_synchronous_fork_is_ready_with_history(&mut app, &source_id).await;
}

#[tokio::test]
async fn failed_or_unrelated_manual_sync_keeps_session_merged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-waiting";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let update = merged_review_request_status_update(session_id, "#11", "def5678", "main");
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Act
    app.apply_app_events(successful_manual_sync(
        app.active_project_id(),
        "develop",
        0,
    ))
    .await;
    app.apply_app_events(AppEvent::SyncMainCompleted {
        completion: sync::SyncMainCompletion {
            operation: sync::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 2,
                project_id: app.active_project_id(),
                project_name: "agentty".to_string(),
            },
            result: Err(SyncSessionStartError::Other("sync failed".to_string())),
            review_request_updates: Vec::new(),
        },
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected merged session to remain loaded");
    assert_eq!(session.status, Status::Merged);
}

#[tokio::test]
async fn merged_review_waits_for_successful_manual_sync_before_cleanup() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-merged";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let (cleanup_started_tx, mut cleanup_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let cleanup_release = Arc::new(tokio::sync::Notify::new());
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_main_repo_root().times(1).returning({
        let cleanup_release = Arc::clone(&cleanup_release);

        move |_| {
            let cleanup_release = Arc::clone(&cleanup_release);
            let cleanup_started_tx = cleanup_started_tx.clone();

            Box::pin(async move {
                let _ = cleanup_started_tx.send(());
                cleanup_release.notified().await;

                Ok(PathBuf::from("/tmp/repo"))
            })
        }
    });
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);

    let merged_update = merged_review_request_status_update(session_id, "#9", "abc1234", "main");

    // Act
    tokio::time::timeout(
        Duration::from_millis(250),
        app.apply_review_request_status_update(merged_update),
    )
    .await
    .expect("foreground status update should not start worktree cleanup");
    app.process_pending_app_events().await;
    let status_before_sync = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded")
        .status;
    let cleanup_started_before_sync =
        tokio::time::timeout(Duration::from_millis(50), cleanup_started_rx.recv()).await;
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;
    tokio::time::timeout(Duration::from_secs(1), cleanup_started_rx.recv())
        .await
        .expect("cleanup task should start after manual sync")
        .expect("cleanup task should report startup");

    // Assert
    assert_eq!(status_before_sync, Status::Merged);
    assert!(
        cleanup_started_before_sync.is_err(),
        "remote merge detection must not start cleanup"
    );
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded");
    assert_eq!(session.status, Status::Done);
    let merged_commit_hash = app
        .services
        .db()
        .sessions()
        .load_session_merged_commit_hash(session_id)
        .await
        .expect("failed to load merged commit hash")
        .expect("expected persisted merged commit hash");
    assert_eq!(merged_commit_hash, "abc1234");

    // Cleanup
    cleanup_release.notify_one();
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn manual_sync_defers_merged_session_when_commit_hash_cannot_load() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "session-load-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let update = merged_review_request_status_update(session_id, "#12", "abc1234", "main");
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    pool.close().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;

    // Assert
    assert_eq!(
        app.sessions
            .session_or_err(session_id)
            .expect("expected session")
            .status,
        Status::Merged
    );
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            deferred_session_count: 1,
            ..
        })
    ));
}

#[tokio::test]
async fn manual_sync_surfaces_restack_failure_and_keeps_parent_merged() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let project_id = app.active_project_id();
    let session_id = "session-restack-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gemini-3.8-flash",
            "wt/parent",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    app.refresh_sessions_now().await;
    let update = merged_review_request_status_update(session_id, "#13", "abc1234", "main");
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    sqlx::query!(
        "CREATE TRIGGER fail_restack BEFORE UPDATE OF parent_session_id ON session BEGIN SELECT \
         RAISE(FAIL, 'restack failed'); END"
    )
    .execute(&pool)
    .await
    .expect("failed to install restack trigger");

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;

    // Assert
    assert_eq!(
        app.sessions
            .session_or_err(session_id)
            .expect("expected parent session")
            .status,
        Status::Merged
    );
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            deferred_session_count: 1,
            ..
        })
    ));
}

#[tokio::test]
async fn manual_sync_archives_merged_parent_and_merged_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let parent_session_id = "merged-parent";
    let child_session_id = "merged-child";
    insert_review_session_with_data_dir(&app, parent_session_id).await;
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3.8-flash",
            "wt/session-id",
            &Status::Review.to_string(),
            parent_session_id,
            project_id,
        )
        .await
        .expect("failed to insert stacked child session");
    fs::create_dir_all(
        app.services
            .base_path()
            .join(child_session_id.chars().take(8).collect::<String>())
            .join(SESSION_DATA_DIR),
    )
    .expect("failed to create child session data dir");
    app.refresh_sessions_now().await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_main_repo_root()
        .times(2)
        .returning(|_| Box::pin(async { Ok(PathBuf::from("/tmp/repo")) }));
    mock_git_client
        .expect_remove_worktree()
        .times(2)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(2)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);
    let parent_update =
        merged_review_request_status_update(parent_session_id, "#20", "parent-tip", "main");
    let child_update =
        merged_review_request_status_update(child_session_id, "#21", "child-tip", "wt/session-id");
    app.apply_review_request_status_update(parent_update).await;
    app.apply_review_request_status_update(child_update).await;
    app.process_pending_app_events().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;

    // Assert
    let parent_session = app
        .sessions
        .session_or_err(parent_session_id)
        .expect("expected merged parent session");
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected merged child session");
    assert_eq!(parent_session.status, Status::Done);
    assert_eq!(child_session.status, Status::Done);
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn manual_sync_recovers_merged_child_after_parent_was_already_archived() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let child_session_id = "stuck-child";
    insert_review_session_with_data_dir(&app, child_session_id).await;
    app.services
        .db()
        .sessions()
        .update_session_stack_base_commit_hash(child_session_id, Some("parent-tip".to_string()))
        .await
        .expect("failed to persist completed parent restack marker");
    app.refresh_sessions_now().await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_main_repo_root()
        .once()
        .returning(|_| Box::pin(async { Ok(PathBuf::from("/tmp/repo")) }));
    mock_git_client
        .expect_remove_worktree()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .once()
        .returning(|_, _| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);
    let child_update = merged_review_request_status_update(
        child_session_id,
        "#21",
        "child-tip",
        "wt/archived-parent",
    );
    app.apply_review_request_status_update(child_update).await;
    app.process_pending_app_events().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;

    // Assert
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected recovered child session");
    assert_eq!(child_session.status, Status::Done);
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn manual_sync_defers_stranded_child_when_restack_marker_cannot_load() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let child_session_id = "stranded-child";
    insert_review_session_with_data_dir(&app, child_session_id).await;
    app.services
        .db()
        .sessions()
        .update_session_stack_base_commit_hash(child_session_id, Some("parent-tip".to_string()))
        .await
        .expect("failed to persist completed parent restack marker");
    app.refresh_sessions_now().await;
    let child_update = merged_review_request_status_update(
        child_session_id,
        "#22",
        "child-tip",
        "wt/archived-parent",
    );
    app.apply_review_request_status_update(child_update).await;
    app.process_pending_app_events().await;
    pool.close().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;

    // Assert
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected stranded child session");
    assert_eq!(child_session.status, Status::Merged);
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            deferred_session_count: 1,
            ..
        })
    ));
    let workflow_output = app
        .sessions
        .session_handles_or_err(child_session_id)
        .expect("expected stranded child handles")
        .transcript
        .lock()
        .expect("transcript lock poisoned")
        .replay_text()
        .expect("expected durable marker warning");
    assert!(workflow_output.contains("Durable restack marker load failed"));
}

#[tokio::test]
async fn complete_externally_merged_session_reports_invalid_done_transition() {
    // Arrange
    let (mut app, _pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "session-done-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let handles = app
        .sessions
        .session_handles_or_err(session_id)
        .expect("expected session handles");
    *handles.status.lock().expect("status lock poisoned") = Status::Draft;

    // Act
    let warning = app
        .complete_externally_merged_session(session_id, Some("abc1234".to_string()))
        .await
        .expect("invalid transition should produce a warning");

    // Assert
    assert_eq!(warning, "Could not archive the merged session");
    assert_eq!(
        *app.sessions
            .session_handles_or_err(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("status lock poisoned"),
        Status::Draft
    );
}

#[tokio::test]
async fn merge_queue_drain_waits_for_active_project_sync() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id: app.active_project_id(),
            project_name: "test-project".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.merge_queue.enqueue("pending-merge".into());

    // Act
    let result = app.start_next_merge_from_queue(false).await;

    // Assert
    assert!(result.is_ok());
    assert!(app.merge_queue.is_queued_or_active("pending-merge"));
    assert!(!app.merge_queue.has_active());
}

#[tokio::test]
async fn apply_branch_publish_action_update_persists_gitlab_merge_request_notice() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .start_branch_publish("session-1", "Publishing review request...".to_string());
    app.mode = AppMode::List;
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 77,
        summary: crate::domain::session::ReviewRequestSummary {
            display_id: "!24".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "wt/session-1".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Add GitLab support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request,
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
            .is_none()
    );
    let transcript_notice = app.sessions.state().sessions()[0]
        .transcript
        .as_ref()
        .and_then(|transcript| transcript.messages().last())
        .expect("merge request notice should be appended to transcript");
    assert_eq!(transcript_notice.kind, SessionMessageKind::WorkflowNotice);
    assert_eq!(
        transcript_notice.content,
        "\n[Review Request] Created MR \
         https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24\n"
    );
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(persisted_messages[0].content, transcript_notice.content);
}

#[tokio::test]
async fn apply_queued_sync_resolved_retracts_waiting_row() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.queue_session_sync("session-1", 0);

    // Act
    app.apply_app_events(AppEvent::SessionQueuedSyncResolved {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .is_none()
    );
}

#[test]
/// Verifies repeated `AgentResponseReceived` events keep the newest
/// reducer projection while accumulating token usage for the session.
fn app_event_batch_collect_event_merges_agent_response_token_usage() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    let latest_turn = test_turn_applied_state(
        vec![
            QuestionItem::new("Need branch?"),
            QuestionItem::new("Need tests?"),
        ],
        vec!["Document the batched reducer path."],
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 7,
            output_tokens: 11,
        },
    );

    // Act
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("Old question")],
            vec!["Old follow-up task"],
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 3,
                output_tokens: 5,
            },
        ),
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: latest_turn.clone(),
    });

    // Assert
    let merged_turn = event_batch.applied_turns.get("session-1");
    assert_eq!(
        merged_turn.map(|turn| turn.questions.clone()),
        Some(latest_turn.questions)
    );
    assert_eq!(
        merged_turn.map(|turn| {
            turn.follow_up_tasks
                .iter()
                .map(|task| task.text.clone())
                .collect::<Vec<_>>()
        }),
        Some(vec!["Document the batched reducer path.".to_string()])
    );
    assert_eq!(
        merged_turn.map(|turn| turn.token_usage_delta.input_tokens),
        Some(10)
    );
    assert_eq!(
        merged_turn.map(|turn| turn.token_usage_delta.output_tokens),
        Some(16)
    );
}

#[test]
/// Verifies successful sync completion requests an immediate git-status
/// refresh in the reducer batch.
fn app_event_batch_collect_event_marks_successful_sync_for_git_status_refresh() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SyncMainCompleted {
        completion: sync::SyncMainCompletion {
            operation: sync::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id: 1,
                project_name: "agentty".to_string(),
            },
            result: Ok(SyncMainOutcome {
                default_branch: "main".to_string(),
                deferred_merged_session_ids: Vec::new(),
                pulled_commit_titles: vec!["Upstream fix".to_string()],
                pulled_commits: Some(1),
                pushed_commit_titles: vec!["Local tweak".to_string()],
                pushed_commits: Some(2),
                resolved_conflict_files: Vec::new(),
            }),
            review_request_updates: Vec::new(),
        },
    });

    // Assert
    assert!(event_batch.should_refresh_git_status);
    assert!(matches!(
        event_batch.sync_main_completion,
        Some(sync::SyncMainCompletion {
            result: Ok(SyncMainOutcome {
                default_branch,
                pulled_commits: Some(1),
                pushed_commits: Some(2),
                ..
            }),
            ..
        }) if default_branch == "main"
    ));
}

#[tokio::test]
async fn restore_prompt_progress_cleans_attachments_for_merged_session() {
    // Arrange
    let session_id = SessionId::from("merged-session");
    let attachment_path = crate::app::agentty_home()
        .join("tmp")
        .join(session_id.as_str())
        .join("images")
        .join("image-1.png");
    let attachment_directory = attachment_path
        .parent()
        .expect("attachment path should have a parent")
        .to_path_buf();
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_cleanup_agent_artifacts()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    fs_client.expect_is_dir().times(0..).return_const(true);
    fs_client.expect_exists().times(0..).return_const(true);
    fs_client
        .expect_remove_file()
        .once()
        .with(eq(attachment_path.clone()))
        .returning(|_| Box::pin(async { Ok(()) }));
    fs_client
        .expect_remove_dir()
        .once()
        .with(eq(attachment_directory))
        .returning(|_| Box::pin(async { Ok(()) }));
    let mut clients = crate::test_support::test_app_clients();
    clients.fs_client = Arc::new(fs_client);
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(session_id.clone())
            .status(Status::Merged)
            .build(),
    );
    let mut snapshot = test_prompt_mode_snapshot(session_id.clone());
    snapshot.attachment_state.attachments = vec![PromptAttachment::new(1, attachment_path)];
    app.save_prompt_progress(snapshot);

    // Act
    let restored = app.restore_prompt_progress(&session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.is_empty());
}

#[tokio::test]
async fn merge_session_rejects_linked_review_request_before_queueing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let review_request = ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Linked review request".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    };
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .review_request(Some(review_request))
            .build(),
    );

    // Act
    let result = app.merge_session("session-id").await;

    // Assert
    let error = result.expect_err("linked review request should block merge queueing");
    assert_eq!(
        error.to_string(),
        "Merge cannot run for linked review requests or while another stack session is active"
    );
}

#[tokio::test]
async fn apply_app_events_sync_conflicts_updates_non_modal_status() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let operation = sync::ProjectSyncContext {
        default_branch: "develop".to_string(),
        operation_id: 7,
        project_id: app.active_project_id(),
        project_name: "agentty".to_string(),
    };
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: operation.clone(),
        phase: sync::ProjectSyncPhase::Running,
    });
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string(), "README.md".to_string()],
        operation,
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status,
        Some(sync::ProjectSyncStatus {
            phase: sync::ProjectSyncPhase::ResolvingConflicts {
                conflicted_file_count: 2
            },
            ..
        })
    ));
}

#[tokio::test]
async fn apply_app_events_stale_sync_conflict_does_not_replace_live_status() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let live_operation = sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 8,
        project_id: app.active_project_id(),
        project_name: "agentty".to_string(),
    };
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: live_operation.clone(),
        phase: sync::ProjectSyncPhase::Running,
    });
    let mut stale_operation = live_operation;
    stale_operation.operation_id = 7;

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string()],
        operation: stale_operation,
    })
    .await;

    // Assert
    assert!(matches!(
        app.project_sync_status,
        Some(sync::ProjectSyncStatus {
            phase: sync::ProjectSyncPhase::Running,
            ..
        })
    ));
}

#[tokio::test]
async fn apply_app_events_sync_conflict_without_running_status_is_ignored() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let operation = sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 7,
        project_id: app.active_project_id(),
        project_name: "agentty".to_string(),
    };

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string()],
        operation,
    })
    .await;

    // Assert
    assert!(app.project_sync_status.is_none());
}

#[tokio::test]
/// Verifies review-request status updates emitted before a sync
/// completion in the same reducer batch are applied before the
/// post-sync refresh bumps the status generation.
async fn apply_app_events_review_request_status_survives_same_batch_sync_refresh() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-sync-batch";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    let generation = app.sync_handle.current_generation();
    app.services
        .event_sender()
        .send(AppEvent::SyncMainCompleted {
            completion: sync::SyncMainCompletion {
                operation: sync::ProjectSyncContext {
                    default_branch: "main".to_string(),
                    operation_id: 1,
                    project_id,
                    project_name: "agentty".to_string(),
                },
                result: Ok(SyncMainOutcome {
                    default_branch: "main".to_string(),
                    deferred_merged_session_ids: Vec::new(),
                    pulled_commit_titles: Vec::new(),
                    pulled_commits: Some(0),
                    pushed_commit_titles: Vec::new(),
                    pushed_commits: Some(0),
                    resolved_conflict_files: Vec::new(),
                }),
                review_request_updates: Vec::new(),
            },
        })
        .expect("sync completion should queue");

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
/// Verifies stale published-branch sync completions do not overwrite the
/// latest in-progress auto-push state.
async fn apply_app_events_ignores_stale_published_branch_sync_updates() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-branch-sync-view"),
        ));

    // Act
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-1".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-2".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: Some("[Branch Push Error] stale failure".to_string()),
        session_id: "session-1".into(),
        sync_operation_id: "sync-1".to_string(),
        sync_status: PublishedBranchSyncStatus::Failed,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::PublishedBranchSync)
            .map(|message| message.body.text()),
        Some("Auto-pushing published branch after completed turn...")
    );
    assert!(
        app.sessions.sessions()[0]
            .transcript
            .as_ref()
            .is_none_or(|transcript| transcript.messages().is_empty())
    );
}

#[tokio::test]
/// Verifies one reducer tick preserves a completed auto-push message even
/// when start and success updates are drained together.
async fn apply_app_events_preserves_completed_published_branch_sync_updates() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let event_sender = app.services.event_sender();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-branch-sync-success"),
        ));

    event_sender
        .send(AppEvent::PublishedBranchSyncUpdated {
            persistent_notice: Some(
                "[Branch Push] Auto-pushed published branch after completed turn.".to_string(),
            ),
            session_id: "session-1".into(),
            sync_operation_id: "sync-1".to_string(),
            sync_status: PublishedBranchSyncStatus::Succeeded,
        })
        .expect("queued event should send");

    // Act
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-1".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;

    // Assert
    let session = &app.sessions.sessions()[0];
    assert!(
        session
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::PublishedBranchSync)
            .is_none()
    );
    assert_eq!(
        session
            .transcript
            .as_ref()
            .expect("promoted notice should update transcript")
            .messages()
            .iter()
            .filter(|message| {
                message.kind == crate::domain::session_message::SessionMessageKind::WorkflowNotice
            })
            .count(),
        1
    );
}

#[tokio::test]
/// Verifies agent-response events still trigger auto review when the
/// handle has already advanced to `Review` but the paired
/// `SessionUpdated` event has not been reduced yet.
async fn apply_app_events_agent_response_starts_auto_review_from_synced_handle_status() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);

    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-auto-review-sync"),
        ));
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::InProgress),
    );
    *app.sessions
        .session_handles()
        .get(session_id)
        .expect("expected session handles")
        .status
        .lock()
        .expect("expected handle status lock") = Status::Review;

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::AgentReview
    );
}

#[tokio::test]
async fn record_externally_merged_session_reports_persistence_failures() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "session-persist-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    sqlx::query!(
        "CREATE TRIGGER fail_merged_hash BEFORE UPDATE OF merged_commit_hash ON session BEGIN \
         SELECT RAISE(FAIL, 'merged hash failed'); END"
    )
    .execute(&pool)
    .await
    .expect("failed to install merged hash trigger");
    let handles = app
        .sessions
        .session_handles_or_err(session_id)
        .expect("expected session handles");
    *handles.status.lock().expect("status lock poisoned") = Status::Done;

    // Act
    let warning = app
        .record_externally_merged_session(session_id, Some("abc1234".to_string()))
        .await
        .expect("persistence failures should produce a warning");

    // Assert
    assert!(warning.contains("Merged commit hash persistence failed"));
    assert!(warning.contains("Could not mark the merged session read-only"));
    assert_eq!(
        app.sessions
            .session_or_err(session_id)
            .expect("expected session")
            .status,
        Status::Review
    );
}

#[tokio::test]
/// Verifies terminal auto-push notices are persisted while their project
/// snapshot is unloaded, so both outcomes survive a later reload.
async fn apply_published_branch_sync_persists_notices_for_unloaded_project() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Review));
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-success".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.sessions.state_mut().replace_sessions(Vec::new());

    // Act
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: Some(
            "[Branch Push] Auto-pushed published branch after completed turn.".to_string(),
        ),
        session_id: "session-1".into(),
        sync_operation_id: "sync-success".to_string(),
        sync_status: PublishedBranchSyncStatus::Succeeded,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-failure".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: Some("[Branch Push Error] Remote rejected the push.".to_string()),
        session_id: "session-1".into(),
        sync_operation_id: "sync-failure".to_string(),
        sync_status: PublishedBranchSyncStatus::Failed,
    })
    .await;

    // Assert
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 2);
    assert_eq!(
        persisted_messages
            .iter()
            .map(|message| (message.kind.as_str(), message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                SessionMessageKind::WorkflowNotice.as_str(),
                "[Branch Push] Auto-pushed published branch after completed turn.",
            ),
            (
                SessionMessageKind::WorkflowNotice.as_str(),
                "[Branch Push Error] Remote rejected the push.",
            ),
        ]
    );
}
