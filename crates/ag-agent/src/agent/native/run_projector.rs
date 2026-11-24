//! Projects one harness run's event stream onto the ordered turn events a
//! session turn reports: notices, streamed output, and tool rows.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityKind, ActivityStatus, DiffStat, LineRange, OutputEvent,
    TurnEvent,
};
use ag_harness::RunEventKind;
use ag_harness::event::{Item, ToolCallItem, ToolCallStatus};
use serde_json::Value;

use super::output::{OutputProjector, turn_notice};
use super::sandbox::notice as sandbox_notice;

/// Most result lines a failed call's output preview keeps.
const FAILURE_TAIL_LINES: usize = 3;
/// Longest command a tool row's input preview keeps, in characters.
const INPUT_LIMIT: usize = 4096;

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
        let run_completed = matches!(event, RunEventKind::Completed(_));
        let mut rows = self.tool_rows.project(&event);
        for output in self.output.project(event) {
            if let OutputEvent::Discard { ids } = &output {
                self.tool_rows.discard(ids);
            }
            events.push(TurnEvent::Output(output));
        }
        // A finished run never answers the calls it left open; the rows of
        // void items are gone already.
        if run_completed {
            rows = self.tool_rows.interrupt_open();
        }
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
    /// Returns the rows `event` starts or finishes.
    fn project(&mut self, event: &RunEventKind) -> Vec<ActivityEvent> {
        match event {
            RunEventKind::ToolCallRequested {
                arguments,
                id,
                name,
                ..
            } => {
                let row = self.requested(id.get(), name, arguments);
                self.open.insert(id.get(), row.clone());

                vec![row]
            }
            RunEventKind::ItemCompleted {
                id,
                item: Item::ToolCall(call),
                ..
            } => {
                let row = self
                    .open
                    .remove(&id.get())
                    .unwrap_or_else(|| self.requested(id.get(), &call.name, &call.arguments));

                vec![Self::answered(row, call)]
            }
            _ => Vec::new(),
        }
    }

    /// Interrupts every open row.
    fn interrupt_open(&mut self) -> Vec<ActivityEvent> {
        std::mem::take(&mut self.open)
            .into_values()
            .map(|mut row| {
                row.status = ActivityStatus::Interrupted;
                row.observed_at = SystemTime::now();

                row
            })
            .collect()
    }

    /// Forgets the rows of void items.
    fn discard(&mut self, ids: &[u64]) {
        for id in ids {
            self.open.remove(id);
        }
    }

    /// Describes a call that starts running, from its name and its
    /// arguments, which need not be valid JSON.
    fn requested(&self, id: u64, name: &str, arguments: &str) -> ActivityEvent {
        let arguments = serde_json::from_str::<Value>(arguments).ok();
        let field = |key: &str| {
            arguments
                .as_ref()
                .and_then(|arguments| arguments.get(key))
                .and_then(Value::as_str)
        };
        let (kind, target, lines) = match name {
            "bash" => (ActivityKind::Command, field("command"), None),
            "write" => (ActivityKind::FileChange, field("path"), None),
            "read" => match field("action") {
                Some("search") => (ActivityKind::Search, field("query"), None),
                Some("list") => (ActivityKind::List, field("path"), None),
                _ => (
                    ActivityKind::Read,
                    field("path"),
                    arguments.as_ref().and_then(Self::line_range),
                ),
            },
            _ => (ActivityKind::Tool, None, None),
        };
        let input = (kind == ActivityKind::Command)
            .then(|| field("command"))
            .flatten()
            .map(|command| command.chars().take(INPUT_LIMIT).collect());

        ActivityEvent {
            attempt_id: self.attempt_id.clone(),
            exit_code: None,
            facts: Box::new(ActivityFacts {
                lines,
                target: target.and_then(ActivityFacts::bounded_line),
                ..ActivityFacts::default()
            }),
            id: id.to_string(),
            input,
            kind,
            name: name.chars().take(128).collect(),
            observed_at: SystemTime::now(),
            output: None,
            parent_id: None,
            status: ActivityStatus::Running,
        }
    }

    /// Returns the line range a file read's `offset` and `limit` select,
    /// either of which may be a decimal string.
    fn line_range(arguments: &Value) -> Option<LineRange> {
        let number = |key: &str| {
            let value = arguments.get(key)?;
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        };

        LineRange::from_window(number("offset"), number("limit"))
    }

    /// Finishes `row` with how `call` was answered.
    fn answered(mut row: ActivityEvent, call: &ToolCallItem) -> ActivityEvent {
        row.status = match call.status {
            ToolCallStatus::Completed | ToolCallStatus::Skipped | ToolCallStatus::Answered => {
                ActivityStatus::Completed
            }
            ToolCallStatus::Unanswered => ActivityStatus::Interrupted,
            _ => ActivityStatus::Failed,
        };
        Self::record_outcome(&mut row, call);
        if row.status == ActivityStatus::Failed {
            row.facts.reason = ActivityFacts::bounded_line(&call.output);
        }
        if row.outcome() == ActivityStatus::Failed {
            row.output = Self::failure_tail(row.kind, &call.output);
        }
        row.observed_at = SystemTime::now();

        row
    }

    /// Copies the structured outcome the harness reported for `call`: its
    /// duration, a command's exit, and a write's line counts.
    fn record_outcome(row: &mut ActivityEvent, call: &ToolCallItem) {
        row.exit_code = call.exit_code.map(i64::from);
        row.facts.diff_stat =
            (call.lines_added.is_some() || call.lines_removed.is_some()).then(|| DiffStat {
                added: call.lines_added.unwrap_or_default(),
                removed: call.lines_removed.unwrap_or_default(),
            });
        row.facts.duration = call.duration_ms.map(Duration::from_millis);
        row.facts.pipe_status = call.pipe_status.iter().copied().map(i64::from).collect();
        row.facts.signal = call.signal.map(i64::from);
    }

    /// Returns the last lines of a failed call's result: a command's
    /// standard error, or its standard output when that is empty, and for
    /// another tool the lines after the first, which is the row's reason.
    fn failure_tail(kind: ActivityKind, output: &str) -> Option<String> {
        let text = if kind == ActivityKind::Command {
            Self::command_output(output).unwrap_or_else(|| output.to_string())
        } else {
            output.lines().skip(1).collect::<Vec<_>>().join("\n")
        };
        let mut lines = text
            .lines()
            .rev()
            .filter_map(ActivityFacts::bounded_line)
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
}

#[cfg(test)]
#[path = "run_projector_test.rs"]
mod tests;
