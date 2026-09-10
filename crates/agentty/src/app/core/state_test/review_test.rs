use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_forge as forge;
use app::branch_publish::{
    BranchPublishTaskContext, BranchPublishTaskFailure, BranchPublishTaskSession,
    push_session_branch, run_branch_publish_action,
};
use app::review::ReviewCacheEntry;
use app::sync;
use tempfile::tempdir;

use super::super::{App, SyncReviewRequestTaskResult};
use super::support::{
    apply_next_session_diff, assert_review_message_reanchored_after_publish, forge_remote,
    insert_test_ready_review, install_mock_git_client, install_mock_review_request_client,
    new_test_app_with_selected_session, persist_selected_session, review_comment_snapshot,
    seed_completed_review_transient_message, seed_materialized_session,
    seed_persisted_review_session, test_loading_review, test_prompt_mode_snapshot,
    test_pushed_branch_result, test_review_request_summary, test_turn_applied_state,
    wait_for_app_condition,
};
use crate::app;
use crate::app::branch_publish::tests::branch_push_failure;
use crate::app::branch_publish::{BranchPublishActionUpdate, BranchPublishTaskSuccess};
use crate::app::core::event::{AppEvent, AppEventBatch, ReviewRequestStatusUpdate};
use crate::app::review::ReviewUpdate;
use crate::app::session;
use crate::app::test_support::diff_content_hash;
use crate::domain::agent::AgentModel;
use crate::domain::session::{
    ForgeKind, PublishBranchAction, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    SESSION_DATA_DIR, SessionHandles, SessionId, SessionRole, SessionStats, Status,
};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::{
    AppMode, ConfirmationViewMode, DiffReviewComments, DiffSidebarFocus,
};
use crate::runtime::mode::diff;

#[tokio::test]
async fn push_session_branch_succeeds_without_review_request_link_for_unsupported_remote() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq(session::session_branch("session-1")),
        )
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-1".to_string()) }));
    mock_git_client
        .expect_repo_url()
        .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
        .once()
        .returning(|_| Box::pin(async { Ok("https://example.com/team/project.git".to_string()) }));
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        None,
    )
    .await;

    // Assert
    assert_eq!(
        result,
        Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: session::session_branch("session-1"),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        })
    );
}

#[tokio::test]
async fn test_switch_project_restores_project_scoped_focused_reviews() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let second_session_id = "second-review";
    let loading_session_id = "loading-review";
    let review_text = "## Review\nSecond project finding.";
    seed_persisted_review_session(
        &database,
        &base_path,
        second_project_id,
        second_session_id,
        "42",
        review_text,
    )
    .await;
    seed_persisted_review_session(
        &database,
        &base_path,
        second_project_id,
        loading_session_id,
        "7",
        "outdated persisted review",
    )
    .await;
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(3)
        .returning(|_| Box::pin(async { None }));
    install_mock_git_client(&mut app, mock_git_client);
    app.review_cache
        .insert(loading_session_id.into(), test_loading_review(21));
    app.deferred_auto_review_session_ids
        .insert(loading_session_id.into());
    insert_test_ready_review(&mut app, "inactive-review");

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert!(matches!(
        app.review_cache.get(second_session_id),
        Some(ReviewCacheEntry::Ready { diff_hash: 42, text }) if text == review_text
    ));
    assert!(matches!(
        app.review_cache.get(loading_session_id),
        Some(ReviewCacheEntry::Loading { diff_hash: 21, .. })
    ));
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert!(!app.review_cache.contains_key("inactive-review"));
    assert_eq!(
        app.sessions
            .session_or_err(second_session_id)
            .expect("second-project session should be loaded")
            .transient_messages
            .get(TransientMessageSlot::Review)
            .map(|message| message.body.text()),
        Some(review_text)
    );
    assert!(matches!(
        app.sessions
            .session_or_err(loading_session_id)
            .expect("loading review session should be loaded")
            .transient_messages
            .get(TransientMessageSlot::Review)
            .map(|message| &message.body),
        Some(TransientMessageBody::Loading(_))
    ));
}

