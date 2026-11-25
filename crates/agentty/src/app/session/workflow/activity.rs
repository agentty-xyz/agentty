//! Bounded per-turn activity reduction for the durable turn summary, beside
//! the turn's live timeline of narration and tool rows.

use std::collections::BTreeMap;

use ag_contracts::{ActivityEvent, ActivityStatus, OutputEvent, TurnNotice};
use tokio::sync::mpsc;

use super::timeline::LiveTurn;
use crate::app::AppEvent;
use crate::domain::session::SessionId;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_summary::TurnSummary;

/// Most distinct calls the turn summary counts.
const CALL_LIMIT: usize = 2048;

/// Reduces snapshots without counting lifecycle updates as additional calls,
/// and feeds the turn's live timeline.
#[derive(Default)]
pub(super) struct TurnActivity {
    calls: ActivityState,
    /// Unix milliseconds when the turn's events ended.
    finished_at: Option<u64>,
    /// Transcript notices the turn reported, in order.
    notices: Vec<String>,
    /// Unix milliseconds when the turn's events started.
    started_at: Option<u64>,
    turn: LiveTurn,
}

impl TurnActivity {
    /// Drains utility activity while its submission runs, retaining only the
    /// bounded per-call state needed for a footer.
    pub(super) async fn collect(
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<ActivityEvent>,
    ) -> Self {
        let mut activity = Self::default();
        while let Some(event) = receiver.recv().await {
            activity.observe(event);
        }

        activity
    }

    /// Records `now` Unix milliseconds as the turn's start, from which the
    /// summary measures the time the turn took.
    pub(super) fn start(&mut self, now: u64) {
        self.started_at = Some(now);
    }

    /// Counts one activity snapshot toward the turn summary.
    pub(super) fn observe(&mut self, event: ActivityEvent) {
        self.calls.observe(event);
    }

    /// Counts one activity snapshot observed at `now` Unix milliseconds and
    /// shows it in the live timeline, queueing at most one unread
    /// notification.
    pub(super) fn observe_live(
        &mut self,
        event: ActivityEvent,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        self.turn
            .observe_activity(event.clone(), now, sender, session_id);
        self.observe(event);
    }

    /// Applies one streamed output change observed at `now` Unix
    /// milliseconds and queues at most one unread notification.
    pub(super) fn observe_output(
        &self,
        event: OutputEvent,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        self.turn.observe_output(event, now, sender, session_id);
    }

    /// Records the turn's latest progress text, which names the current
    /// action while nothing more specific runs.
    pub(super) fn observe_progress(
        &self,
        text: String,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        self.turn.observe_progress(text, now, sender, session_id);
    }

    /// Returns what the turn is doing now, such as `Running cargo test`, or
    /// `None` before its timeline shows anything.
    pub(super) fn current_action_text(&self) -> Option<String> {
        self.turn.current_action_text()
    }

    /// Keeps the transcript notice for one thing the turn reported about how
    /// it ran, such as a compaction of the session's earlier context.
    /// Sandbox notices depend on the session's earlier ones, so a session
    /// turn relays them to its transcript as they arrive instead.
    pub(super) fn observe_notice(&mut self, notice: &TurnNotice) {
        let text = match notice {
            TurnNotice::SandboxUnavailable { .. }
            | TurnNotice::SandboxAvailable
            | TurnNotice::LikelySandboxDenial
            | TurnNotice::HostCachesNotShared { .. } => return,
            TurnNotice::ContextCompacted { model: Some(model) } => TranscriptNotice::Context
                .format(format_args!(
                    "Summarized earlier turns with `{model}` to fit the context window."
                )),
            TurnNotice::ContextCompacted { model: None } => TranscriptNotice::Context
                .format("Summarized earlier turns to fit the context window."),
            TurnNotice::ContextCompactionFailed { reason } => TranscriptNotice::ContextWarning
                .format(format_args!(
                    "Could not summarize earlier turns ({reason}); the oldest no longer fit and \
                     were dropped from the context."
                )),
        };
        self.notices.push(text);
    }

    /// Returns the transcript notices the turn reported, in order.
    pub(super) fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Settles calls lacking a terminal provider event when the stream
    /// closes, at `now` Unix milliseconds.
    pub(super) fn finish_live(
        &mut self,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        self.finished_at = Some(now);
        self.calls.interrupt_running();
        self.turn.interrupt_running(now, sender, session_id);
    }

    /// Removes the live timeline once the turn's saved answer or failure
    /// notice replaces it; later calls send nothing.
    pub(super) fn clear_live(
        &self,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        self.turn.clear(sender, session_id);
    }

    /// Summarizes the turn from its calls, with the steps its live timeline
    /// counted when it narrated; a utility prompt has no timeline. The time
    /// it took is known once both its start and end were recorded.
    pub(super) fn summary(&self) -> TurnSummary {
        let elapsed_seconds = self
            .started_at
            .zip(self.finished_at)
            .map(|(started_at, finished_at)| finished_at.saturating_sub(started_at) / 1000);
        let calls = self.calls.calls.iter().collect::<Vec<_>>();

        TurnSummary::from_calls(&calls, self.turn.narrated_steps(), elapsed_seconds)
    }
}

/// Bounded calls retained in first-seen order.
#[derive(Debug, Default)]
struct ActivityState {
    calls: Vec<ActivityEvent>,
    indices: BTreeMap<(String, String), usize>,
}

impl ActivityState {
    fn observe(&mut self, event: ActivityEvent) {
        let key = (event.attempt_id.clone(), event.id.clone());
        if let Some(&index) = self.indices.get(&key) {
            self.calls[index] = event;

            return;
        }
        if self.calls.len() >= CALL_LIMIT {
            return;
        }
        self.indices.insert(key, self.calls.len());
        self.calls.push(event);
    }

    /// Marks calls that never reported a result as interrupted.
    fn interrupt_running(&mut self) {
        for event in &mut self.calls {
            if event.status == ActivityStatus::Running {
                event.status = ActivityStatus::Interrupted;
            }
        }
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
