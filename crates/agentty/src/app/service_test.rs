use std::collections::HashMap;
use std::future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_orchestration::{OrchestrationEvent, OrchestrationEventSink};
use tokio::sync::mpsc;
use tokio::time;
use tracing::instrument::WithSubscriber;

use crate::app::branch_publish::BranchPublishTaskFailure;
use crate::app::service::AppServices;
use crate::app::session::{SyncSessionStartError, TurnAppliedState};
use crate::app::sync::{ProjectSyncContext, SyncMainCompletion};
use crate::app::{AppEvent, UpdateStatus};
use crate::domain::session::{PublishedBranchSyncStatus, SessionId, SessionStats};
use crate::test_support::{FixedClock, TestSubscriber};

/// Exercises failure while cancellation drops a task-owned resource.
struct CleanupResource {
    should_succeed: bool,
}

impl Drop for CleanupResource {
    fn drop(&mut self) {
        assert!(self.should_succeed, "injected cleanup failure");
    }
}

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let label = AppServices::app_event_label(&event);

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
    let clock = FixedClock::new(
        time::Instant::now()
            .into_std()
            .checked_sub(Duration::from_secs(2))
            .expect("past deadline"),
        std::time::SystemTime::UNIX_EPOCH,
    );

    // Act
    time::timeout(
        Duration::from_secs(1),
        AppServices::wait_for_cleanup_task_handles(
            &cleanup_task_handles,
            &clock,
            Duration::from_secs(1),
        ),
    )
    .with_subscriber(TestSubscriber)
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

#[tokio::test]
async fn cleanup_wait_settles_successful_and_failed_tasks() {
    // Arrange
    let success = tokio::spawn(async {});
    let failure = tokio::spawn(async {
        drop(CleanupResource {
            should_succeed: false,
        });
    });
    let handles = Mutex::new(vec![success, failure]);
    let clock = FixedClock::unix_epoch();

    // Act
    AppServices::wait_for_cleanup_task_handles(&handles, &clock, Duration::from_secs(1)).await;

    // Assert
    assert!(handles.lock().expect("cleanup handles").is_empty());
}

#[tokio::test]
async fn cleanup_wait_observes_resource_failure_during_cancellation() {
    // Arrange
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _resource = CleanupResource {
            should_succeed: false,
        };
        started_tx.send(()).expect("task readiness");
        future::pending::<()>().await;
    });
    started_rx.await.expect("cleanup task started");
    let handles = Mutex::new(vec![task]);
    let clock = FixedClock::new(
        time::Instant::now()
            .into_std()
            .checked_sub(Duration::from_secs(1))
            .expect("past deadline"),
        std::time::SystemTime::UNIX_EPOCH,
    );

    // Act
    AppServices::wait_for_cleanup_task_handles(&handles, &clock, Duration::ZERO).await;

    // Assert
    assert!(handles.lock().expect("cleanup handles").is_empty());
}

#[test]
fn background_event_labels_preserve_variant_identity() {
    // Arrange
    let session_id = SessionId::from("background-session");
    let operation = ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 1,
        project_id: 1,
        project_name: "example".to_string(),
    };
    let events = [
        AppEvent::AtMentionEntriesLoaded {
            entries: Vec::new(),
            session_id: session_id.clone(),
        },
        AppEvent::GitStatusUpdated {
            generation: 1,
            session_statuses: HashMap::default(),
            status: None,
        },
        AppEvent::VersionAvailabilityUpdated {
            latest_available_version: None,
        },
        AppEvent::AgentCliVersionsUpdated {
            agent_clis: Vec::new(),
        },
        AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: "1.0".to_string(),
            },
        },
        AppEvent::RefreshGitStatus,
        AppEvent::SessionProgressUpdated {
            progress_message: None,
            session_id,
        },
        AppEvent::SyncMainCompleted {
            completion: SyncMainCompletion {
                operation: operation.clone(),
                result: Err(SyncSessionStartError::Other("offline".to_string())),
                review_request_updates: Vec::new(),
            },
        },
        AppEvent::SyncMainConflictResolutionStarted {
            conflicted_files: Vec::new(),
            operation,
        },
    ];

    // Act / Assert
    assert_event_labels(events);
}

#[test]
fn session_event_labels_preserve_variant_identity() {
    // Arrange
    let session_id = SessionId::from("session");
    let events = [
        AppEvent::SessionTitleGenerationFinished {
            generation: 1,
            session_id: session_id.clone(),
        },
        AppEvent::BranchPublishActionCompleted {
            result: Box::new(Err(BranchPublishTaskFailure {
                is_blocked: false,
                message: "offline".to_string(),
                title: "Publish".to_string(),
            })),
            session_id: session_id.clone(),
        },
        AppEvent::ReviewPrepared {
            diff_hash: 1,
            review_text: "Review".to_string(),
            session_id: session_id.clone(),
        },
        AppEvent::ReviewPreparationFailed {
            diff_hash: 1,
            error: "offline".to_string(),
            session_id: session_id.clone(),
        },
        AppEvent::AgentResponseReceived {
            session_id: session_id.clone(),
            turn_applied_state: TurnAppliedState {
                follow_up_tasks: Vec::new(),
                questions: Vec::new(),
                token_usage_delta: SessionStats::default(),
            },
        },
        AppEvent::StackedParentTurnCompleted {
            session_id: session_id.clone(),
        },
        AppEvent::StackedParentSyncCompleted {
            session_id: session_id.clone(),
        },
        AppEvent::StackedParentMergeCompleted {
            child_session_ids: vec![session_id.clone()],
        },
        AppEvent::SessionWorkflowNoticeUpdated {
            notice: "Working".to_string(),
            session_id: session_id.clone(),
        },
        AppEvent::PublishedBranchSyncUpdated {
            persistent_notice: None,
            session_id: session_id.clone(),
            sync_operation_id: "push".to_string(),
            sync_status: PublishedBranchSyncStatus::Succeeded,
        },
        AppEvent::ReviewRequestStatusUpdated {
            generation: 1,
            result: Err("offline".to_string()),
            session_id,
        },
    ];
    // Act / Assert
    assert_event_labels(events);
}

fn assert_event_labels(events: impl IntoIterator<Item = AppEvent>) {
    for event in events {
        let debug = format!("{event:?}");
        let expected = debug.split_whitespace().next().expect("variant identity");

        // Act
        let label = AppServices::app_event_label(&event);

        // Assert
        assert_eq!(label, expected);
    }
}