#[tokio::test]
async fn test_switch_project_recovers_persisted_deferred_review_after_restart() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let session_id = "pending-review";
    database
        .sessions()
        .insert_session(
            session_id,
            "gpt-5.6-sol",
            "main",
            "Review",
            second_project_id,
        )
        .await
        .expect("failed to insert pending review session");
    fs::create_dir_all(session::session_folder(&base_path, session_id).join(SESSION_DATA_DIR))
        .expect("failed to create pending review session data dir");
    assert!(
        database
            .sessions()
            .defer_session_focused_review(session_id)
            .await
            .expect("failed to persist deferred review")
    );
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(3)
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(std::future::pending()));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert_eq!(app.pending_session_diff_requests.len(), 1);
}

#[tokio::test]
async fn auto_start_reviews_clears_cache_on_in_progress_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-cache-clear"),
        ));
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.set_review_ready_output(session_id, 789, "old review".to_string());
    let session_ids = HashSet::from([session_id.into()]);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].transient_messages.messages(), []);
}

#[tokio::test]
async fn auto_start_reviews_skips_when_diff_hash_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-hash-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let hash = diff_content_hash(diff_text);
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Ready {
            diff_hash: hash,
            text: "existing review".to_string(),
        },
    );
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Ready { text, .. }) if text == "existing review"
    ));
}

#[tokio::test]
/// Verifies that a review already in `Loading` state with matching diff
/// hash is not re-triggered by a subsequent reducer tick.
async fn auto_start_reviews_skips_when_already_loading_with_same_hash() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-loading-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let hash = diff_content_hash(diff_text);
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(hash));
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert — still Loading, not re-triggered
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == hash
    ));
    // Status remains Review because mark_session_agent_review was not called.
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_skips_when_auto_review_is_suppressed() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-suppressed-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    app.review_cache
        .insert(session_id.to_string().into(), ReviewCacheEntry::Suppressed);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Suppressed)
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_keeps_orchestrator_controller_in_review() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/orchestrator-auto-review-skip",
    ));
    session.role = SessionRole::Orchestrator;
    session.status = Status::Review;
    app.sessions.push_session(session);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_starts_loading_for_review_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-hash-start"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
}

#[tokio::test]
async fn startup_recovery_restarts_incomplete_managed_focused_review() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-recovered-review"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;
    let diff_text = "diff --git a/file.rs b/file.rs\n+recovered line";
    let expected_hash = diff_content_hash(diff_text);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.recover_startup_focused_reviews(vec![session_id.to_string()]);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
}

#[tokio::test]
async fn apply_review_update_stores_success_in_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-cache";
    let review_text = "## Review\nLooks good.";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-cache",
    ));
    session.id = session_id.to_string().into();
    session.status = Status::AgentReview;
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::AgentReview),
    );
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(123));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 123,
            result: Ok(review_text.to_string()),
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Ready { text, diff_hash }) if text == review_text && *diff_hash == 123
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::Review
    );
}

#[tokio::test]
async fn apply_review_update_stores_failure_in_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-fail";
    let error_message = "Review assist failed with exit code 1";
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review-fail"));
    session.id = session_id.to_string().into();
    session.status = Status::AgentReview;
    app.sessions.push_session(session);
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(456));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 456,
            result: Err(error_message.to_string()),
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Failed { error, diff_hash }) if error == error_message && *diff_hash == 456
    ));
}

#[tokio::test]
async fn apply_review_update_ignores_stale_diff_hash() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-stale";
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(999));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 111,
            result: Ok("stale review".to_string()),
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == 999
    ));
}

#[tokio::test]
async fn apply_review_update_keeps_non_agent_review_status_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-progress";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-progress",
    ));
    session.id = session_id.to_string().into();
    session.status = Status::InProgress;
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::InProgress),
    );
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(222));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 222,
            result: Ok("## Review\nBackground review".to_string()),
        },
    );

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::InProgress
    );
}

#[tokio::test]
async fn test_apply_review_request_status_update_ignores_background_errors() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Err("network timeout".to_string()),
        session_id: "session-1".into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_apply_review_request_status_update_persists_summary() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-1";
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

    let summary = test_review_request_summary("#5", ReviewRequestState::Open);
    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Open {
            display_id: "#5".to_string(),
            status_summary: None,
        },
        summary: Some(summary),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    let session = &app.sessions.sessions()[0];
    let review_request = session
        .review_request
        .as_ref()
        .expect("expected linked review request after sync");
    assert_eq!(review_request.summary.display_id, "#5");
}

