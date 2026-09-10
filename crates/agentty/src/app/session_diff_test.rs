use std::collections::HashSet;

use super::{
    DeferredAutoReviewPersistenceRetry, FocusedReviewTarget,
    MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES, PendingSessionDiffRequest, SessionDiffPurpose,
    SessionDiffUpdate,
};
use crate::app::review::ReviewCacheEntry;
use crate::app::{App, review, session};
use crate::domain::input::InputState;
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, Status,
};
use crate::domain::transient_message::TransientMessageSlot;
use crate::infra::db::DbError;
use crate::presentation::app_mode::{
    AppMode, DiffRestoreTarget, DiffSidebarFocus, PromptModeSnapshot,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};

/// Builds one Git-backed review session for diff-request state tests.
async fn review_app() -> (App, tempfile::TempDir, SessionId) {
    let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
    let session_id = SessionId::from(
        app.create_session()
            .await
            .expect("session should be created"),
    );
    app.sessions.sessions_mut()[0].status = Status::Review;

    (app, base_dir, session_id)
}

/// Builds a persisted review session without requiring a real worktree.
async fn review_app_with_mock_backend() -> (App, tempfile::TempDir, SessionId) {
    let (mut app, base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    let mut session =
        crate::test_support::session_fixture_with_folder(base_dir.path().to_path_buf());
    session.status = Status::Review;
    let session_id = session.id.clone();
    app.services
        .db()
        .sessions()
        .insert_session(
            &session_id,
            "gpt-5.6-sol",
            "main",
            "Review",
            app.projects.active_project_id(),
        )
        .await
        .expect("failed to persist review session");
    app.sessions.push_session(session);

    (app, base_dir, session_id)
}

/// Returns the active loading request generation.
fn loading_request_id(app: &App) -> Option<u64> {
    match app.mode {
        AppMode::DiffLoading { request_id, .. } => Some(request_id),
        _ => None,
    }
}

/// Attaches one open review request to the created session.
fn attach_review_request(app: &mut App) {
    app.sessions.sessions_mut()[0].review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-diff".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Session diff".to_string(),
            web_url: "https://example.test/pull/42".to_string(),
        },
    });
}

/// Builds stable review inputs for direct diff-completion tests.
fn test_review_target(app: &App, session_id: &SessionId) -> FocusedReviewTarget {
    let folder = app.sessions.session_for_id(session_id).map_or_else(
        || session::session_folder(app.services.base_path(), session_id.as_str()),
        |session| session.folder.clone(),
    );

    FocusedReviewTarget {
        folder,
        review_agent: app.review_agent(),
    }
}

#[tokio::test]
async fn cancel_diff_view_load_restores_view_and_discards_completion() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    app.mode = AppMode::View {
        scroll_offset: Some(7),
        session_id: session_id.clone(),
    };
    assert_eq!(loading_request_id(&app), None);
    assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
    let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

    // Act
    app.cancel_diff_view_load();
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("stale diff".to_string()),
        session_id: session_id.clone(),
    })
    .await;
    app.cancel_diff_view_load();

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(7),
        } if viewed_session_id == &session_id
    ));
}

#[tokio::test]
async fn stale_open_diff_completion_keeps_newer_loading_generation() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
    let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
    let newer_request_id = request_id.saturating_add(1);
    app.mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: None,
        request_id: newer_request_id,
        restore: None,
        session_id: session_id.clone(),
        sidebar_focus: DiffSidebarFocus::Comments,
    };

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("stale diff".to_string()),
        session_id,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::DiffLoading {
            request_id,
            sidebar_focus: DiffSidebarFocus::Comments,
            ..
        } if request_id == newer_request_id
    ));
}

#[tokio::test]
async fn open_diff_completion_after_mode_change_is_discarded() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
    let request_id = loading_request_id(&app).expect("diff loading mode should have a request");
    app.mode = AppMode::List;

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("stale diff".to_string()),
        session_id,
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(app.pending_session_diff_requests.is_empty());
}

