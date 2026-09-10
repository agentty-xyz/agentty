use std::future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_orchestration::{OrchestrationEvent, OrchestrationEventSink};
use tokio::sync::mpsc;
use tokio::time;

use super::{app_event_label, wait_for_cleanup_task_handles};
use crate::app::AppEvent;
use crate::domain::session::SessionId;

#[tokio::test]
async fn orchestration_notifications_reach_the_app_event_channel() {
    // Arrange
    let (app, _directory) = crate::test_support::new_test_app().await;
    let mut services = app.services;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    services.event_tx = event_tx;
    let session_id = SessionId::from("controller");

    // Act
    services.emit(OrchestrationEvent::RefreshSessions);
    for progress in [Some("Running 2 tasks".to_string()), None] {
        services.emit(OrchestrationEvent::ProgressUpdated {
            progress,
            session_id: session_id.clone(),
        });
    }

    // Assert
    assert_eq!(
        event_rx.try_recv().expect("refresh event"),
        AppEvent::RefreshSessions
    );
    for progress in [Some("Running 2 tasks".to_string()), None] {
        assert_eq!(
            event_rx.try_recv().expect("progress event"),
            AppEvent::SessionOrchestrationProgressUpdated {
                progress,
                session_id: session_id.clone(),
            }
        );
    }
    assert!(event_rx.try_recv().is_err());
}

#[test]
fn app_event_label_names_session_review_comment_snapshot_loads() {
    // Arrange
    let event = AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Err("forge unavailable".to_string()),
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "SessionReviewCommentSnapshotLoaded");
}

#[test]
fn app_event_label_names_diff_preview_loads() {
    // Arrange
    let event = AppEvent::DiffPreviewLoaded {
        path: "README.md".to_string(),
        request_id: 1,
        result: Ok(ag_git::WorktreeFileContent::Missing),
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "DiffPreviewLoaded");
}

#[test]
fn app_event_label_names_session_diff_loads() {
    // Arrange
    let event = AppEvent::SessionDiffLoaded {
        request_id: 1,
        result: Ok("diff".to_string()),
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "SessionDiffLoaded");
}

#[test]
fn app_event_label_names_focused_review_persistence_retries() {
    // Arrange
    let event = AppEvent::FocusedReviewPersistenceRetry {
        retry: crate::app::review::FocusedReviewPersistenceRetry {
            attempt: 1,
            persistence_update: crate::app::review::FocusedReviewPersistence {
                diff_hash: Some(42),
                session_id: "session-id".into(),
                status: crate::domain::review::FocusedReviewStatus::Ready,
                text: Some("review".to_string()),
            },
        },
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "FocusedReviewPersistenceRetry");
}

#[test]
fn app_event_label_names_deferred_auto_review_persistence_retries() {
    // Arrange
    let event = AppEvent::DeferredAutoReviewPersistenceRetry {
        retry: crate::app::session_diff::DeferredAutoReviewPersistenceRetry {
            attempt: 1,
            session_id: "session-id".into(),
        },
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "DeferredAutoReviewPersistenceRetry");
}

#[test]
fn app_event_label_names_session_diff_stats_updates() {
    // Arrange
    let event = AppEvent::SessionDiffStatsUpdated {
        diff_stats: crate::domain::session::SessionDiffStats::Unknown,
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "SessionDiffStatsUpdated");
}

#[test]
fn app_event_label_names_orchestration_progress_updates() {
    // Arrange
    let event = AppEvent::SessionOrchestrationProgressUpdated {
        progress: Some("Working... protocol: running".to_string()),
        session_id: "controller".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "SessionOrchestrationProgressUpdated");
}

#[test]
fn app_event_label_names_branch_publish_starts() {
    // Arrange
    let event = AppEvent::BranchPublishActionStarted {
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "BranchPublishActionStarted");
}

#[test]
fn app_event_label_names_branch_publish_resolutions() {
    // Arrange
    let event = AppEvent::BranchPublishActionResolved {
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "BranchPublishActionResolved");
}

#[test]
fn app_event_label_names_queued_sync_resolutions() {
    // Arrange
    let event = AppEvent::SessionQueuedSyncResolved {
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "SessionQueuedSyncResolved");
}