#[tokio::test]
async fn test_apply_review_request_status_update_closed_cancels_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-closed";
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

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        },
        summary: Some(test_review_request_summary(
            "#7",
            ReviewRequestState::Closed,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
async fn test_apply_review_request_status_update_closed_cancels_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-closed";
    let child_session_id = "session-child";
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
        .expect("failed to insert parent session");
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
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        },
        summary: Some(test_review_request_summary(
            "#7",
            ReviewRequestState::Closed,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let parent_session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected parent session to remain loaded");
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child session to remain loaded");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Canceled);
}

#[tokio::test]
async fn session_git_status_targets_include_active_unpublished_sessions() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    let mut done_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-done"));
    done_session.id = "session-2".into();
    done_session.status = Status::Done;
    app.sessions.push_session(review_session);
    app.sessions.push_session(done_session);

    // Act
    let targets = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/session-".to_string(),
            session_id: "session-1".into(),
        }]
    );
}

#[tokio::test]
async fn manual_branch_publish_waits_for_existing_branch_operation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session"));
    review_session.status = Status::Done;
    app.sessions.push_session(review_session);
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Done));
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err("session-1")
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let branch_publish_context = app
        .branch_publish_task_context("session-1")
        .expect("expected branch-publish context");

    // Act
    let publish_task = tokio::spawn(run_branch_publish_action(
        PublishBranchAction::Push,
        branch_publish_context,
        app.services.db().clone(),
        app.services.clock(),
        app.services.git_client(),
        app.services.review_request_client(),
        None,
    ));
    tokio::task::yield_now().await;
    let waited_for_existing_operation = !publish_task.is_finished();
    drop(existing_operation_guard);
    let result = tokio::time::timeout(Duration::from_secs(1), publish_task)
        .await
        .expect("manual publish should resume after the existing branch operation")
        .expect("manual publish task should not panic");

    // Assert
    assert!(waited_for_existing_operation);
    assert_eq!(
        result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
}

/// Verifies generic and authentication-related branch-push failures map
/// to the correct popup severity and current recovery guidance.
#[test]
fn branch_push_failure_maps_blocked_and_failed_errors() {
    // Arrange
    let auth_error = "Git push failed: fatal: could not read Username for 'https://github.com': \
                      terminal prompts disabled";
    let failed_error = "remote rejected";

    // Act
    let blocked = branch_push_failure(PublishBranchAction::Push, auth_error);
    let failed = branch_push_failure(PublishBranchAction::Push, failed_error);

    // Assert
    assert_eq!(blocked.title, "Branch push blocked");
    assert!(blocked.message.contains("Git push requires authentication"));
    assert!(blocked.message.contains("gh auth login"));
    assert_eq!(failed.title, "Branch push failed");
    assert!(
        failed
            .message
            .contains("Failed to publish session branch: remote rejected")
    );
}

#[tokio::test]
async fn test_switch_immediately_after_response_recovers_in_progress_review() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let session_id = "in-progress-completed-review";
    seed_materialized_session(
        &database,
        &base_path,
        first_project_id,
        session_id,
        Status::Review,
    )
    .await;
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let repositories = database.clone();
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    crate::test_support::set_session_status_for_test(&mut app, session_id, Status::InProgress);
    repositories
        .sessions()
        .update_session_status_with_timing_at(session_id, "InProgress", 0)
        .await
        .expect("failed to persist in-progress status");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(6)
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(std::future::pending()));
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
    let deferred_before_switch = app.deferred_auto_review_session_ids.clone();
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch away from completed session");
    repositories
        .sessions()
        .update_session_status_with_timing_at(session_id, "Review", 1)
        .await
        .expect("failed to persist final review status");
    crate::test_support::set_session_status_for_test(&mut app, session_id, Status::Review);
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: 1,
    })
    .await;
    let pending_before_return = app.pending_session_diff_requests.len();
    let pending_after_status_transition = repositories
        .sessions()
        .load_pending_focused_review_session_ids(first_project_id)
        .await
        .expect("failed to load deferred review after status transition");
    app.switch_project(first_project_id)
        .await
        .expect("failed to restore completed session project");

    // Assert
    assert_eq!(
        deferred_before_switch,
        HashSet::from([SessionId::from(session_id)])
    );
    assert_eq!(pending_after_status_transition, [session_id]);
    assert_eq!(pending_before_return, 1);
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert_eq!(app.pending_session_diff_requests.len(), 1);
}