#[tokio::test]
async fn open_diff_completion_preserves_comment_focus() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    attach_review_request(&mut app);
    assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Comments, true,));
    let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("diff --git a/file b/file\n+change".to_string()),
        session_id,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(crate::presentation::app_mode::DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            }),
            ..
        }
    ));
}

#[tokio::test]
async fn open_diff_failure_restores_prompt_snapshot() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    let restore = DiffRestoreTarget::Prompt(PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::default(),
        input: InputState::with_text("preserved draft".to_string()),
        scroll_offset: Some(5),
        session_id: session_id.clone(),
        slash_state: PromptSlashState::default(),
    });
    assert!(app.start_diff_view_load(&session_id, Some(restore), DiffSidebarFocus::Files, false,));
    let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Err("worktree unavailable".to_string()),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            input,
            scroll_offset: Some(5),
            session_id: restored_session_id,
            ..
        } if input.text() == "preserved draft" && restored_session_id == &session_id
    ));
}

#[tokio::test]
async fn mismatched_session_diff_completion_is_ignored() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    assert!(app.start_diff_view_load(&session_id, None, DiffSidebarFocus::Files, false,));
    let request_id = loading_request_id(&app).expect("diff loading mode should have a request");

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("wrong session".to_string()),
        session_id: "other-session".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::DiffLoading { .. }));
    assert!(app.pending_session_diff_requests.contains_key(&request_id));
}

#[tokio::test]
async fn missing_session_cannot_start_diff_requests() {
    // Arrange
    let (mut app, _base_dir, _session_id) = review_app().await;
    let missing_session_id = SessionId::from("missing-session");

    // Act
    let diff_started =
        app.start_diff_view_load(&missing_session_id, None, DiffSidebarFocus::Files, false);
    let apply_started =
        app.start_apply_review_diff_load(&missing_session_id, 1, "suggestion".to_string());
    let review_started = app.start_manual_review_diff_load(&missing_session_id);

    // Assert
    assert!(!diff_started);
    assert!(!apply_started);
    assert!(!review_started);
}

#[tokio::test]
async fn apply_review_diff_request_is_deduplicated_per_session() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;

    // Act
    let first_started =
        app.start_apply_review_diff_load(&session_id, 1, "first suggestion".to_string());
    let second_started =
        app.start_apply_review_diff_load(&session_id, 1, "duplicate suggestion".to_string());

    // Assert
    assert!(first_started);
    assert!(!second_started);
    assert_eq!(app.pending_session_diff_requests.len(), 1);
    assert!(app.pending_session_diff_requests.values().any(|request| {
        request.session_id == session_id
            && matches!(
                &request.purpose,
                SessionDiffPurpose::ApplyFocusedReview { .. }
            )
    }));
}

#[tokio::test]
async fn clearing_review_output_discards_pending_apply_request() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: 1,
            text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
        },
    );
    assert!(app.start_apply_review_diff_load(&session_id, 1, "- Fix the issue.".to_string(),));

    // Act
    app.clear_review_output(&session_id);

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.review_cache.contains_key(&session_id));
}

#[tokio::test]
async fn automatic_review_diff_load_ignores_missing_loaded_session() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let session_ids = HashSet::from([SessionId::from("missing-session")]);

    // Act
    app.start_auto_review_diff_loads(&session_ids);

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(app.review_cache.is_empty());
}

#[tokio::test]
async fn inactive_auto_review_diff_load_accepts_existing_request() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("inactive-review-pending");
    let target = test_review_target(&app, &session_id);
    app.pending_session_diff_requests.insert(
        42,
        PendingSessionDiffRequest {
            purpose: SessionDiffPurpose::Review {
                cached_diff_hash: None,
                is_manual: false,
                target,
            },
            session_id: session_id.clone(),
        },
    );

    // Act
    let accepted = app.start_inactive_auto_review_diff_load(&session_id).await;

    // Assert
    assert!(accepted);
    assert_eq!(app.pending_session_diff_requests.len(), 1);
}

