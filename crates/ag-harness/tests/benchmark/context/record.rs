//! Per-run measurements written as one JSON line.

use std::collections::BTreeMap;

use ag_harness::TurnOutcome;
use ag_harness::turn::{ContextActivity, ModelRequestPurpose, ToolActivity};
use serde_json::{Map, Value, json};

/// Content-free activity summed over every accepted request of one turn.
pub(super) struct Activity {
    context: [(&'static str, usize); 8],
    duplicate_reads: usize,
    input_tokens: Option<u64>,
    model_calls: usize,
    output_corrections: usize,
    output_tokens: Option<u64>,
    repetition_nudges: usize,
    summary_calls: usize,
    summary_failures: usize,
    tool_calls: usize,
}

impl Activity {
    /// Measures a finished turn from its public report.
    pub(super) fn from_outcome(outcome: &TurnOutcome) -> Self {
        let report = outcome.report();
        let mut context = ContextCounts::default();
        let mut input_tokens = None::<u64>;
        let mut output_tokens = None::<u64>;
        for request in report.model_requests() {
            context.add(request.context());
            let usage = request
                .completion()
                .and_then(|completion| completion.usage());
            if let Some(tokens) = usage.and_then(|usage| usage.input_tokens()) {
                input_tokens = Some(input_tokens.unwrap_or(0).saturating_add(tokens));
            }
            if let Some(tokens) = usage.and_then(|usage| usage.output_tokens()) {
                output_tokens = Some(output_tokens.unwrap_or(0).saturating_add(tokens));
            }
        }

        Self {
            context: context.0,
            duplicate_reads: duplicate_reads(report.tool_calls()),
            input_tokens,
            model_calls: report.model_requests().len(),
            output_corrections: report.output_corrections(),
            output_tokens,
            repetition_nudges: report.repetition_nudges(),
            summary_calls: report
                .model_requests()
                .iter()
                .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
                .count(),
            summary_failures: report.context_summary_failures(),
            tool_calls: report.tool_calls().len(),
        }
    }

    fn insert_into(&self, line: &mut Map<String, Value>) {
        line.insert("model_calls".into(), json!(self.model_calls));
        line.insert("output_corrections".into(), json!(self.output_corrections));
        line.insert("tool_calls".into(), json!(self.tool_calls));
        line.insert("duplicate_reads".into(), json!(self.duplicate_reads));
        line.insert("input_tokens".into(), json!(self.input_tokens));
        line.insert("output_tokens".into(), json!(self.output_tokens));
        line.insert("repetition_nudges".into(), json!(self.repetition_nudges));
        line.insert("summary_calls".into(), json!(self.summary_calls));
        line.insert("summary_failures".into(), json!(self.summary_failures));
        for (name, count) in self.context {
            line.insert(name.into(), json!(count));
        }
    }
}

/// One scenario run as written to the results file.
pub(super) struct RunRecord {
    pub(super) activity: Option<Activity>,
    pub(super) context_summaries: bool,
    pub(super) correctness: f64,
    pub(super) detail: Option<String>,
    pub(super) model: String,
    pub(super) provider: &'static str,
    pub(super) repetition: usize,
    pub(super) reserved_output: u64,
    pub(super) revision: Option<String>,
    pub(super) scenario: &'static str,
    pub(super) started_at: u64,
    pub(super) success: bool,
    pub(super) wall_ms: u128,
    pub(super) window: u64,
}

impl RunRecord {
    /// Serializes the record; activity fields are `null` when the turn
    /// failed, because the public report exists only for finished turns.
    pub(super) fn to_json(&self) -> Value {
        let mut line = Map::new();
        line.insert("started_at".into(), json!(self.started_at));
        line.insert("revision".into(), json!(self.revision));
        line.insert("provider".into(), json!(self.provider));
        line.insert("model".into(), json!(self.model));
        line.insert("scenario".into(), json!(self.scenario));
        line.insert("repetition".into(), json!(self.repetition));
        line.insert("window".into(), json!(self.window));
        line.insert("reserved_output".into(), json!(self.reserved_output));
        line.insert("context_summaries".into(), json!(self.context_summaries));
        line.insert("success".into(), json!(self.success));
        line.insert("correctness".into(), json!(self.correctness));
        line.insert("wall_ms".into(), json!(self.wall_ms));
        match &self.activity {
            Some(activity) => activity.insert_into(&mut line),
            None => {
                for name in ACTIVITY_FIELDS.iter().chain(&CONTEXT_FIELDS) {
                    line.insert((*name).into(), Value::Null);
                }
            }
        }
        line.insert("detail".into(), json!(self.detail));

        Value::Object(line)
    }
}

const ACTIVITY_FIELDS: [&str; 9] = [
    "model_calls",
    "output_corrections",
    "tool_calls",
    "duplicate_reads",
    "input_tokens",
    "output_tokens",
    "repetition_nudges",
    "summary_calls",
    "summary_failures",
];
const CONTEXT_FIELDS: [&str; 8] = [
    "elided_results",
    "cleared_reasoning",
    "shrunk_write_calls",
    "dropped_history_turns",
    "dropped_tool_calls",
    "truncated_results",
    "shrunk_latest_write_calls",
    "summarized_tool_calls",
];

struct ContextCounts([(&'static str, usize); 8]);

impl Default for ContextCounts {
    fn default() -> Self {
        Self(CONTEXT_FIELDS.map(|name| (name, 0)))
    }
}

impl ContextCounts {
    fn add(&mut self, activity: ContextActivity) {
        let counts = [
            activity.elided_results(),
            activity.cleared_reasoning(),
            activity.shrunk_write_calls(),
            activity.dropped_history_turns(),
            activity.dropped_tool_calls(),
            activity.truncated_results(),
            activity.shrunk_latest_write_calls(),
            activity.summarized_tool_calls(),
        ];
        for ((_, total), count) in self.0.iter_mut().zip(counts) {
            *total += count;
        }
    }
}

/// Counts reads that repeat an earlier read target in the same turn: a file
/// read at the same path and starting line, or an identical inspection.
fn duplicate_reads(tool_calls: &[ToolActivity]) -> usize {
    let mut seen = BTreeMap::<String, usize>::new();
    for activity in tool_calls {
        let target = match activity {
            ToolActivity::Read {
                path, start_line, ..
            } => format!("file:{path}:{start_line}"),
            ToolActivity::ReadInspection {
                action, summary, ..
            } => format!("{action:?}:{summary}"),
            _ => continue,
        };
        *seen.entry(target).or_default() += 1;
    }

    seen.values().map(|count| count - 1).sum()
}
