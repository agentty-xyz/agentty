//! Bounded per-turn activity reduction for live rows and durable usage footers.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus};
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::session::SessionId;

/// Reduces snapshots without counting lifecycle updates as additional calls.
#[derive(Default)]
pub(super) struct TurnActivity {
    live: LiveActivity,
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

    pub(super) fn observe(&mut self, event: ActivityEvent) {
        self.live
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(event);
    }

    /// Updates retained calls in place and queues at most one unread
    /// notification.
    pub(super) fn observe_live(
        &mut self,
        event: ActivityEvent,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        self.observe(event);
        self.live.notify(sender, session_id);
    }

    /// Settles calls lacking a terminal provider event when the stream closes.
    pub(super) fn finish_live(
        &mut self,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        let mut state = self
            .live
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = false;
        for event in &mut state.calls {
            if event.status == ActivityStatus::Running {
                event.status = ActivityStatus::Interrupted;
                changed = true;
            }
        }
        drop(state);
        if changed {
            self.live.notify(sender, session_id);
        }
    }

    /// Removes live rows after the caller has handled the completed turn.
    pub(super) fn clear_live(
        &self,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        if !self
            .live
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .calls
            .is_empty()
        {
            let _ = sender.send(AppEvent::SessionActivityUpdated {
                activity: None,
                session_id: session_id.clone(),
            });
        }
    }

    /// Summarizes actual invocations; missing terminal events remain explicit.
    pub(super) fn summary(&self) -> String {
        let state = self
            .live
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut groups = BTreeMap::<(bool, &str), (usize, usize, usize)>::new();
        for event in &state.calls {
            let counts = groups
                .entry((event.kind == ActivityKind::Skill, event.name.as_str()))
                .or_default();
            counts.0 += 1;
            counts.1 += usize::from(event.status == ActivityStatus::Failed);
            counts.2 += usize::from(matches!(
                event.status,
                ActivityStatus::Running | ActivityStatus::Interrupted
            ));
        }
        let mut summary = String::new();
        for skill in [false, true] {
            let mut separator = if skill { "Skills: " } else { "Tools: " };
            for ((is_skill, name), (count, failed, interrupted)) in &groups {
                if *is_skill != skill {
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
}

/// A replaceable live snapshot shared by a turn producer and the foreground.
/// Equality identifies the mailbox, independent of its changing contents.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveActivity {
    state: Arc<Mutex<ActivityState>>,
}

impl LiveActivity {
    /// Formats only when the foreground consumes the notification. A later
    /// update can then enqueue another notification without losing a wakeup.
    pub(crate) fn snapshot(&self) -> String {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending = false;

        state.live_text()
    }

    fn notify(&self, sender: &mpsc::UnboundedSender<AppEvent>, session_id: &SessionId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.pending {
            state.pending = sender
                .send(AppEvent::SessionActivityReady {
                    activity: self.clone(),
                    session_id: session_id.clone(),
                })
                .is_ok();
        }
    }
}

impl PartialEq for LiveActivity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for LiveActivity {}

/// Bounded calls retained in first-seen order, with one outstanding wakeup.
#[derive(Debug, Default)]
struct ActivityState {
    calls: Vec<ActivityEvent>,
    indices: BTreeMap<(String, String), usize>,
    pending: bool,
}

impl ActivityState {
    fn observe(&mut self, event: ActivityEvent) {
        let key = (event.attempt_id.clone(), event.id.clone());
        if let Some(index) = self.indices.get(&key) {
            self.calls[*index] = event;
        } else if self.calls.len() < 2048 {
            self.indices.insert(key, self.calls.len());
            self.calls.push(event);
        }
    }

    /// Formats compact call rows in first-seen order, with output only for
    /// failures. The UI clips each logical line to the available pane width.
    fn live_text(&self) -> String {
        let mut text = String::new();
        for event in &self.calls {
            let failed = event.status == ActivityStatus::Failed
                || event.exit_code.is_some_and(|code| code != 0);
            let marker = match event.status {
                ActivityStatus::Running => "…",
                ActivityStatus::Completed if !failed => "✓",
                ActivityStatus::Completed | ActivityStatus::Failed => "✗",
                ActivityStatus::Interrupted => "!",
            };
            let kind = if event.kind == ActivityKind::Skill {
                "skill "
            } else {
                ""
            };
            let _ = write!(text, "{marker} {kind}{}", Self::single_line(&event.name));
            if event.status == ActivityStatus::Interrupted {
                text.push_str(" (interrupted)");
            }
            if let Some(exit_code) = event.exit_code.filter(|code| *code != 0) {
                let _ = write!(text, " (exit {exit_code})");
            }
            if event.kind != ActivityKind::Skill
                && let Some(input) = event.input.as_deref()
            {
                let input = Self::single_line(input);
                if !input.is_empty() {
                    let _ = write!(text, " · {input}");
                }
            }
            text.push('\n');
            if failed && let Some(output) = event.output.as_deref() {
                Self::append_failure_preview(&mut text, output);
            }
        }

        text
    }

    /// Keeps multiline commands and arguments on one preview row.
    fn single_line(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Shows at most two nonempty error lines, with an inline truncation hint.
    fn append_failure_preview(text: &mut String, preview: &str) {
        let mut lines = preview
            .lines()
            .filter(|line| !line.trim().is_empty())
            .peekable();
        for index in 0..2 {
            let Some(line) = lines.next() else {
                break;
            };
            let _ = write!(text, "  {}", Self::single_line(line));
            if index == 1 && lines.peek().is_some() {
                text.push_str(" …");
            }
            text.push('\n');
        }
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