#[tokio::test]
async fn inactive_auto_review_diff_load_rejects_invalid_persisted_metadata() {
    // Arrange
    let (mut app, _base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let project_id = app.projects.active_project_id();
    let invalid_status_id = SessionId::from("inactive-invalid-status");
    let inactive_status_id = SessionId::from("inactive-done-status");
    let missing_project_id = SessionId::from("inactive-missing-project");
    for (session_id, status) in [
        (&invalid_status_id, "Review"),
        (&inactive_status_id, "Done"),
        (&missing_project_id, "Review"),
    ] {
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id.as_str(),
                "gpt-5.6-sol",
                "main",
                status,
                project_id,
            )
            .await
            .expect("failed to insert inactive session fixture");
    }
    sqlx::query("UPDATE session SET status = 'Unknown' WHERE id = ?")
        .bind(invalid_status_id.as_str())
        .execute(&pool)
        .await
        .expect("failed to invalidate inactive session status");
    sqlx::query("UPDATE session SET project_id = NULL WHERE id = ?")
        .bind(missing_project_id.as_str())
        .execute(&pool)
        .await
        .expect("failed to clear inactive session project");

    // Act
    let invalid_status_started = app
        .start_inactive_auto_review_diff_load(&invalid_status_id)
        .await;
    let inactive_status_started = app
        .start_inactive_auto_review_diff_load(&inactive_status_id)
        .await;
    let missing_project_started = app
        .start_inactive_auto_review_diff_load(&missing_project_id)
        .await;

    // Assert
    assert!(!invalid_status_started);
    assert!(!inactive_status_started);
    assert!(!missing_project_started);
    assert!(app.pending_session_diff_requests.is_empty());
}

#[tokio::test]
async fn automatic_review_diff_completion_continues_for_inactive_project() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let repositories = app.services.db().clone();
    let session_id = SessionId::from("inactive-review-diff");
    let inactive_project_path = base_dir.path().join("inactive-project");
    let inactive_project_id = repositories
        .projects()
        .upsert_project(&inactive_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert inactive project");
    repositories
        .sessions()
        .insert_session(
            session_id.as_str(),
            "gpt-5.6-sol",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("failed to insert inactive review session");
    let update = SessionDiffUpdate {
        request_id: 1,
        result: Ok("diff --git a/file b/file".to_string()),
        session_id: session_id.clone(),
    };

    // Act
    let target = test_review_target(&app, &session_id);
    app.apply_review_diff_update(update, None, false, target)
        .await;
    let review_is_loading = matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Loading { .. })
    );
    drop(app);
    let recoverable_session_ids = repositories
        .sessions()
        .load_pending_focused_review_session_ids(inactive_project_id)
        .await
        .expect("failed to recover deferred review after restart");

    // Assert
    assert!(review_is_loading);
    assert_eq!(recoverable_session_ids, [session_id.as_str()]);
}