#[tokio::test]
async fn push_action_still_dispatches_through_background_publish_path() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(5),
        session_id: session_id.clone().into(),
    };
    let pending_git_status_event =
        tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
            .await
            .expect("pending git status event should arrive")
            .expect("app event channel should remain open");
    assert!(matches!(
        pending_git_status_event,
        AppEvent::GitStatusUpdated { .. }
    ));

    // Act
    app.start_publish_branch_action(restore_view, &session_id, PublishBranchAction::Push, None)
        .await;
    let completion_event = tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
        .await
        .expect("background branch publish should complete")
        .expect("app event channel should remain open");
    let publish_label = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .map(|message| message.body.text());

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(5),
        } if viewed_session_id == &session_id
    ));
    assert_eq!(publish_label, Some("Pushing branch..."));
    assert!(matches!(
        completion_event,
        AppEvent::BranchPublishActionCompleted {
            result,
            session_id: completed_session_id,
        } if completed_session_id == session_id
            && matches!(
                *result,
                Err(BranchPublishTaskFailure { ref message, .. })
                    if message == "Session must be in review to push the branch."
            )
    ));
}

#[tokio::test]
async fn open_session_review_comments_requires_link_and_applies_background_snapshot() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = SessionId::from("session-review-comments");
    let session = crate::test_support::SessionFixtureBuilder::new()
        .id(session_id.clone())
        .folder(PathBuf::from("/tmp/session-review-comments"))
        .build();
    app.sessions.push_session(session);

    let missing_session_comments =
        app.start_session_review_comment_load(&SessionId::from("missing-session"));
    let unlinked_session_comments = app.start_session_review_comment_load(&session_id);

    let session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-review-comments".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Review comments".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    });
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    install_mock_git_client(&mut app, mock_git_client);
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(forge_remote()));
    mock_review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot()) }));
    install_mock_review_request_client(&mut app, mock_review_request_client);

    // Act
    diff::tests::support::enter_diff_mode(
        &mut app,
        &session_id,
        "review diff".to_string(),
        None,
        DiffSidebarFocus::Comments,
    );
    wait_for_app_condition(&mut app, |app| {
        matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    is_loading_comments: false,
                    ..
                }),
                ..
            }
        )
    })
    .await;

    // Assert
    assert!(missing_session_comments.is_none());
    assert!(unlinked_session_comments.is_none());
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            ref diff,
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                comment_error: None,
                comment_snapshot: Some(ref snapshot),
                is_loading_comments: false,
                request_id,
                selected_comment_index: 0,
                sidebar_focus: DiffSidebarFocus::Comments,
            }),
            ref session_id,
            scroll_offset: 0,
            ..
        } if selected_comments.is_empty()
            && snapshot == &review_comment_snapshot()
            && diff == "review diff"
            && request_id > 0
            && session_id == "session-review-comments"
    ));
}

#[test]
fn branch_publish_inline_helpers_format_copy() {
    // Arrange / Act
    let loading_label = App::branch_publish_loading_label(PublishBranchAction::Push);
    let success_title = App::branch_publish_success_title(PublishBranchAction::Push);
    let success_message = App::branch_publish_success_message(
        "wt/session-1",
        Some(&crate::app::branch_publish::ReviewRequestCreationInfo {
            forge_kind: forge::ForgeKind::GitHub,
            web_url: Some(
                "https://github.com/org/repo/compare/main...wt%2Fsession-1?expand=1".to_string(),
            ),
        }),
    );
    let fallback_success_message = App::branch_publish_success_message("wt/session-1", None);
    let pull_request_loading_label =
        App::branch_publish_loading_label(PublishBranchAction::PublishPullRequest);
    let pull_request_success_title =
        App::branch_publish_success_title(PublishBranchAction::PublishPullRequest);

    // Assert
    assert_eq!(loading_label, "Pushing branch...");
    assert_eq!(success_title, "Branch pushed");
    assert!(success_message.contains("Pushed session branch `wt/session-1`."));
    assert!(success_message.contains("Open this link to create the pull request"));
    assert!(
        success_message
            .contains("https://github.com/org/repo/compare/main...wt%2Fsession-1?expand=1")
    );
    assert!(fallback_success_message.contains("Create the review request manually"));
    assert_eq!(pull_request_loading_label, "Publishing review request...");
    assert_eq!(pull_request_success_title, "Review request published");
}

