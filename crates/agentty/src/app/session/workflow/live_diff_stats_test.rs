//! Live diff-stat refresh tests covering which turn events request a refresh,
//! throttling, and the pending refresh that `finish` runs before post-turn
//! effects.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus, TurnEvent};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

use super::{LIVE_REFRESH_INTERVAL, LiveDiffStats};
use crate::infra::clock::Clock;

/// Clock that follows Tokio's clock, so paused-time tests can measure how
/// long a refresh took.
struct TokioClock;

impl Clock for TokioClock {
    fn now_instant(&self) -> std::time::Instant {
        Instant::now().into_std()
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

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

/// Starts a refresher whose refreshes take `duration` and report their start
/// time on the returned receiver.
fn recording_refresher(duration: Duration) -> (LiveDiffStats, mpsc::UnboundedReceiver<Instant>) {
    let (refreshed_tx, refreshed_rx) = mpsc::unbounded_channel();
    let refresher = LiveDiffStats::with_refresh(Arc::new(TokioClock), move || {
        let _ = refreshed_tx.send(Instant::now());

        tokio::time::sleep(duration)
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
    let (refresher, mut refreshed_rx) = recording_refresher(Duration::ZERO);
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
    assert!(refreshed_rx.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn signals_within_the_interval_coalesce_into_one_trailing_refresh() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher(Duration::ZERO);
    let started = Instant::now();

    // Act
    refresher.trigger.notify_one();
    let first = refreshed_rx.recv().await.expect("first refresh");
    refresher.trigger.notify_one();
    refresher.trigger.notify_one();
    let trailing = refreshed_rx.recv().await.expect("trailing refresh");
    tokio::time::sleep(LIVE_REFRESH_INTERVAL * 3).await;
    let extra = refreshed_rx.try_recv();
    refresher.finish().await;

    // Assert
    assert_eq!(first, started);
    assert_eq!(trailing - first, LIVE_REFRESH_INTERVAL);
    assert!(extra.is_err(), "coalesced signals refreshed again");
    assert!(refreshed_rx.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn slow_refreshes_wait_their_own_duration_before_the_next() {
    // Arrange
    let duration = LIVE_REFRESH_INTERVAL * 2;
    let (refresher, mut refreshed_rx) = recording_refresher(duration);

    // Act
    refresher.trigger.notify_one();
    let first = refreshed_rx.recv().await.expect("first refresh");
    refresher.trigger.notify_one();
    let next = refreshed_rx.recv().await.expect("next refresh");
    refresher.finish().await;

    // Assert
    assert_eq!(next - first, duration * 2);
}

#[tokio::test(start_paused = true)]
async fn finish_runs_a_pending_refresh_without_waiting_for_the_interval() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher(Duration::ZERO);
    refresher.trigger.notify_one();
    let first = refreshed_rx.recv().await.expect("first refresh");
    refresher.trigger.notify_one();

    // Act
    refresher.finish().await;
    let pending = refreshed_rx.recv().await.expect("pending refresh");

    // Assert
    assert_eq!(pending, first);
    assert!(refreshed_rx.recv().await.is_none());
}

#[tokio::test]
async fn finish_waits_for_an_in_flight_refresh_then_runs_one_pending_refresh() {
    // Arrange
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    let refresher = LiveDiffStats::with_refresh(Arc::new(TokioClock), {
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
                completed.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    refresher.trigger.notify_one();
    started.notified().await;
    refresher.trigger.notify_one();
    refresher.trigger.notify_one();

    // Act
    let finishing = tokio::spawn(refresher.finish());
    release.notify_one();
    started.notified().await;
    let completed_before_pending = completed.load(Ordering::SeqCst);
    release.notify_one();
    finishing.await.expect("finish");

    // Assert
    assert_eq!(completed_before_pending, 1);
    assert_eq!(completed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn finish_without_a_signal_runs_no_refresh() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher(Duration::ZERO);

    // Act
    refresher.finish().await;

    // Assert
    assert!(refreshed_rx.recv().await.is_none());
}

#[tokio::test]
async fn dropping_the_refresher_stops_its_task() {
    // Arrange
    let (refresher, mut refreshed_rx) = recording_refresher(Duration::ZERO);

    // Act
    drop(refresher);

    // Assert
    let closed = tokio::time::timeout(Duration::from_secs(5), refreshed_rx.recv()).await;
    assert!(matches!(closed, Ok(None)));
}