#[tokio::test]
async fn transient_deferred_auto_review_persistence_failure_retains_and_retries_trigger() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("retry-deferred-review");
    let inactive_project_path = base_dir.path().join("inactive-project");
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&inactive_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert inactive project");
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id.as_str(),
            "gpt-5.6-sol",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("failed to insert inactive review session");
    let (retry_tx, mut retry_rx) = tokio::sync::mpsc::unbounded_channel();
    let retry_scheduled = App::handle_deferred_auto_review_persistence_result(
        &mut app.deferred_auto_review_session_ids,
        retry_tx,
        DeferredAutoReviewPersistenceRetry::initial(session_id.clone()),
        Err(DbError::Query(sqlx::Error::PoolClosed)),
    );

    // Act
    let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), retry_rx.recv())
        .await
        .expect("timed out waiting for deferred review persistence retry")
        .expect("deferred review persistence failure should requeue an event");
    app.apply_app_events(retry_event).await;
    let recoverable_session_ids = app
        .services
        .db()
        .sessions()
        .load_pending_focused_review_session_ids(inactive_project_id)
        .await
        .expect("failed to load retried deferred review");
    app.services
        .db()
        .sessions()
        .update_session_focused_review(session_id.as_str(), None, None, None)
        .await
        .expect("failed to clear retried deferred review");
    app.deferred_auto_review_session_ids.remove(&session_id);
    app.apply_app_events(crate::app::AppEvent::DeferredAutoReviewPersistenceRetry {
        retry: DeferredAutoReviewPersistenceRetry {
            attempt: 2,
            session_id: session_id.clone(),
        },
    })
    .await;
    let stale_retry_session_ids = app
        .services
        .db()
        .sessions()
        .load_pending_focused_review_session_ids(inactive_project_id)
        .await
        .expect("failed to check stale deferred review retry");
    let (exhausted_tx, mut exhausted_rx) = tokio::sync::mpsc::unbounded_channel();
    let exhausted_retry_scheduled = App::handle_deferred_auto_review_persistence_result(
        &mut app.deferred_auto_review_session_ids,
        exhausted_tx,
        DeferredAutoReviewPersistenceRetry {
            attempt: MAX_DEFERRED_AUTO_REVIEW_PERSISTENCE_RETRIES,
            session_id: session_id.clone(),
        },
        Err(DbError::Query(sqlx::Error::PoolClosed)),
    );
    let exhausted_event = exhausted_rx.recv().await;

    // Assert
    assert!(retry_scheduled);
    assert!(!exhausted_retry_scheduled);
    assert_eq!(exhausted_event, None);
    assert!(app.deferred_auto_review_session_ids.contains(&session_id));
    assert_eq!(recoverable_session_ids, [session_id.as_str()]);
    assert_eq!(stale_retry_session_ids, [] as [String; 0]);
}

#[tokio::test]
async fn review_diff_after_new_turn_compares_content_and_allows_manual_review() {
    for (previous_diff, current_diff, is_manual, expects_review) in [
        ("+old line", "+old line", false, false),
        ("+old line", "+new line", false, true),
        ("+old line", "+old line", true, true),
        ("+old line", "", false, false),
        ("", "+old line", false, true),
    ] {
        // Arrange
        let (mut app, _base_dir, session_id) = review_app_with_mock_backend().await;
        app.set_review_ready_output(
            &session_id,
            review::diff_content_hash(previous_diff),
            "Previous review".to_string(),
        );

        // Act
        app.clear_review_output(&session_id);
        app.supersede_review_diff_loads(&HashSet::from([session_id.clone()]));
        let target = test_review_target(&app, &session_id);
        app.apply_review_diff_update(
            SessionDiffUpdate {
                request_id: 1,
                result: Ok(current_diff.to_string()),
                session_id: session_id.clone(),
            },
            None,
            is_manual,
            target,
        )
        .await;

        // Assert
        assert_eq!(app.review_is_loading(&session_id), expects_review);
        assert_eq!(
            app.review_diff_hashes.get(&session_id),
            Some(&review::diff_content_hash(current_diff))
        );
        assert_eq!(
            app.sessions.sessions()[0].status,
            if expects_review {
                Status::AgentReview
            } else {
                Status::Review
            }
        );
        if !expects_review {
            assert_eq!(app.review_view_state(&session_id), (None, None));
            assert_eq!(app.sessions.sessions()[0].transient_messages.messages(), []);
        }
    }
}

#[tokio::test]
async fn unchanged_review_diff_clears_durable_trigger_across_consecutive_turns() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app_with_mock_backend().await;
    let diff = "+existing change";
    let diff_hash = review::diff_content_hash(diff);
    app.set_review_ready_output(&session_id, diff_hash, "Previous review".to_string());

    for request_id in 1..=2 {
        // Act
        app.clear_review_output(&session_id);
        app.defer_auto_review_session(&session_id).await;
        let target = test_review_target(&app, &session_id);
        app.apply_review_diff_update(
            SessionDiffUpdate {
                request_id,
                result: Ok(diff.to_string()),
                session_id: session_id.clone(),
            },
            None,
            false,
            target,
        )
        .await;

        // Assert
        assert!(!app.review_cache.contains_key(&session_id));
        assert!(!app.deferred_auto_review_session_ids.contains(&session_id));
        assert_eq!(app.review_diff_hashes.get(&session_id), Some(&diff_hash));
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_pending_focused_review_session_ids(app.projects.active_project_id())
                .await
                .expect("failed to load pending reviews"),
            [] as [String; 0]
        );
    }
}

