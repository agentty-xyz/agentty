//! Live diff-stat refresh tests covering which turn events request a refresh,
//! throttling, and the hand-off to the turn's own final refresh.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus, TurnEvent};
use ag_git::{DiffStats, MockGitClient};
use ag_session::test_support as model_fixture;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

use super::{LIVE_REFRESH_INTERVAL, LiveDiffStats};
use crate::app::AppEvent;
use crate::app::graph_activity::GraphActivityRegistry;
use crate::app::session::workflow::post_turn::TurnFinalizerContext;
use crate::domain::session::{SessionDiffStats, SessionSize, Status};
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs::MockFsClient;

/// Returns one activity snapshot of `kind` in `status`.
fn activity(kind: ActivityKind, status: ActivityStatus) -> TurnEvent {
    TurnEvent::Activity(ActivityEvent {
        attempt_id: "attempt".into(),
        exit_code: None,
        id: "call".into(),
        input: None,
        kind,
        name: "Edit".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status,
    })
}

/// Starts a refresher whose refreshes report their start time on the
/// returned receiver.
fn recording_refresher() -> (LiveDiffStats, mpsc::UnboundedReceiver<Instant>) {
    let (refreshed_tx, refreshed_rx) = mpsc::unbounded_channel();
    let refresher = LiveDiffStats::spawn(move || {
        let _ = refreshed_tx.send(Instant::now());

        async {}
    });

    (refresher, refreshed_rx)
}

#[test]
fn only_finished_calls_that_may_edit_files_request_a_refresh() {
    // Arrange
    let editing_kinds = [
        ActivityKind::Command,
        ActivityKind::FileChange,
        ActivityKind::Mcp,
        ActivityKind::Tool,
    ];
    let finished_statuses = [
        ActivityStatus::Completed,
        ActivityStatus::Failed,
        ActivityStatus::Interrupted,
    ];

    // Act, Assert
    for kind in editing_kinds {
        assert!(!LiveDiffStats::may_change_worktree(&activity(
            kind,
            ActivityStatus::Running
        )));
        for status in finished_statuses {
            assert!(LiveDiffStats::may_change_worktree(&activity(kind, status)));
        }
    }
    for kind in [ActivityKind::Skill, ActivityKind::WebSearch] {
        assert!(!LiveDiffStats::may_change_worktree(&activity(
            kind,
            ActivityStatus::Completed
        )));
    }
    assert!(!LiveDiffStats::may_change_worktree(
        &TurnEvent::ThoughtDelta("Editing".into())
    ));
}

#[tokio::test(start_paused = true)]
async fn relay_forwards_every_event_and_refreshes_after_a_finished_edit() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (forward_tx, mut forward_rx) = mpsc::unbounded_channel();
    event_tx
        .send(TurnEvent::ThoughtDelta("Editing".into()))
        .expect("event");
    event_tx
        .send(activity(ActivityKind::Tool, ActivityStatus::Completed))
        .expect("event");
    drop(event_tx);

    // Act
    refresher.relay(event_rx, forward_tx).await;
    let refresh_time = refreshed_rx.recv().await;
    refresher.finish().await;

    // Assert
    assert!(refresh_time.is_some());
    assert!(matches!(
        forward_rx.recv().await,
        Some(TurnEvent::ThoughtDelta(_))
    ));
    assert!(matches!(
        forward_rx.recv().await,
        Some(TurnEvent::Activity(_))
    ));
    assert!(forward_rx.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn refreshes_within_the_interval_coalesce_into_one_trailing_refresh() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher();
    let started = Instant::now();

    // Act
    refresher.trigger.notify_one();
    let first = refreshed_rx.recv().await.expect("first refresh");
    refresher.trigger.notify_one();
    refresher.trigger.notify_one();
    let trailing = refreshed_rx.recv().await.expect("trailing refresh");
    refresher.finish().await;

    // Assert
    assert_eq!(first, started);
    assert_eq!(trailing - first, LIVE_REFRESH_INTERVAL);
    assert!(refreshed_rx.recv().await.is_none());
}

#[tokio::test]
async fn finish_waits_for_an_in_flight_refresh_and_skips_pending_ones() {
    // Arrange
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let refresher = LiveDiffStats::spawn({
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        let completed = Arc::clone(&completed);
        move || {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            let completed = Arc::clone(&completed);

            async move {
                started.notify_one();
                release.notified().await;
                completed.store(true, Ordering::SeqCst);
            }
        }
    });
    refresher.trigger.notify_one();
    started.notified().await;

    // Act
    let finishing = tokio::spawn(refresher.finish());
    tokio::task::yield_now().await;
    let finished_early = finishing.is_finished();
    release.notify_one();
    finishing.await.expect("finish");

    // Assert
    assert!(!finished_early);
    assert!(completed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn finish_without_a_signal_runs_no_refresh() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher();

    // Act
    refresher.finish().await;

    // Assert
    assert!(refreshed_rx.recv().await.is_none());
}

#[tokio::test]
async fn started_refresher_persists_and_projects_the_worktree_diff() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("project");
    db.sessions()
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("session");
    let mut fs_client = MockFsClient::new();
    fs_client.expect_is_dir().times(1).return_const(true);
    let mut git_client = MockGitClient::new();
    git_client
        .expect_diff_stats()
        .times(1)
        .returning(|_, base_branch| {
            assert_eq!(base_branch, "main");

            Box::pin(async {
                Ok(DiffStats {
                    added_lines: 478,
                    deleted_lines: 73,
                    has_changes: true,
                })
            })
        });
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let refresher = LiveDiffStats::start(TurnFinalizerContext {
        app_event_tx,
        clock: Arc::new(RealClock),
        db: db.clone(),
        folder: PathBuf::from("/tmp/session"),
        fs_client: Arc::new(fs_client),
        git_client: Arc::new(git_client),
        graph_activity: GraphActivityRegistry::default(),
        session_id: "session-id".into(),
        session_update_versions: Arc::default(),
        status: Arc::new(Mutex::new(Status::InProgress)),
    });

    // Act
    refresher.trigger.notify_one();
    let event = tokio::time::timeout(Duration::from_secs(5), app_event_rx.recv())
        .await
        .expect("refresh event");
    refresher.finish().await;
    let sessions = db
        .sessions()
        .load_sessions_for_project(project_id)
        .await
        .expect("sessions");

    // Assert
    assert!(matches!(
        event,
        Some(AppEvent::SessionDiffStatsUpdated {
            diff_stats: SessionDiffStats::Known {
                added_lines: 478,
                deleted_lines: 73,
                has_diff: true,
                session_size: SessionSize::Xxl,
            },
            ..
        })
    ));
    assert_eq!(sessions[0].added_lines, 478);
    assert_eq!(sessions[0].size, "XXL");
}
