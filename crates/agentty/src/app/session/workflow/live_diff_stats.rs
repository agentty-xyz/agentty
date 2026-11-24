//! Live diff-stat refresh while a turn changes its session worktree.
//!
//! Without it the size badge and `+N -N` totals stay at their pre-turn values
//! until the turn and every post-turn effect, such as auto-commit recovery,
//! finish, which can take many minutes.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{ActivityKind, ActivityStatus, TurnEvent};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use super::post_turn::{self, TurnFinalizerContext};
use super::worker::SessionWorkerContext;
use crate::app::session::Clock;
use crate::app::spawn;

/// Minimum spacing between two live refreshes, so command-heavy turns do not
/// run Git diffs back to back on large worktrees.
const LIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// Background refresher fed by the activity a running turn reports.
///
/// The first finished tool call refreshes immediately; calls finishing within
/// [`LIVE_REFRESH_INTERVAL`], or within the last refresh's own duration when
/// that is longer, coalesce into one trailing refresh.
/// [`LiveDiffStats::finish`] runs a still-pending refresh before returning.
pub(super) struct LiveDiffStats {
    /// Ends live refreshing, also when the turn drops the refresher early.
    stop: DropGuard,
    /// Refresher task, awaited so a pending refresh lands before post-turn
    /// effects start.
    task: JoinHandle<bool>,
    /// Pending-refresh signal; holds at most one stored permit.
    trigger: Arc<Notify>,
}

impl LiveDiffStats {
    /// Starts a refresher for the session that `context` runs.
    pub(super) fn start(context: &SessionWorkerContext) -> Self {
        let context = Arc::new(TurnFinalizerContext::from_worker(context));
        let clock = Arc::clone(&context.clock);

        Self::with_refresh(clock, move || {
            let context = Arc::clone(&context);

            async move { post_turn::refresh_channel_diff_stats(&context).await }
        })
    }

    /// Forwards `events` to `forward` until the turn's event stream ends,
    /// requesting a refresh whenever a tool call that may change the worktree
    /// finishes.
    pub(super) async fn relay(
        &self,
        mut events: mpsc::UnboundedReceiver<TurnEvent>,
        forward: mpsc::UnboundedSender<TurnEvent>,
    ) {
        while let Some(event) = events.recv().await {
            if Self::may_change_worktree(&event) {
                self.trigger.notify_one();
            }
            // Fire-and-forget: the consumer may stop early during shutdown.
            let _ = forward.send(event);
        }
    }

    /// Stops live refreshing and waits for any in-flight refresh, then runs
    /// the refresh still pending for edits that finished during the throttle
    /// interval, so post-turn effects start from the turn's final totals.
    ///
    /// Returns whether that pending refresh ran.
    pub(super) async fn finish(self) -> bool {
        let Self { stop, task, .. } = self;
        drop(stop);

        // A panicked refresher only loses live updates; finalization
        // refreshes again.
        task.await.unwrap_or(false)
    }

    /// Starts a refresher that runs `refresh` for each coalesced signal,
    /// timing refreshes with `clock`.
    fn with_refresh<Refresh, Work>(clock: Arc<dyn Clock>, refresh: Refresh) -> Self
    where
        Refresh: Fn() -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let stop = CancellationToken::new();
        let trigger = Arc::new(Notify::new());
        let task = spawn::spawn_detached(Self::run(
            clock,
            refresh,
            Arc::clone(&trigger),
            stop.clone(),
        ));

        Self {
            stop: stop.drop_guard(),
            task,
            trigger,
        }
    }

    /// Returns whether `event` reports a finished tool call that may have
    /// edited files, including commands and provider tools such as `Edit`.
    fn may_change_worktree(event: &TurnEvent) -> bool {
        let TurnEvent::Activity(activity) = event else {
            return false;
        };

        activity.status != ActivityStatus::Running
            && matches!(
                activity.kind,
                ActivityKind::Command
                    | ActivityKind::FileChange
                    | ActivityKind::Mcp
                    | ActivityKind::Tool
            )
    }

    /// Refreshes once per coalesced signal until `stop` is cancelled, then
    /// once more if a signal is still pending, returning whether it did.
    ///
    /// After each refresh it waits [`LIVE_REFRESH_INTERVAL`] or the refresh's
    /// own duration, whichever is longer, so slow worktrees spend at most
    /// half the turn diffing.
    async fn run<Refresh, Work>(
        clock: Arc<dyn Clock>,
        refresh: Refresh,
        trigger: Arc<Notify>,
        stop: CancellationToken,
    ) -> bool
    where
        Refresh: Fn() -> Work,
        Work: Future<Output = ()>,
    {
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                () = trigger.notified() => {}
            }
            let started = clock.now_instant();
            refresh().await;
            let pause = clock
                .now_instant()
                .saturating_duration_since(started)
                .max(LIVE_REFRESH_INTERVAL);
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                () = tokio::time::sleep(pause) => {}
            }
        }

        // A stored permit completes `notified()` on its first poll.
        let pending = tokio::select! {
            biased;
            () = trigger.notified() => true,
            () = std::future::ready(()) => false,
        };
        if pending {
            refresh().await;
        }

        pending
    }
}

#[cfg(test)]
#[path = "live_diff_stats_test.rs"]
mod tests;