#[tokio::test]
async fn restart_during_turn_recovers_review_diff_baseline() {
    for (current_diff, expects_review) in [("+old line", false), ("+new line", true)] {
        // Arrange
        let (mut app, base_dir, session_id) = review_app_with_mock_backend().await;
        std::fs::create_dir_all(session::session_folder(base_dir.path(), &session_id))
            .expect("persisted session worktree directory should exist");
        let diff_hash = review::diff_content_hash("+old line");
        app.prepare_review_diff(&session_id, diff_hash, false, false)
            .await
            .expect("baseline should persist");
        app.set_review_ready_output(&session_id, diff_hash, "Previous review".to_string());
        let repositories = app.services.db().clone();
        repositories
            .sessions()
            .update_session_focused_review(
                &session_id,
                Some(FocusedReviewStatus::Ready),
                Some(diff_hash.to_string()),
                Some("Previous review".to_string()),
            )
            .await
            .expect("previous review should persist");

        // Act: a new turn clears the output, then the application restarts.
        app.clear_review_output(&session_id);
        repositories
            .sessions()
            .update_session_focused_review(&session_id, None, None, None)
            .await
            .expect("new turn should clear review output");
        repositories
            .sessions()
            .update_session_status_with_timing_at(&session_id, "InProgress", 1)
            .await
            .expect("turn should be running before restart");
        drop(app);
        let base_path = base_dir.path().to_path_buf();
        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            repositories,
            crate::test_support::test_app_clients_with_mock_app_server(),
        )
        .await
        .expect("application should restart with persisted sessions");
        assert!(app.review_diff_hashes.is_empty());
        app.sessions
            .state_mut()
            .session_mut_for_id(&session_id)
            .expect("session should survive restart")
            .status = Status::Review;
        let target = test_review_target(&app, &session_id);
        app.apply_review_diff_update(
            SessionDiffUpdate {
                request_id: 1,
                result: Ok(current_diff.to_string()),
                session_id: session_id.clone(),
            },
            None,
            false,
            target,
        )
        .await;

        // Assert
        assert_eq!(app.review_is_loading(&session_id), expects_review);
        assert_eq!(
            app.review_diff_hashes.get(&session_id),
            Some(&review::diff_content_hash(current_diff))
        );
        if !expects_review {
            assert_eq!(app.review_view_state(&session_id), (None, None));
            assert_eq!(app.sessions.sessions()[0].status, Status::Review);
        }
    }
}

#[tokio::test]
async fn restart_after_review_claim_recovers_before_worker_starts() {
    // Arrange
    let (mut app, base_dir, session_id) = review_app_with_mock_backend().await;
    std::fs::create_dir_all(session::session_folder(base_dir.path(), &session_id))
        .expect("persisted session worktree directory should exist");
    app.prepare_review_diff(
        &session_id,
        review::diff_content_hash("+old line"),
        false,
        false,
    )
    .await
    .expect("old baseline should persist");
    let repositories = app.services.db().clone();
    let diff = "+changed line";
    let diff_hash = review::diff_content_hash(diff);

    // Act: stop immediately after preparation, before spawning the worker.
    assert!(
        app.prepare_review_diff(&session_id, diff_hash, true, false)
            .await
            .expect("new baseline and claim should commit")
    );
    assert!(!app.review_is_loading(&session_id));
    drop(app);
    let base_path = base_dir.path().to_path_buf();
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        repositories,
        crate::test_support::test_app_clients_with_mock_app_server(),
    )
    .await
    .expect("application should restart");
    let pending = app
        .services
        .db()
        .sessions()
        .load_pending_focused_review_session_ids(app.projects.active_project_id())
        .await
        .expect("startup trigger should persist");
    let request_id = app
        .pending_session_diff_requests
        .iter()
        .find_map(|(request_id, request)| (request.session_id == session_id).then_some(*request_id))
        .expect("startup should schedule review diff recovery");
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok(diff.to_string()),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert_eq!(pending, vec![session_id.to_string()]);
    assert!(app.review_is_loading(&session_id));
    assert_eq!(app.review_diff_hashes.get(&session_id), Some(&diff_hash));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
}

