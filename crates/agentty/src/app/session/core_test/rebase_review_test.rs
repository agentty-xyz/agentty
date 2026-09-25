//! Review retention at the rebase admission and execution boundaries.

use ag_git::{self as git, MockGitClient};
use sqlx::SqlitePool;
use tempfile::tempdir;
use uuid::Uuid;

use super::support::{
    install_mock_git_client, new_test_app_with_git_and_db, session_status_or_done,
    setup_mock_commit_and_branch_expectations, setup_mock_worktree_expectations,
    wait_for_output_contains, wait_for_status,
};
use crate::app::{App, AppEvent};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::Status;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn rejected_rebase_admission_preserves_completed_and_partial_reviews() {
    for review_status in [FocusedReviewStatus::Ready, FocusedReviewStatus::Partial] {
        for (status, expected_error) in [
            (Status::Done, "must be in review"),
            (Status::AgentReview, "no other stack session is active"),
            (Status::InProgress, "active session worker is unavailable"),
            (Status::Review, "operation admission failed"),
        ] {
            // Arrange
            let directory = tempdir().expect("directory");
            let (db, pool) = AppRepositories::in_memory_with_pool()
                .await
                .expect("database");
            let mut app = new_test_app_with_git_and_db(directory.path(), db).await;
            let id = app.create_session().await.expect("session");
            crate::test_support::set_session_status_for_test(&mut app, &id, status);
            let (request_id, text) = seed_review(&mut app, &id, review_status).await;
            if status == Status::Review {
                reject_operation_admission(&pool).await;
            } else if status == Status::AgentReview {
                let mut child = crate::test_support::SessionFixtureBuilder::new()
                    .id("active-child")
                    .status(Status::InProgress)
                    .build();
                child.parent_session_id = Some(id.as_str().into());
                app.sessions.push_session(child);
            }

            // Act
            let error = app
                .rebase_session(&id)
                .await
                .expect_err("rejected admission");

            // Assert
            assert!(error.to_string().contains(expected_error), "{error}");
            assert_eq!(app.review_view_state(&id).1, Some(text.as_str()));
            assert_eq!(
                app.review_cache
                    .get(id.as_str())
                    .expect("review cache")
                    .request_id(),
                Some(request_id)
            );
            assert_eq!(session_status_or_done(&app, &id), status);
            assert_durable_review_retained(&app, &id, &text, request_id).await;
        }
    }
}

#[tokio::test]
async fn conflict_free_rebase_preserves_completed_and_partial_reviews() {
    for review_status in [FocusedReviewStatus::Ready, FocusedReviewStatus::Partial] {
        // Arrange
        let directory = tempdir().expect("directory");
        let db = AppRepositories::in_memory().await.expect("database");
        let mut app = new_test_app_with_git_and_db(directory.path(), db).await;
        let id = app.create_session().await.expect("session");
        crate::test_support::set_session_status_for_test(&mut app, &id, Status::Review);
        let (request_id, text) = seed_review(&mut app, &id, review_status).await;

        // Act
        app.rebase_session(&id).await.expect("accepted sync");
        wait_for_output_contains(&mut app, &id, "[Sync] Successfully synced", 200).await;

        // Assert
        assert_eq!(app.review_view_state(&id).1, Some(text.as_str()));
        assert_durable_review_retained(&app, &id, &text, request_id).await;
    }
}

#[tokio::test]
async fn rebase_conflict_invalidation_failure_stops_before_assistance() {
    // Arrange
    let directory = tempdir().expect("directory");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("database");
    let mut app = new_test_app_with_git_and_db(directory.path(), db).await;
    let id = app.create_session().await.expect("session");
    crate::test_support::set_session_status_for_test(&mut app, &id, Status::Review);
    let (request_id, text) = seed_review(&mut app, &id, FocusedReviewStatus::Partial).await;
    sqlx::query(
        "CREATE TRIGGER reject_review_invalidation BEFORE DELETE ON session_review_generation \
         BEGIN SELECT RAISE(ABORT, 'review invalidation failed'); END",
    )
    .execute(&pool)
    .await
    .expect("invalidation failure fixture");
    let mut git = MockGitClient::new();
    setup_mock_worktree_expectations(&mut git, directory.path().to_path_buf());
    setup_mock_commit_and_branch_expectations(&mut git);
    git.expect_is_rebase_in_progress()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    git.expect_rebase_start().times(1).returning(|_, _| {
        Box::pin(async {
            Ok(git::RebaseStepResult::Conflict {
                detail: "content conflict".to_string(),
            })
        })
    });
    git.expect_list_conflicted_files().never();
    git.expect_abort_rebase()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, git);

    // Act: admission succeeds, but the worker cannot invalidate evidence.
    app.rebase_session(&id).await.expect("accepted sync");
    wait_for_output_contains(&mut app, &id, "review invalidation failed", 200).await;
    wait_for_status(&mut app, &id, Status::Review).await;

    // Assert: transaction rollback preserves evidence and assistance never
    // runs.
    assert_durable_review_retained(&app, &id, &text, request_id).await;
    assert_eq!(app.review_view_state(&id).1, Some(text.as_str()));
    assert_eq!(session_status_or_done(&app, &id), Status::Review);
}