#[tokio::test]
async fn branch_publish_task_helpers_reject_unsupported_session_states() {
    // Arrange
    let app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session"));
    review_session.status = Status::Done;
    let done_snapshot = BranchPublishTaskSession::from_session(&review_session);

    // Act
    let push_result = run_branch_publish_action(
        PublishBranchAction::Push,
        BranchPublishTaskContext {
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            session: done_snapshot.clone(),
        },
        app.services.db().clone(),
        app.services.clock(),
        app.services.git_client(),
        app.services.review_request_client(),
        None,
    )
    .await;
    let helper_result = push_session_branch(
        PublishBranchAction::Push,
        &done_snapshot,
        app.services.db().clone(),
        app.services.git_client(),
        None,
    )
    .await;

    // Assert
    assert_eq!(
        push_result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
    assert_eq!(
        helper_result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
}

#[tokio::test]
async fn branch_publish_task_context_targets_stacked_parent_review_source_branch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut parent_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/parent-session"));
    parent_session.id = "parent-session".into();
    parent_session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#12".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "review/parent-session".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Parent review".to_string(),
            web_url: "https://github.com/org/repo/pull/12".to_string(),
        },
    });
    let mut child_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/child-session"));
    child_session.id = "child-session".into();
    child_session.base_branch = session::session_branch("parent-session");
    child_session.parent_session_id = Some("parent-session".into());
    app.sessions.push_session(parent_session);
    app.sessions.push_session(child_session);
    app.sessions
        .session_handles_mut()
        .insert("child-session".into(), SessionHandles::new(Status::Review));

    // Act
    let branch_publish_context = app
        .branch_publish_task_context("child-session")
        .expect("expected branch-publish context");

    // Assert
    assert_eq!(
        branch_publish_context.session.base_branch,
        "review/parent-session"
    );
    let session_lock = &app
        .sessions
        .session_handles_or_err("child-session")
        .expect("expected child session handles")
        .branch_operation_lock;
    assert!(Arc::ptr_eq(
        &branch_publish_context.branch_operation_lock,
        session_lock
    ));
}

#[tokio::test]
async fn branch_publish_task_context_targets_stacked_parent_upstream_branch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut parent_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/parent-session"));
    parent_session.id = "parent-session".into();
    parent_session.published_upstream_ref = Some("origin/review/parent-custom".to_string());
    let mut child_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/child-session"));
    child_session.id = "child-session".into();
    child_session.base_branch = session::session_branch("parent-session");
    child_session.parent_session_id = Some("parent-session".into());
    app.sessions.push_session(parent_session);
    app.sessions.push_session(child_session);
    app.sessions
        .session_handles_mut()
        .insert("child-session".into(), SessionHandles::new(Status::Review));

    // Act
    let branch_publish_context = app
        .branch_publish_task_context("child-session")
        .expect("expected branch-publish context");

    // Assert
    assert_eq!(
        branch_publish_context.session.base_branch,
        "review/parent-custom"
    );
}

#[tokio::test]
async fn rebasing_review_request_action_queues_on_existing_worker() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    app.start_publish_branch_action(
        ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.clone().into(),
        },
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Rebasing);

    // Act
    app.start_publish_branch_action(
        ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: session_id.clone().into(),
        },
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    let publish_body = app.sessions.state().sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::BranchPublish)
        .map(|message| &message.body);

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(4),
        } if viewed_session_id == &session_id
    ));
    assert!(matches!(
        publish_body,
        Some(TransientMessageBody::Queued(action))
            if action.order == 0 && action.text == "review request — publish after this turn"
    ));
    drop(existing_operation_guard);
}