#[tokio::test]
async fn review_diff_baseline_tolerates_invalid_hash_and_database_failure() {
    // Arrange
    let base_dir = tempfile::tempdir().expect("temporary directory should exist");
    let database = crate::infra::db::Database::open_in_memory()
        .await
        .expect("database should open");
    let base_path = base_dir.path().to_path_buf();
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database.clone(),
        crate::test_support::test_app_clients_with_mock_app_server(),
    )
    .await
    .expect("application should start");
    let session_id = SessionId::from("baseline-failure");
    database
        .sessions()
        .insert_session(
            &session_id,
            "gpt-5.6-sol",
            "main",
            "Review",
            app.projects.active_project_id(),
        )
        .await
        .expect("session should persist");
    database
        .sessions()
        .update_session_review_diff_hash(&session_id, "invalid", false)
        .await
        .expect("baseline should persist");

    let mut session =
        crate::test_support::session_fixture_with_folder(base_dir.path().to_path_buf());
    session.id = session_id.clone();
    session.status = Status::Review;
    app.sessions.push_session(session);

    // Act
    let invalid_previous = app.prepare_review_diff(&session_id, 42, true, false).await;
    database.pool().close().await;
    let target = test_review_target(&app, &session_id);
    app.apply_review_diff_update(
        SessionDiffUpdate {
            request_id: 1,
            result: Ok("+new line".to_string()),
            session_id: session_id.clone(),
        },
        None,
        false,
        target,
    )
    .await;

    // Assert
    assert!(invalid_previous.expect("invalid baseline should allow review"));
    assert_eq!(app.review_diff_hashes.get(&session_id), Some(&42));
    assert!(!app.review_is_loading(&session_id));
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Failed { .. })
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn automatic_empty_review_diff_clears_durable_trigger() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(session_id.as_str(), "Review", 1)
        .await
        .expect("failed to persist review status");
    assert!(
        app.services
            .db()
            .sessions()
            .defer_session_focused_review(session_id.as_str())
            .await
            .expect("failed to persist deferred review")
    );
    let project_id = app.projects.active_project_id();
    let update = SessionDiffUpdate {
        request_id: 1,
        result: Ok(String::new()),
        session_id: session_id.clone(),
    };

    // Act
    let target = test_review_target(&app, &session_id);
    app.apply_review_diff_update(update, None, false, target)
        .await;

    // Assert
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_pending_focused_review_session_ids(project_id)
            .await
            .expect("failed to load pending reviews"),
        [] as [String; 0]
    );
    assert!(!app.review_cache.contains_key(&session_id));
}

#[tokio::test]
async fn automatic_empty_review_diff_preserves_cached_output() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    let cached_diff_hash = 42;
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: cached_diff_hash,
            text: "## Review\nExisting finding.".to_string(),
        },
    );
    let update = SessionDiffUpdate {
        request_id: 1,
        result: Ok(String::new()),
        session_id: session_id.clone(),
    };
    let target = test_review_target(&app, &session_id);

    // Act
    app.apply_review_diff_update(update, Some(cached_diff_hash), false, target)
        .await;

    // Assert
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Ready { diff_hash, text })
            if *diff_hash == cached_diff_hash && text.contains("Existing finding")
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn deleting_session_discards_pending_review_diff_and_late_completion() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    app.review_diff_hashes.insert(session_id.clone(), 42);
    let request_id = 42;
    app.pending_session_diff_requests.insert(
        request_id,
        PendingSessionDiffRequest {
            purpose: SessionDiffPurpose::Review {
                cached_diff_hash: None,
                is_manual: false,
                target: test_review_target(&app, &session_id),
            },
            session_id: session_id.clone(),
        },
    );
    app.deferred_auto_review_session_ids
        .insert(session_id.clone());

    // Act
    app.delete_selected_session().await;
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("late diff".to_string()),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.deferred_auto_review_session_ids.contains(&session_id));
    assert!(!app.review_diff_hashes.contains_key(&session_id));
}