#[test]
fn app_event_label_names_turn_starts() {
    // Arrange
    let event = AppEvent::SessionTurnStarted {
        session_id: "session-id".into(),
    };

    // Act
    let label = app_event_label(&event);

    // Assert
    assert_eq!(label, "SessionTurnStarted");
}

#[tokio::test]
async fn creation_completion_releases_handles_and_preserves_pending_work() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    app.services.track_session_creation_task(
        "pending".to_string(),
        tokio::spawn(async move {
            release_rx.await.expect("release pending creation");
        }),
    );

    for index in 0..64 {
        let request_id = format!("completed-{index}");
        let services = app.services.clone();
        let (settled_tx, mut settled_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            // Joining must not retain the tracker lock.
            let unlocked = services.creation_task_handles.try_lock().is_ok();
            let _ = settled_tx.send(unlocked);
        });
        app.services
            .track_session_creation_task(request_id.clone(), task);

        // Act
        app.complete_session_creations(vec![(request_id, Err("setup failed".to_string()))])
            .await;

        // Assert
        assert!(
            settled_rx
                .try_recv()
                .expect("task joined before completion")
        );
        let tasks = app.services.creation_task_handles.lock().expect("tracker");
        assert_eq!(tasks.len(), 1);
        assert!(
            !tasks
                .get("pending")
                .expect("pending creation")
                .is_finished()
        );
    }

    release_tx.send(()).expect("release creation");
    app.services.wait_for_cleanup_tasks().await;
    assert!(
        app.services
            .creation_task_handles
            .lock()
            .expect("tracker")
            .is_empty()
    );
}

#[tokio::test]
async fn creation_completion_releases_canceled_tasks_and_tolerates_duplicates() {
    // Arrange
    let (app, _directory) = crate::test_support::new_test_app().await;
    let task = tokio::spawn(future::pending::<()>());
    task.abort();
    app.services
        .track_session_creation_task("canceled".to_string(), task);

    // Act
    app.services.finish_session_creation_task("canceled").await;
    app.services.finish_session_creation_task("canceled").await;

    // Assert
    assert!(
        app.services
            .creation_task_handles
            .lock()
            .expect("tracker")
            .is_empty()
    );
}

#[tokio::test]
async fn shutdown_settles_creation_before_cleanup() {
    // Arrange
    let (app, _directory) = crate::test_support::new_test_app().await;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let creation_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.services.track_session_creation_task(
        "pending".to_string(),
        tokio::spawn({
            let creation_finished = Arc::clone(&creation_finished);
            async move {
                release_rx.await.expect("creation release");
                creation_finished.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }),
    );

    // Act
    let shutdown = app.services.wait_for_cleanup_tasks();
    tokio::pin!(shutdown);
    let completed_before_release = tokio::select! {
        () = &mut shutdown => true,
        () = time::sleep(Duration::from_millis(25)) => false,
    };
    assert!(!completed_before_release, "shutdown abandoned creation");
    release_tx.send(()).expect("release creation");
    shutdown.await;

    // Assert
    assert!(creation_finished.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        app.services
            .creation_task_handles
            .lock()
            .expect("creation tasks")
            .is_empty()
    );
}

#[tokio::test]
async fn shutdown_observes_canceled_creation_task() {
    // Arrange
    let (app, _directory) = crate::test_support::new_test_app().await;
    let task = tokio::spawn(future::pending::<()>());
    task.abort();
    app.services
        .track_session_creation_task("canceled".to_string(), task);

    // Act
    app.services.wait_for_cleanup_tasks().await;

    // Assert
    assert!(
        app.services
            .creation_task_handles
            .lock()
            .expect("creation tasks")
            .is_empty()
    );
}

#[tokio::test]
async fn cleanup_task_wait_cancels_work_after_shared_deadline() {
    // Arrange
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task_handle = tokio::spawn(async move {
        let _ = started_tx.send(());
        future::pending::<()>().await;
    });
    let cleanup_task_handles = Mutex::new(vec![task_handle]);
    started_rx.await.expect("cleanup task should start");

    // Act
    time::timeout(
        Duration::from_secs(1),
        wait_for_cleanup_task_handles(&cleanup_task_handles, Duration::from_millis(25)),
    )
    .await
    .expect("cleanup wait should honor its shared deadline");

    // Assert
    assert!(
        cleanup_task_handles
            .lock()
            .expect("cleanup task mutex should remain available")
            .is_empty()
    );
}