#[tokio::test]
async fn apply_branch_publish_action_persists_result_for_unloaded_project() {
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
    app.sessions.state_mut().replace_sessions(Vec::new());

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
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
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert!(persisted_messages[0].content.contains("Branch pushed"));
    assert!(
        persisted_messages[0]
            .content
            .contains("Pushed session branch `wt/session-1`.")
    );
    {
        let live_transcript = app
            .sessions
            .session_handles()
            .get("session-1")
            .expect("session handles should remain loaded")
            .transcript
            .lock()
            .expect("session transcript lock should succeed");
        assert_eq!(
            live_transcript
                .messages()
                .last()
                .map(|message| message.content.as_str()),
            Some(persisted_messages[0].content.as_str())
        );
    }

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "remote rejected".to_string(),
        )),
        session_id: "session-1".into(),
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
        persisted_messages[1].content,
        "**Review request publish failed**\n\nremote rejected"
    );
}

#[tokio::test]
async fn apply_branch_publish_action_update_persists_pull_request_notice() {
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
    seed_completed_review_transient_message(&mut app);
    app.mode = AppMode::List;
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 55,
        summary: crate::domain::session::ReviewRequestSummary {
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request: review_request.clone(),
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert_review_message_reanchored_after_publish(&app);
    let transcript = app.sessions.state().sessions()[0]
        .transcript
        .as_ref()
        .expect("review request notice should be appended to transcript");
    let transcript_notice = transcript
        .messages()
        .last()
        .expect("review request notice should be present");
    assert_eq!(transcript_notice.kind, SessionMessageKind::WorkflowNotice);
    assert_eq!(
        transcript_notice.content,
        "\n[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42\n"
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
    {
        let handles = app
            .sessions
            .session_handles()
            .get("session-1")
            .expect("session handles should exist");
        let mut live_transcript = handles
            .transcript
            .lock()
            .expect("session transcript lock should succeed");
        live_transcript.append_message(SessionMessageKind::UserPrompt, "continue the session");
    }
    app.sessions
        .state_mut()
        .sync_session_from_handle("session-1");
    let messages_after_new_turn = app.sessions.state().sessions()[0]
        .transcript
        .as_ref()
        .expect("session transcript should remain available")
        .messages();
    assert_eq!(messages_after_new_turn.len(), 2);
    assert_eq!(
        messages_after_new_turn[0].kind,
        SessionMessageKind::WorkflowNotice
    );
    assert_eq!(
        messages_after_new_turn[1].kind,
        SessionMessageKind::UserPrompt
    );
    assert_eq!(
        app.sessions
            .state()
            .sessions()
            .first()
            .and_then(|session| session.review_request.clone()),
        Some(review_request)
    );
}

#[tokio::test]
async fn apply_branch_publish_action_persists_review_request_for_unloaded_project() {
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
    app.sessions.state_mut().replace_sessions(Vec::new());
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 55,
        summary: crate::domain::session::ReviewRequestSummary {
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
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
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].content,
        "\n[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42\n"
    );
}

#[tokio::test]
async fn apply_branch_publish_started_replaces_queued_label() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.queue_branch_publish(
        "session-1",
        0,
        "review request — publish after this turn".to_string(),
    );

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionStarted {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
            .map(|message| message.body.text()),
        Some("Publishing review request...")
    );
}

#[tokio::test]
async fn apply_branch_publish_resolved_retracts_waiting_row() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.queue_branch_publish(
        "session-1",
        0,
        "review request — publish after this turn".to_string(),
    );

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionResolved {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
}

#[tokio::test]
async fn apply_branch_publish_action_update_keeps_active_mode_on_failure() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "remote rejected".to_string(),
        )),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    let publish_message = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .expect("review request publish failure should be visible inline");
    assert_eq!(
        publish_message.body,
        TransientMessageBody::Markdown(
            "**Review request publish failed**\n\nremote rejected".to_string()
        )
    );
}

