//! Bounded per-turn activity reduction for live rows and durable usage footers.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::session::SessionId;

const LIVE_RUNNING_LIMIT: usize = 3;
const LIVE_ISSUE_LIMIT: usize = 3;

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
        let ActivityState { calls, issues, .. } = &mut *state;
        for (index, event) in calls.iter_mut().enumerate() {
            if event.status == ActivityStatus::Running {
                event.status = ActivityStatus::Interrupted;
                issues.push(index);
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
        let mut summary = state.usage_text(true);
        for event in &state.calls {
            if ActivityState::status(event) == ActivityStatus::Failed {
                ActivityState::append_call(&mut summary, event);
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
    /// Unique issue indices in observed terminal-transition order, bounded by
    /// calls.
    issues: Vec<usize>,
    /// Last successful completion, independent of invocation order.
    last_completed: Option<usize>,
    pending: bool,
}

impl ActivityState {
    fn observe(&mut self, event: ActivityEvent) {
        let key = (event.attempt_id.clone(), event.id.clone());
        let status = Self::status(&event);
        let (index, previous_status) = if let Some(&index) = self.indices.get(&key) {
            let previous_status = Self::status(&self.calls[index]);
            self.calls[index] = event;

            (index, Some(previous_status))
        } else {
            if self.calls.len() >= 2048 {
                return;
            }
            let index = self.calls.len();
            self.indices.insert(key, index);
            self.calls.push(event);

            (index, None)
        };
        if previous_status == Some(status) {
            return;
        }
        self.issues
            .retain(|previous_index| *previous_index != index);
        match status {
            ActivityStatus::Completed => self.last_completed = Some(index),
            ActivityStatus::Failed | ActivityStatus::Interrupted => self.issues.push(index),
            ActivityStatus::Running => {}
        }
    }

    /// Groups invocations without treating running calls as interrupted in
    /// the live view. Durable summaries retain missing terminal results.
    fn usage_text(&self, include_outcomes: bool) -> String {
        let mut groups = BTreeMap::<(bool, &str), (usize, usize, usize)>::new();
        for event in &self.calls {
            let counts = groups
                .entry((event.kind == ActivityKind::Skill, event.name.as_str()))
                .or_default();
            counts.0 += 1;
            counts.1 += usize::from(Self::status(event) == ActivityStatus::Failed);
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
                if include_outcomes && *failed > 0 {
                    let _ = write!(summary, " ({failed} failed)");
                }
                if include_outcomes && *interrupted > 0 {
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

    /// Shows rolling totals, recent issues, the last successful call, and a
    /// bounded set of running calls. The UI clips previews to the pane width.
    fn live_text(&self) -> String {
        if self.calls.is_empty() {
            return String::new();
        }
        let mut text = self.usage_text(false);
        let mut counts = [0_usize; 4];
        for event in &self.calls {
            let index = match Self::status(event) {
                ActivityStatus::Completed => 0,
                ActivityStatus::Failed => 1,
                ActivityStatus::Running => 2,
                ActivityStatus::Interrupted => 3,
            };
            counts[index] += 1;
        }
        let [succeeded, failed, running, interrupted] = counts;
        let _ = write!(
            text,
            "Status: {succeeded} succeeded · {failed} failed · {running} running"
        );
        if interrupted > 0 {
            let _ = write!(text, " · {interrupted} interrupted");
        }
        text.push_str("\n\n");

        let first_issue = self.issues.len().saturating_sub(LIVE_ISSUE_LIMIT);
        for &index in &self.issues[first_issue..] {
            Self::append_call(&mut text, &self.calls[index]);
        }
        if let Some(index) = self.last_completed
            && Self::status(&self.calls[index]) == ActivityStatus::Completed
        {
            Self::append_call(&mut text, &self.calls[index]);
        }
        let hidden_issues = (failed + interrupted).saturating_sub(LIVE_ISSUE_LIMIT);
        if hidden_issues > 0 {
            let _ = writeln!(text, "+{hidden_issues} earlier failed or interrupted calls");
        }
        for event in self
            .calls
            .iter()
            .filter(|event| event.status == ActivityStatus::Running)
            .take(LIVE_RUNNING_LIMIT)
        {
            Self::append_call(&mut text, event);
        }
        if running > LIVE_RUNNING_LIMIT {
            let hidden = running - LIVE_RUNNING_LIMIT;
            let _ = writeln!(text, "+{hidden} more running");
        }

        text
    }

    /// Accounts for backends that report completion with a nonzero exit code.
    fn status(event: &ActivityEvent) -> ActivityStatus {
        if event.status == ActivityStatus::Completed
            && event.exit_code.is_some_and(|code| code != 0)
        {
            ActivityStatus::Failed
        } else {
            event.status
        }
    }

    fn append_call(text: &mut String, event: &ActivityEvent) {
        let status = Self::status(event);
        let marker = match status {
            ActivityStatus::Running => "…",
            ActivityStatus::Completed => "✓",
            ActivityStatus::Failed => "✗",
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
            && let Some(input) = event
                .input
                .as_deref()
                .and_then(|input| Self::input_preview(input, event.kind))
            && !input.is_empty()
        {
            let _ = write!(text, " · {input}");
        }
        text.push('\n');
        if status == ActivityStatus::Failed
            && let Some(output) = event.output.as_deref()
        {
            Self::append_failure_preview(text, output);
        }
    }

    /// Keeps command text literal while extracting useful fields from tool
    /// arguments without exposing edit payloads or raw JSON.
    fn input_preview(input: &str, kind: ActivityKind) -> Option<String> {
        if kind == ActivityKind::Command {
            return Some(Self::single_line(input));
        }
        let preview = match serde_json::from_str::<Value>(input) {
            Ok(Value::Object(arguments)) => [
                "command",
                "CommandLine",
                "file_path",
                "path",
                "TargetFile",
                "target_file",
                "query",
                "pattern",
            ]
            .iter()
            .find_map(|key| arguments.get(*key).and_then(Value::as_str))?
            .to_string(),
            Ok(Value::String(text)) => text,
            Ok(Value::Array(_)) => return None,
            Err(_) if input.trim_start().starts_with('{') => return None,
            _ => input.to_string(),
        };

        Some(Self::single_line(&preview))
    }

    /// Keeps multiline commands and arguments on one preview row.
    fn single_line(text: &str) -> String {
        text.split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ")
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