#[tokio::test]
async fn deferred_cleanup_deletion_discards_pending_review_diff_state() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    app.pending_session_diff_requests.insert(
        42,
        PendingSessionDiffRequest {
            purpose: SessionDiffPurpose::Review {
                cached_diff_hash: None,
                is_manual: false,
                target: test_review_target(&app, &session_id),
            },
            session_id: session_id.clone(),
        },
    );
    app.deferred_auto_review_session_ids
        .insert(session_id.clone());

    // Act
    app.delete_selected_session_deferred_cleanup().await;

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.deferred_auto_review_session_ids.contains(&session_id));
}

#[tokio::test]
async fn apply_completion_ignores_replaced_review_generation() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let current_diff = String::new();
    let diff_hash = review::diff_content_hash(&current_diff);
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash,
            text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
        },
    );
    assert!(app.start_apply_review_diff_load(
        &session_id,
        diff_hash,
        "- Fix the issue.".to_string(),
    ));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("apply diff request should be pending");
    let review_agent = app.review_agent();
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Loading {
            diff_hash,
            review_agent,
        },
    );

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok(current_diff),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Loading {
            diff_hash: cached_hash,
            ..
        })
            if *cached_hash == diff_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn apply_completion_ignores_session_that_left_review() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let current_diff = String::new();
    let diff_hash = review::diff_content_hash(&current_diff);
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash,
            text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
        },
    );
    assert!(app.start_apply_review_diff_load(
        &session_id,
        diff_hash,
        "- Fix the issue.".to_string(),
    ));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("apply diff request should be pending");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok(current_diff),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Ready {
            diff_hash: cached_hash,
            ..
        }) if *cached_hash == diff_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
}

#[tokio::test]
async fn manual_apply_completion_enqueues_remediation_turn() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    let current_diff = String::new();
    let diff_hash = review::diff_content_hash(&current_diff);
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash,
            text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
        },
    );
    assert!(app.start_apply_review_diff_load(
        &session_id,
        diff_hash,
        "- Fix the issue.".to_string(),
    ));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("manual apply diff request should be pending");

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok(current_diff),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(!app.auto_address_review_iterations.contains_key(&session_id));
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.review_cache.contains_key(&session_id));
}

#[tokio::test]
async fn automatic_apply_completion_counts_enqueued_remediation_turn() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    app.sessions.sessions_mut()[0].permission_mode =
        crate::domain::permission::PermissionMode::AutoEditAddressComments;
    let current_diff = String::new();
    let diff_hash = review::diff_content_hash(&current_diff);
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash,
            text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
        },
    );
    assert!(app.start_auto_apply_review_diff_load(
        &session_id,
        diff_hash,
        "- Fix the issue.".to_string(),
    ));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("automatic apply diff request should be pending");

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok(current_diff),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert_eq!(
        app.auto_address_review_iterations.get(&session_id),
        Some(&1)
    );
    assert!(app.pending_session_diff_requests.is_empty());
}