#[tokio::test]
async fn review_request_enqueue_does_not_wait_for_existing_branch_operation() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let restore_view = ConfirmationViewMode {
        scroll_offset: None,
        session_id: session_id.clone().into(),
    };

    // Act
    let enqueue_result = tokio::time::timeout(
        Duration::from_secs(1),
        app.start_publish_branch_action(
            restore_view,
            &session_id,
            PublishBranchAction::PublishPullRequest,
            None,
        ),
    )
    .await;
    let publish_label = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .map(|message| message.body.text().to_string());
    drop(existing_operation_guard);

    // Assert
    assert!(
        enqueue_result.is_ok(),
        "queueing should not wait for the existing branch operation"
    );
    assert_eq!(
        publish_label.as_deref(),
        Some("Publishing review request...")
    );
}

#[tokio::test]
async fn review_request_enqueue_failure_replaces_queued_status_with_error() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: session_id.clone().into(),
    };

    // Act
    app.start_publish_branch_action(
        restore_view,
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    let publish_body = app
        .sessions
        .state()
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .and_then(|session| {
            session
                .transient_messages
                .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        })
        .map(|message| message.body.text().to_string());

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(3),
        } if viewed_session_id == &session_id
    ));
    assert!(
        publish_body
            .as_deref()
            .is_some_and(|body| body.contains("**Review request publish failed**"))
    );
    assert!(
        publish_body
            .as_deref()
            .is_some_and(|body| body.contains("active session worker is unavailable"))
    );
}

#[test]
fn app_event_batch_collect_event_keeps_publish_results_and_latest_reviews() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 11,
        review_text: "first review".to_string(),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::ReviewPreparationFailed {
        diff_hash: 12,
        error: "latest failure".to_string(),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 21,
        review_text: "stable review".to_string(),
        session_id: "session-b".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(test_pushed_branch_result("feature/first"))),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(test_pushed_branch_result("feature/final"))),
        session_id: "session-b".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionStarted {
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionResolved {
        session_id: "session-b".into(),
    });
    event_batch.collect_event(AppEvent::SessionQueuedSyncResolved {
        session_id: "session-b".into(),
    });

    // Assert
    assert_eq!(
        event_batch.review_updates.get("session-a"),
        Some(&ReviewUpdate {
            diff_hash: 12,
            result: Err("latest failure".to_string()),
        })
    );
    assert_eq!(
        event_batch.review_updates.get("session-b"),
        Some(&ReviewUpdate {
            diff_hash: 21,
            result: Ok("stable review".to_string()),
        })
    );
    assert_eq!(
        event_batch.branch_publish_action_updates,
        vec![
            BranchPublishActionUpdate {
                result: Ok(test_pushed_branch_result("feature/first")),
                session_id: "session-a".into(),
            },
            BranchPublishActionUpdate {
                result: Ok(test_pushed_branch_result("feature/final")),
                session_id: "session-b".into(),
            },
        ]
    );
    assert!(
        event_batch
            .branch_publish_resolved_session_ids
            .contains("session-b")
    );
    assert!(
        event_batch
            .branch_publish_started_session_ids
            .contains("session-a")
    );
    assert!(
        event_batch
            .session_queued_sync_resolved_ids
            .contains("session-b")
    );
    assert!(event_batch.should_refresh_git_status);
}

#[tokio::test]
async fn apply_app_events_branch_publish_action_sets_inline_success() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                forge_kind: forge::ForgeKind::GitHub,
                web_url: Some(
                    "https://github.com/agentty-xyz/agentty/compare/main...wt%2Fsession-1?expand=1"
                        .to_string(),
                ),
            }),
            upstream_reference: "origin/wt/session-1".to_string(),
        })),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    let publish_message = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .expect("branch publish result should be visible inline")
        .body
        .text();
    assert!(publish_message.contains("Branch pushed"));
    assert!(publish_message.contains("Pushed session branch `wt/session-1`."));
    assert!(
        publish_message.contains(
            "https://github.com/agentty-xyz/agentty/compare/main...wt%2Fsession-1?expand=1"
        )
    );
    assert_eq!(
        app.sessions
            .state()
            .sessions()
            .first()
            .and_then(|session| session.published_upstream_ref.as_deref()),
        Some("origin/wt/session-1")
    );
}

