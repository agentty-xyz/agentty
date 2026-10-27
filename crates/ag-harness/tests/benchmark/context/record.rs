//! Per-run measurements written as one JSON line.

use ag_harness::TurnOutcome;
use ag_harness::lifecycle::LifecycleEventKind;
use ag_harness::model::CompletionMetadata;
use ag_harness::turn::{ContextActivity, ModelRequestActivity, ModelRequestPurpose, TurnReport};
use serde_json::{Map, Value, json};

use super::metric;

/// Content-free activity summed over every accepted request of a run's
/// turns, with one record per request and, for a session, one per turn.
pub(super) struct Activity {
    cache: Map<String, Value>,
    context: [(&'static str, usize); 9],
    context_overflow_recoveries: usize,
    deferred_reads: usize,
    duplicate_reads: usize,
    input_tokens: Option<u64>,
    malformed_tool_calls: usize,
    model_calls: usize,
    output_corrections: usize,
    output_tokens: Option<u64>,
    repetition_nudges: usize,
    requests: Vec<Value>,
    summary_calls: usize,
    summary_failures: usize,
    tool_calls: usize,
    turns: Vec<Value>,
}

/// One measured turn of a run: its report and the message digests of the
/// requests the report records.
pub(super) struct MeasuredTurn<'a> {
    pub(super) report: &'a TurnReport,
    pub(super) sent: &'a [Vec<u64>],
}

impl Activity {
    /// Measures one finished turn from its public report.
    pub(super) fn from_outcome(outcome: &TurnOutcome, sent: &[Vec<u64>]) -> Self {
        Self::from_turns(&[MeasuredTurn {
            report: outcome.report(),
            sent,
        }])
    }

