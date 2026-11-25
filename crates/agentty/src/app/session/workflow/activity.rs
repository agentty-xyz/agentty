//! Bounded per-turn activity reduction for the durable usage footer, beside
//! the turn's live timeline of narration and tool rows.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus, OutputEvent, TurnNotice};
use tokio::sync::mpsc;

use super::timeline::LiveTurn;
use crate::app::AppEvent;
use crate::domain::session::SessionId;
use crate::domain::transcript_notice::TranscriptNotice;

/// Most distinct calls the usage footer counts.
const CALL_LIMIT: usize = 2048;
/// Usage footer line labels in display order, indexed by
/// `ActivityState::usage_section`.
const USAGE_SECTIONS: [&str; 3] = ["Tools: ", "MCP: ", "Skills: "];

/// Reduces snapshots without counting lifecycle updates as additional calls,
/// and feeds the turn's live timeline.
#[derive(Default)]
pub(super) struct TurnActivity {
    calls: ActivityState,
    /// Transcript notices the turn reported, in order.
    notices: Vec<String>,
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

    /// Counts one activity snapshot toward the usage footer.
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
            | TurnNotice::LikelySandboxDenial => return,
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

    /// Summarizes invocation and outcome counts without per-call previews.
    pub(super) fn summary(&self) -> String {
        self.calls.usage_text()
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

    /// Groups invocations with their failed and unfinished counts.
    fn usage_text(&self) -> String {
        let mut groups = BTreeMap::<(usize, &str), (usize, usize, usize)>::new();
        for event in &self.calls {
            let counts = groups
                .entry((Self::usage_section(event.kind), event.name.as_str()))
                .or_default();
            counts.0 += 1;
            counts.1 += usize::from(event.outcome() == ActivityStatus::Failed);
            counts.2 += usize::from(matches!(
                event.status,
                ActivityStatus::Running | ActivityStatus::Interrupted
            ));
        }
        let mut summary = String::new();
        for (section, label) in USAGE_SECTIONS.iter().enumerate() {
            let mut separator = *label;
            for ((group_section, name), (count, failed, interrupted)) in &groups {
                if *group_section != section {
                    continue;
                }
                let _ = write!(summary, "{separator}{name} ×{count}");
                if *failed > 0 {
                    let _ = write!(summary, " ({failed} failed)");
                }
                if *interrupted > 0 {
                    let _ = write!(summary, " ({interrupted} interrupted)");
                }
                separator = ", ";
            }
            if separator == ", " {
                summary.push('\n');
            }
        }
        summary
    }

    /// Maps an activity kind to its `USAGE_SECTIONS` line.
    fn usage_section(kind: ActivityKind) -> usize {
        match kind {
            ActivityKind::Mcp => 1,
            ActivityKind::Skill => 2,
            ActivityKind::Command
            | ActivityKind::FileChange
            | ActivityKind::List
            | ActivityKind::Read
            | ActivityKind::Search
            | ActivityKind::Tool
            | ActivityKind::WebSearch => 0,
        }
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