#[tokio::test]
/// Verifies stale review-request status results cannot transition a
/// session after the sync context moved to a newer generation.
async fn apply_app_events_review_request_status_updated_ignores_stale_generation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-review-stale"),
        ));
    app.publish_sync_context_for_refresh();
    let stale_generation = app.sync_handle.current_generation().saturating_sub(1);

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation: stale_generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "session-1")
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Review);
}

/// Verifies reducer-applied review-request status transitions update the
/// session state.
#[tokio::test]
async fn apply_app_events_review_request_status_transition_updates_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-status-transition";
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
/// Verifies an unchanged review-ready snapshot does not trigger another full
/// diff when an unrelated handle field emits `SessionUpdated`.
async fn apply_app_events_session_updated_skips_auto_review_without_status_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-update",
    ));
    session.status = Status::Review;
    app.sessions.push_session(session);
    app.sessions
        .session_handles_mut()
        .insert(session_id.into(), SessionHandles::new(Status::Review));
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: 1,
    })
    .await;

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
/// Verifies `SessionUpdated` still triggers automatic review when the synced
/// handle actually transitions into `Review`.
async fn apply_app_events_session_updated_starts_auto_review_on_status_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-transition",
    ));
    session.status = Status::InProgress;
    app.sessions.push_session(session);
    app.sessions
        .session_handles_mut()
        .insert(session_id.into(), SessionHandles::new(Status::Review));
    let diff_call_count = Arc::new(AtomicUsize::new(0));
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().once().returning({
        let diff_call_count = Arc::clone(&diff_call_count);

        move |_, _| {
            diff_call_count.fetch_add(1, Ordering::Relaxed);

            Box::pin(std::future::pending())
        }
    });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: 1,
    })
    .await;
    tokio::task::yield_now().await;

    // Assert
    assert_eq!(diff_call_count.load(Ordering::Relaxed), 1);
    assert_eq!(app.pending_session_diff_requests.len(), 1);
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
/// Verifies a completed turn supersedes an older pending review diff before
/// starting review preparation for the latest generation.
async fn apply_app_events_agent_response_supersedes_pending_auto_review_diff() {
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
            PathBuf::from("/tmp/session-already-review"),
        ));
    // Simulate sync_from_handles() having already updated the snapshot
    // to `Review` in a prior render tick.
    app.sessions.sessions_mut()[0].status = Status::AgentReview;
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Review),
    );
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };

    let diff_call_count = Arc::new(AtomicUsize::new(0));
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(2).returning({
        let diff_call_count = Arc::clone(&diff_call_count);

        move |_, _| {
            let is_obsolete_request = diff_call_count.fetch_add(1, Ordering::Relaxed) == 0;
            Box::pin(async move {
                if is_obsolete_request {
                    std::future::pending::<()>().await;
                }

                Ok(diff_text.to_string())
            })
        }
    });
    install_mock_git_client(&mut app, mock_git_client);
    let session_ids = HashSet::from([SessionId::from(session_id)]);
    app.auto_start_reviews(&session_ids);
    tokio::task::yield_now().await;
    assert_eq!(diff_call_count.load(Ordering::Relaxed), 1);
    let obsolete_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("obsolete review diff should be pending");

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
    let replacement_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("replacement review diff should be pending");
    apply_next_session_diff(&mut app).await;

    // Assert
    assert_ne!(replacement_request_id, obsolete_request_id);
    assert!(
        !app.pending_session_diff_requests
            .contains_key(&obsolete_request_id)
    );
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref mode_session_id,
            ..
        } if mode_session_id == session_id
    ));
}

#[tokio::test]
/// Verifies a viewed session keeps its review state when its live status
/// transition reaches `Done`.
async fn apply_app_events_session_updated_keeps_done_view_review_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-done-view"),
        ));
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(
            Status::Done,
            crate::test_support::assistant_transcript("Merge finished"),
        ),
    );
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: Some(9),
    };

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 1,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(9),
            ..
        }
    ));
}

#[tokio::test]
async fn delete_selected_session_clears_review_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-delete-cache"),
        ));
    app.sessions.select_session_index(Some(0));
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "cached review".to_string(),
        },
    );
    app.save_prompt_progress(test_prompt_mode_snapshot(session_id.clone()));

    // Act
    app.delete_selected_session().await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id.as_str()));
    assert!(!app.prompt_progress.contains_key(session_id.as_str()));
}