#[tokio::test]
async fn rebase_conflict_invalidates_display_and_durable_review() {
    // Arrange
    let directory = tempdir().expect("directory");
    let db = AppRepositories::in_memory().await.expect("database");
    let mut app = new_test_app_with_git_and_db(directory.path(), db).await;
    let id = app.create_session().await.expect("session");
    crate::test_support::set_session_status_for_test(&mut app, &id, Status::Review);
    let (request_id, _) = seed_review(&mut app, &id, FocusedReviewStatus::Partial).await;
    let mut git = MockGitClient::new();
    setup_mock_worktree_expectations(&mut git, directory.path().to_path_buf());
    setup_mock_commit_and_branch_expectations(&mut git);
    git.expect_is_rebase_in_progress()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    git.expect_rebase_start().times(1).returning(|_, _| {
        Box::pin(async {
            Ok(git::RebaseStepResult::Conflict {
                detail: "content conflict".to_string(),
            })
        })
    });
    git.expect_list_conflicted_files().times(1).returning(|_| {
        Box::pin(async { Err(git::GitError::OutputParse("assist unavailable".to_string())) })
    });
    git.expect_abort_rebase()
        .times(2)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, git);

    // Act
    app.rebase_session(&id).await.expect("accepted sync");
    wait_for_output_contains(&mut app, &id, "assist unavailable", 200).await;
    app.apply_app_events(AppEvent::ReviewPrepared {
        diff_hash: 42,
        review_text: "stale focused review".to_string(),
        session_id: id.clone().into(),
        request_id,
    })
    .await;

    // Assert
    assert!(!app.review_cache.contains_key(id.as_str()));
    assert_eq!(app.review_view_state(&id).1, None);
    let persisted_reviews = app
        .services
        .db()
        .sessions()
        .load_session_focused_reviews_for_project(app.active_project_id())
        .await
        .expect("persisted reviews");
    assert_eq!(persisted_reviews.first().map(|review| &review.text), None);
    assert!(
        app.services
            .db()
            .sessions()
            .load_review_fragment(&id, "same-inputs", "batch")
            .await
            .expect("checkpoint")
            .is_none()
    );
}

/// Seeds a displayed review with durable output and resumable evidence.
async fn seed_review(app: &mut App, id: &str, status: FocusedReviewStatus) -> (Uuid, String) {
    let text = if status == FocusedReviewStatus::Partial {
        "Retained finding.\n\nPartial review: interrupted"
    } else {
        "Retained completed review."
    }
    .to_string();
    let request_id = app.set_review_ready_output(id, 42, text.clone());
    let sessions = app.services.db().sessions();
    sessions
        .update_session_focused_review(id, Some(status), Some("42".into()), Some(text.clone()))
        .await
        .expect("saved review");
    sessions
        .begin_review_generation(id, "same-inputs", &request_id.to_string())
        .await
        .expect("active generation");
    sessions
        .save_review_fragment(
            id,
            "same-inputs",
            &request_id.to_string(),
            "batch",
            "evidence",
        )
        .await
        .expect("checkpoint");

    (request_id, text)
}

/// Rejects operation admission after validation and the status transition.
async fn reject_operation_admission(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TRIGGER reject_rebase_operation BEFORE INSERT ON session_operation BEGIN SELECT \
         RAISE(ABORT, 'operation admission failed'); END",
    )
    .execute(pool)
    .await
    .expect("admission failure fixture");
}

/// Checks persisted output and evidence independently of the display cache.
async fn assert_durable_review_retained(app: &App, id: &str, text: &str, request_id: Uuid) {
    let sessions = app.services.db().sessions();
    let reviews = sessions
        .load_session_focused_reviews_for_project(app.active_project_id())
        .await
        .expect("persisted reviews");
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].text, text);
    assert_eq!(
        sessions
            .load_review_fragment(id, "same-inputs", "batch")
            .await
            .expect("checkpoint"),
        Some("evidence".into())
    );
    sessions
        .save_review_fragment(
            id,
            "same-inputs",
            &request_id.to_string(),
            "batch",
            "later evidence",
        )
        .await
        .expect("generation remains active");
    assert_eq!(
        sessions
            .load_review_fragment(id, "same-inputs", "batch")
            .await
            .expect("updated checkpoint"),
        Some("later evidence".into())
    );
}
