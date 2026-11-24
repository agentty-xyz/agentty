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
use tokio_util::sync::CancellationToken;

use super::post_turn::{self, TurnFinalizerContext};
use crate::app::spawn;

/// Minimum spacing between two live refreshes, so command-heavy turns do not
/// run Git diffs back to back on large worktrees.
const LIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// Background refresher fed by the activity a running turn reports.
///
/// The first finished tool call refreshes immediately; calls finishing within
/// [`LIVE_REFRESH_INTERVAL`] of a refresh coalesce into one trailing refresh.
pub(super) struct LiveDiffStats {
    /// Stops the refresher between refreshes.
    stop: CancellationToken,
    /// Refresher task, awaited so no stale refresh lands after the turn's own.
    task: JoinHandle<()>,
    /// Pending-refresh signal; holds at most one stored permit.
    trigger: Arc<Notify>,
}

impl LiveDiffStats {
    /// Starts a refresher for the session that `context` finalizes.
    pub(super) fn start(context: TurnFinalizerContext) -> Self {
        let context = Arc::new(context);

        Self::spawn(move || {
            let context = Arc::clone(&context);

            async move { post_turn::refresh_channel_diff_stats(&context).await }
        })
    }

    /// Starts a refresher that runs `refresh` for each coalesced signal.
    fn spawn<Refresh, Work>(refresh: Refresh) -> Self
    where
        Refresh: Fn() -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let stop = CancellationToken::new();
        let trigger = Arc::new(Notify::new());
        let task = spawn::spawn_detached(Self::run(refresh, Arc::clone(&trigger), stop.clone()));

        Self {
            stop,
            task,
            trigger,
        }
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

    /// Stops refreshing and waits for an in-flight refresh to land, so the
    /// caller's next refresh is the latest one persisted.
    pub(super) async fn finish(self) {
        self.stop.cancel();
        // A panicked refresher only loses live updates; the caller refreshes.
        let _ = self.task.await;
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

    /// Refreshes once per signal, at most once per [`LIVE_REFRESH_INTERVAL`],
    /// until `stop` is cancelled.
    async fn run<Refresh, Work>(refresh: Refresh, trigger: Arc<Notify>, stop: CancellationToken)
    where
        Refresh: Fn() -> Work,
        Work: Future<Output = ()>,
    {
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                () = trigger.notified() => {}
            }
            refresh().await;
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                () = tokio::time::sleep(LIVE_REFRESH_INTERVAL) => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "live_diff_stats_test.rs"]
mod tests;