#[tokio::test]
async fn automatic_apply_completion_does_not_count_failed_remediation_enqueue() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
    app.sessions.sessions_mut()[0].permission_mode =
        crate::domain::permission::PermissionMode::AutoEditAddressComments;
    app.auto_address_review_iterations
        .insert(session_id.clone(), 2);
    let current_diff = String::new();
    let diff_hash = review::diff_content_hash(&current_diff);
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash,
            text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
        },
    );
    assert!(app.start_auto_apply_review_diff_load(
        &session_id,
        diff_hash,
        "- Fix the issue.".to_string(),
    ));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("automatic apply diff request should be pending");
    app.sessions.session_handles_mut().remove(&session_id);

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok(current_diff),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert_eq!(
        app.auto_address_review_iterations.get(&session_id),
        Some(&2)
    );
    assert!(app.pending_session_diff_requests.is_empty());
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Ready { diff_hash: cached_diff_hash, text })
            if *cached_diff_hash == diff_hash && text.contains("Fix the issue")
    ));
    let visible_review_text = app.sessions.sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::Review)
        .map(|message| message.body.text());
    assert!(visible_review_text.is_some_and(|text| text.contains("Fix the issue")));
    let persisted_reviews = app
        .services
        .db()
        .sessions()
        .load_session_focused_reviews_for_project(app.active_project_id())
        .await
        .expect("failed to load restored focused review");
    assert!(persisted_reviews.iter().any(|review| {
        review.session_id == session_id.as_str()
            && review.diff_hash == diff_hash.to_string()
            && review.text.contains("Fix the issue")
    }));
}

#[tokio::test]
async fn automatic_apply_completion_revalidates_mode_and_iteration_limit() {
    // Arrange, Act, Assert
    for (permission_mode, completed_iterations) in [
        (crate::domain::permission::PermissionMode::AutoEdit, 0),
        (
            crate::domain::permission::PermissionMode::AutoEditAddressComments,
            crate::app::prompt_intent::MAX_AUTO_ADDRESS_REVIEW_ITERATIONS,
        ),
    ] {
        let (mut app, _base_dir, session_id) = review_app().await;
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Review);
        app.sessions.sessions_mut()[0].permission_mode = permission_mode;
        app.auto_address_review_iterations
            .insert(session_id.clone(), completed_iterations);
        let current_diff = String::new();
        let diff_hash = review::diff_content_hash(&current_diff);
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash,
                text: "## Review\n### Suggestions\n- Fix the issue.".to_string(),
            },
        );
        assert!(app.start_auto_apply_review_diff_load(
            &session_id,
            diff_hash,
            "- Fix the issue.".to_string(),
        ));
        let request_id = *app
            .pending_session_diff_requests
            .keys()
            .next()
            .expect("automatic apply diff request should be pending");

        app.apply_session_diff_update(SessionDiffUpdate {
            request_id,
            result: Ok(current_diff),
            session_id: session_id.clone(),
        })
        .await;

        assert_eq!(
            app.auto_address_review_iterations.get(&session_id),
            Some(&completed_iterations)
        );
        assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    }
}

#[tokio::test]
async fn review_diff_request_is_deduplicated_and_cleared_after_status_change() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;

    // Act
    let first_started = app.start_manual_review_diff_load(&session_id);
    let second_started = app.start_manual_review_diff_load(&session_id);
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("review diff request should be pending");
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("diff".to_string()),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(first_started);
    assert!(!second_started);
    assert!(!app.review_cache.contains_key(&session_id));
}

#[tokio::test]
async fn superseding_review_turn_discards_review_action_requests() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    assert!(app.start_manual_review_diff_load(&session_id));
    assert!(app.start_apply_review_diff_load(&session_id, 1, "suggestion".to_string()));
    let completed_sessions = HashSet::from([session_id.clone()]);

    // Act
    app.supersede_review_diff_loads(&completed_sessions);

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.review_cache.contains_key(&session_id));
}

#[tokio::test]
async fn review_completion_for_removed_session_clears_loading_cache() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    assert!(app.start_manual_review_diff_load(&session_id));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("review diff request should be pending");
    app.sessions.remove_session_at(0);

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Ok("diff".to_string()),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(!app.review_cache.contains_key(&session_id));
}

#[tokio::test]
async fn review_diff_failure_restores_review_status() {
    // Arrange
    let (mut app, _base_dir, session_id) = review_app().await;
    assert!(app.start_manual_review_diff_load(&session_id));
    let request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("review diff request should be pending");

    // Act
    app.apply_session_diff_update(SessionDiffUpdate {
        request_id,
        result: Err("Failed to run git diff: unavailable".to_string()),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Failed { error, .. }) if error.contains("unavailable")
    ));
}
