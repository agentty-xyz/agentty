//! Projects one harness run's event stream onto the ordered turn events a
//! session turn reports: notices, streamed output, and tool rows.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, DiffStat,
    TurnEvent,
};
use ag_harness::bash::{CommandExit, CommandTermination};
use ag_harness::event::{Item, RunSummary, ToolCallItem, ToolCallOutcome, ToolCallStatus};
use ag_harness::{ErrorKind, RunEventKind};
use serde_json::Value;

use super::output::{OutputProjector, turn_notice};
use super::sandbox::notice as sandbox_notice;
use super::tool_description::ToolDescription;
use crate::agent::activity_content::first_line;

/// Most result lines a failed call's output preview keeps.
const FAILURE_TAIL_LINES: usize = 3;

/// Turns one run's events into [`TurnEvent`]s in stream order, so tool rows
/// stay ordered with the narration around them.
///
/// Tool rows are keyed by the call's run item id, which its streamed
/// [`ag_contracts::OutputKind::ToolCall`] item shares, under one attempt id
/// per run. Further event kinds a turn should report project here too.
pub(crate) struct RunProjector {
    output: OutputProjector,
    tool_rows: ToolRows,
}

impl RunProjector {
    /// Creates a projector for one run with a fresh attempt id.
    pub(crate) fn new() -> Self {
        Self {
            output: OutputProjector::default(),
            tool_rows: ToolRows {
                attempt_id: uuid::Uuid::new_v4().to_string(),
                open: BTreeMap::new(),
            },
        }
    }

    /// Returns the turn events `event` warrants, in order.
    pub(crate) fn project(&mut self, event: RunEventKind) -> Vec<TurnEvent> {
        let mut events = Vec::new();
        if let Some(notice) = turn_notice(&event).or_else(|| sandbox_notice(&event)) {
            events.push(TurnEvent::Notice(notice));
        }
        let rows = self.tool_rows.project(&event);
        events.extend(
            self.output
                .project(event)
                .into_iter()
                .map(TurnEvent::Output),
        );
        events.extend(rows.into_iter().map(TurnEvent::Activity));

        events
    }
}

/// Tool rows of one run, open from the call's request until its result.
struct ToolRows {
    attempt_id: String,
    /// Rows whose call has not been answered, by run item id.
    open: BTreeMap<u64, ActivityEvent>,
}

impl ToolRows {
    /// Returns the rows `event` starts, finishes, or closes.
    fn project(&mut self, event: &RunEventKind) -> Vec<ActivityEvent> {
        match event {
            RunEventKind::ToolCallRequested {
                arguments,
                call_id,
                id,
                name,
                ..
            } => {
                let row = self.requested(id.get(), call_id, name, arguments);
                self.open.insert(id.get(), row.clone());

                vec![row]
            }
            RunEventKind::ItemCompleted {
                id,
                item: Item::ToolCall(call),
                ..
            } => {
                let row = self.open.remove(&id.get()).unwrap_or_else(|| {
                    self.requested(id.get(), &call.call_id, &call.name, &call.arguments)
                });

                vec![Self::answered(row, call)]
            }
            RunEventKind::Completed(summary) => self.close_open(summary),
            _ => Vec::new(),
        }
    }

    /// Describes a call that starts running, from its name and its
    /// arguments, which need not be valid.
    fn requested(&self, id: u64, call_id: &str, name: &str, arguments: &str) -> ActivityEvent {
        let description = ToolDescription::new(call_id, name, Some(arguments));

        ActivityEvent {
            attempt_id: self.attempt_id.clone(),
            exit_code: None,
            facts: Box::new(ActivityFacts {
                lines: description.lines,
                target: description.target,
                ..ActivityFacts::default()
            }),
            id: id.to_string(),
            input: description.input,
            kind: description.kind,
            name: name.chars().take(128).collect(),
            observed_at: SystemTime::now(),
            output: None,
            parent_id: None,
            status: ActivityStatus::Running,
        }
    }

    /// Finishes `row` with how `call` was answered.
    fn answered(mut row: ActivityEvent, call: &ToolCallItem) -> ActivityEvent {
        let (status, failure) = match call.status {
            ToolCallStatus::Completed | ToolCallStatus::Skipped | ToolCallStatus::Answered => {
                (ActivityStatus::Completed, None)
            }
            ToolCallStatus::Unanswered => (ActivityStatus::Interrupted, None),
            status => (ActivityStatus::Failed, Some(Self::failure_of(status))),
        };
        row.status = status;
        row.facts.failure = failure;
        Self::record_outcome(&mut row, call);
        if row.status == ActivityStatus::Failed && row.facts.reason.is_none() {
            row.facts.reason = first_line(&call.output);
        }
        if row.outcome() == ActivityStatus::Failed {
            row.output = Self::failure_tail(row.kind, &call.output);
        }
        row.observed_at = SystemTime::now();

        row
    }