    /// Measures a run's turns, in order, from their reports; a failed turn
    /// carries its report on its failure event.
    pub(super) fn from_turns(turns: &[MeasuredTurn<'_>]) -> Self {
        let mut context = ContextCounts::default();
        let mut input_tokens = None::<u64>;
        let mut output_tokens = None::<u64>;
        let mut requests = Vec::new();
        let mut tool_calls = Vec::new();
        let mut turn_records = Vec::new();
        for (index, turn) in turns.iter().enumerate() {
            let reported = turn.report.model_requests();
            for request in reported {
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
            // Digests that do not pair one-to-one with the reported requests
            // are left out rather than attributed to the wrong request.
            let aligned = turn.sent.len() == reported.len();
            requests.extend(reported.iter().enumerate().map(|(position, request)| {
                let sent = turn.sent.get(position).filter(|_| aligned);

                request_record(request, index, sent)
            }));
            tool_calls.extend_from_slice(turn.report.tool_calls());
            let history = turn.report.history();
            turn_records.push(json!({
                "model_calls": reported.len(),
                "replayed_turns": history.replayed_turns(),
                "evicted_turns": history.evicted_turns(),
                "checkpoint_replayed": history.checkpoint_replayed(),
            }));
        }
        let cache = metric::record_cache_reuse(&mut requests);
        let reports = turns.iter().map(|turn| turn.report);

        Self {
            cache,
            context: context.0,
            context_overflow_recoveries: reports
                .clone()
                .map(TurnReport::context_overflow_recoveries)
                .sum(),
            deferred_reads: metric::deferred_reads(&tool_calls),
            duplicate_reads: metric::duplicate_reads(&tool_calls),
            input_tokens,
            malformed_tool_calls: reports.clone().map(TurnReport::malformed_tool_calls).sum(),
            model_calls: requests.len(),
            output_corrections: reports.clone().map(TurnReport::output_corrections).sum(),
            output_tokens,
            repetition_nudges: reports.clone().map(TurnReport::repetition_nudges).sum(),
            summary_calls: requests
                .iter()
                .filter(|request| request["purpose"] == "context_summary")
                .count(),
            summary_failures: reports.map(TurnReport::context_summary_failures).sum(),
            requests,
            tool_calls: tool_calls.len(),
            turns: turn_records,
        }
    }

    fn insert_into(&self, line: &mut Map<String, Value>) {
        line.insert("model_calls".into(), json!(self.model_calls));
        line.insert("output_corrections".into(), json!(self.output_corrections));
        line.insert("tool_calls".into(), json!(self.tool_calls));
        line.insert("duplicate_reads".into(), json!(self.duplicate_reads));
        line.insert("deferred_reads".into(), json!(self.deferred_reads));
        line.insert("input_tokens".into(), json!(self.input_tokens));
        line.insert("output_tokens".into(), json!(self.output_tokens));
        for (name, value) in &self.cache {
            line.insert(name.clone(), value.clone());
        }
        line.insert("repetition_nudges".into(), json!(self.repetition_nudges));
        line.insert("summary_calls".into(), json!(self.summary_calls));
        line.insert("summary_failures".into(), json!(self.summary_failures));
        line.insert(
            "malformed_tool_calls".into(),
            json!(self.malformed_tool_calls),
        );
        line.insert(
            "context_overflow_recoveries".into(),
            json!(self.context_overflow_recoveries),
        );
        line.insert("requests".into(), json!(self.requests));
        line.insert("turns".into(), json!(self.turns));
        for (name, count) in self.context {
            line.insert(name.into(), json!(count));
        }
    }
}

/// Summary outcomes and failed model requests seen as lifecycle events,
/// kept even when the turn fails, with the report a failed turn carries.
#[derive(Default)]
pub(super) struct Observed {
    failed_report: Option<Box<TurnReport>>,
    failed_requests: Vec<String>,
    summaries: Vec<(&'static str, u128)>,
}

impl Observed {
    /// Records one lifecycle event of the measured turn.
    pub(super) fn observe(&mut self, event: &LifecycleEventKind) {
        match event {
            LifecycleEventKind::ModelRequestFailed {
                error_type,
                http_status,
                ..
            } => self.failed_requests.push(http_status.map_or_else(
                || error_type.as_str().to_string(),
                |status| format!("{}:{status}", error_type.as_str()),
            )),
            LifecycleEventKind::ContextSummaryCompleted { duration, .. } => {
                self.summaries.push(("completed", duration.as_millis()));
            }
            LifecycleEventKind::ContextSummaryFailed {
                duration,
                error_type,
                ..
            } => self
                .summaries
                .push((error_type.as_str(), duration.as_millis())),
            LifecycleEventKind::TurnFailed { report, .. } => self.failed_report.clone_from(report),
            _ => {}
        }
    }

    /// Takes the report of the measured turn when it failed.
    pub(super) fn take_failed_report(&mut self) -> Option<Box<TurnReport>> {
        self.failed_report.take()
    }

    fn insert_into(&self, line: &mut Map<String, Value>) {
        let (outcomes, durations): (Vec<_>, Vec<_>) = self.summaries.iter().copied().unzip();
        line.insert("summary_outcomes".into(), json!(outcomes));
        line.insert("summary_ms".into(), json!(durations));
        line.insert("failed_requests".into(), json!(self.failed_requests));
    }
}

/// One scenario run as written to the results file.
pub(super) struct RunRecord {
    pub(super) activity: Option<Activity>,
    pub(super) context_summaries: bool,
    pub(super) correctness: f64,
    pub(super) detail: Option<String>,
    pub(super) estimate: Option<String>,
    pub(super) history_bytes: Option<usize>,
    pub(super) model: String,
    pub(super) observed: Observed,
    pub(super) provider: &'static str,
    pub(super) repetition: usize,
    pub(super) reserved_output: u64,
    pub(super) revision: Option<String>,
    pub(super) scenario: &'static str,
    pub(super) started_at: u64,
    pub(super) success: bool,
    /// The run's exported trace, when OTLP export is enabled.
    pub(super) trace_id: Option<String>,
    pub(super) wall_ms: u128,
    pub(super) window: u64,
    pub(super) window_mode: &'static str,
}

impl RunRecord {
    /// Serializes the record; activity fields are `null` when no report
    /// exists: the turn timed out or failed before preparing its request.
    pub(super) fn to_json(&self) -> Value {
        let mut line = Map::new();
        line.insert("started_at".into(), json!(self.started_at));
        line.insert("revision".into(), json!(self.revision));
        line.insert("trace_id".into(), json!(self.trace_id));
        line.insert("provider".into(), json!(self.provider));
        line.insert("model".into(), json!(self.model));
        line.insert("scenario".into(), json!(self.scenario));
        line.insert("repetition".into(), json!(self.repetition));
        line.insert("window".into(), json!(self.window));
        line.insert("window_mode".into(), json!(self.window_mode));
        line.insert("reserved_output".into(), json!(self.reserved_output));
        line.insert("context_summaries".into(), json!(self.context_summaries));
        line.insert("estimate".into(), json!(self.estimate));
        line.insert("history_bytes".into(), json!(self.history_bytes));
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
        self.observed.insert_into(&mut line);
        line.insert("detail".into(), json!(self.detail));

        Value::Object(line)
    }
}

const ACTIVITY_FIELDS: [&str; 22] = [
    "model_calls",
    "output_corrections",
    "tool_calls",
    "duplicate_reads",
    "deferred_reads",
    "input_tokens",
    "output_tokens",
    "cache_hit_tokens",
    "cached_share",
    "reduction_events",
    "rewritten_prefixes",
    "turn_boundaries",
    "held_prefixes",
    "lost_prefixes",
    "lost_cache_tokens",
    "repetition_nudges",
    "summary_calls",
    "summary_failures",
    "malformed_tool_calls",
    "context_overflow_recoveries",
    "requests",
    "turns",
];
const CONTEXT_FIELDS: [&str; 9] = [
    "elided_results",
    "cleared_reasoning",
    "shrunk_write_calls",
    "dropped_history_turns",
    "dropped_tool_calls",
    "truncated_results",
    "shrunk_latest_write_calls",
    "summarized_tool_calls",
    "superseded_results",
];

struct ContextCounts([(&'static str, usize); 9]);

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
            activity.superseded_results(),
        ];
        for ((_, total), count) in self.0.iter_mut().zip(counts) {
            *total += count;
        }
    }
}

/// One reported request: why it was sent, whether its output was rejected,
/// how long it took, what the provider reported, the capacity it was fitted
/// into, what the harness estimated before and after reduction, and how it
/// was reduced, with the index of its `turn` in the run and the digests of
/// the messages it `sent`, which [`metric::record_cache_reuse`] consumes.
fn request_record(request: &ModelRequestActivity, turn: usize, sent: Option<&Vec<u64>>) -> Value {
    let completion = request.completion();
    let usage = completion.and_then(|completion| completion.usage());
    let mut reductions = ContextCounts::default();
    reductions.add(request.context());
    let reductions: Map<String, Value> = reductions
        .0
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| (name.to_string(), json!(count)))
        .collect();

    json!({
        "turn": turn,
        "purpose": match request.purpose() {
            ModelRequestPurpose::Turn => "turn",
            ModelRequestPurpose::ContextSummary => "context_summary",
        },
        "ms": request.duration().as_millis(),
        "input_tokens": usage.and_then(|usage| usage.input_tokens()),
        "cache_hit_tokens": usage.and_then(|usage| usage.cache_hit_tokens()),
        "output_tokens": usage.and_then(|usage| usage.output_tokens()),
        "reasoning_tokens": usage.and_then(|usage| usage.reasoning_tokens()),
        "finish_reason": completion.map(CompletionMetadata::finish_reason),
        "rejected_output": request.rejected_output(),
        "budget_weight": request.budget_weight(),
        "unreduced_weight": request.unreduced_weight(),
        "estimated_weight": request.estimated_weight(),
        "calibrated_weight": request.calibrated_weight(),
        "reductions": reductions,
        "sent": sent,
    })
}