    /// Returns why a call answered with the failing `status` failed; a
    /// status this projector does not know is an error.
    fn failure_of(status: ToolCallStatus) -> ActivityFailure {
        [
            (ToolCallStatus::Denied, ActivityFailure::Denied),
            (ToolCallStatus::Rejected, ActivityFailure::Rejected),
            (ToolCallStatus::Unavailable, ActivityFailure::Unavailable),
        ]
        .into_iter()
        .find_map(|(known, failure)| (known == status).then_some(failure))
        .unwrap_or(ActivityFailure::Error)
    }

    /// Copies the structured outcome the harness reported for `call`: its
    /// duration, a command's exit, and a write's line counts.
    fn record_outcome(row: &mut ActivityEvent, call: &ToolCallItem) {
        row.facts.duration = call.duration;
        match &call.outcome {
            Some(ToolCallOutcome::Command(exit)) => {
                row.exit_code = exit.exit_code.map(i64::from);
                row.facts.pipe_status = exit.pipe_status.iter().copied().map(i64::from).collect();
                row.facts.signal = exit.signal.map(i64::from);
                Self::record_termination(row, exit);
            }
            Some(ToolCallOutcome::Write {
                lines_added,
                lines_removed,
                ..
            }) => {
                row.facts.diff_stat = Some(DiffStat {
                    added: *lines_added,
                    removed: *lines_removed,
                });
            }
            _ => {}
        }
    }

    /// Judges a completed command that did not run to its end: one that hit
    /// its deadline or failed to run failed, with how it exited as the
    /// reason, and a cancelled one was interrupted.
    fn record_termination(row: &mut ActivityEvent, exit: &CommandExit) {
        let (status, failure) = match exit.termination {
            CommandTermination::Deadline => {
                (ActivityStatus::Failed, Some(ActivityFailure::TimedOut))
            }
            CommandTermination::Failed => (ActivityStatus::Failed, Some(ActivityFailure::Error)),
            CommandTermination::Cancelled => (ActivityStatus::Interrupted, None),
            _ => return,
        };
        row.status = status;
        row.facts.failure = failure;
        row.facts.reason = failure.and_then(|_| first_line(&exit.to_string()));
    }

    /// Returns the last lines of a failed call's result: a command's
    /// standard error, or its standard output when that is empty. A result
    /// that is not a command outcome skips its first line, which is the
    /// row's reason.
    fn failure_tail(kind: ActivityKind, output: &str) -> Option<String> {
        let text = (kind == ActivityKind::Command)
            .then(|| Self::command_output(output))
            .flatten()
            .unwrap_or_else(|| output.lines().skip(1).collect::<Vec<_>>().join("\n"));
        let mut lines = text
            .lines()
            .rev()
            .filter_map(first_line)
            .take(FAILURE_TAIL_LINES)
            .collect::<Vec<_>>();
        lines.reverse();

        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Returns the end of a `bash` result's standard error, or of its
    /// standard output when standard error is blank, or `None` when the
    /// result is not a command outcome.
    fn command_output(output: &str) -> Option<String> {
        let outcome = serde_json::from_str::<Value>(output).ok()?;
        let stream = |name: &str| {
            let text = |key: &str| {
                outcome
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
            };

            text(&format!("{name}_tail")).or_else(|| text(name))
        };

        stream("stderr")
            .or_else(|| stream("stdout"))
            .map(str::to_string)
            .or_else(|| outcome.is_object().then(String::new))
    }

    /// Closes the rows a finished run never answered: interrupted when the
    /// run succeeded or was cancelled, and failed with the run's error
    /// otherwise.
    fn close_open(&mut self, summary: &RunSummary) -> Vec<ActivityEvent> {
        let error = summary
            .error
            .as_ref()
            .filter(|error| error.kind != ErrorKind::Cancelled);
        let failure = error.map(|error| match error.kind {
            ErrorKind::ApprovalTimeout => ActivityFailure::TimedOut,
            _ => ActivityFailure::Error,
        });
        let reason = error.and_then(|error| first_line(&error.message));

        std::mem::take(&mut self.open)
            .into_values()
            .map(|mut row| {
                row.status = if failure.is_some() {
                    ActivityStatus::Failed
                } else {
                    ActivityStatus::Interrupted
                };
                row.facts.failure = failure;
                row.facts.reason.clone_from(&reason);
                row.observed_at = SystemTime::now();

                row
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "run_projector_test.rs"]
mod tests;
