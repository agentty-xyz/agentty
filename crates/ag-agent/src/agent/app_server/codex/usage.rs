//! Codex app-server usage extraction helpers, and detection of a `ChatGPT`
//! plan login through `account/read`.

use ag_contracts::{ContextFill, ModelUsage, PlanLogin};
use serde_json::Value;

use crate::agent::usage::Billing;

/// Retained thread counters, separated from the latest request's context size.
///
/// Codex reports `inputTokens` with the cached prompt included and
/// `outputTokens` with reasoning included, as the shared usage contract
/// counts them.
#[derive(Clone)]
pub(super) struct UsageTracker {
    pub(super) latest_input: Option<u64>,
    /// Id of the `account/read` request whose answer is pending.
    account_request: Option<String>,
    baseline_observed: bool,
    /// How the app server's login bills usage, metered until its account
    /// is known to be a `ChatGPT` plan.
    billing: Billing,
    cache_write: DetailCounter,
    cached: DetailCounter,
    /// Context window Codex reports for the thread's model.
    context_window: Option<u64>,
    observed: bool,
    /// Largest request input of the current turn.
    peak_input: Option<u64>,
    previous: Option<(u64, u64)>,
    reasoning: DetailCounter,
    /// Usage notifications of the current turn, one per model request.
    requests: u32,
    turn: Option<(u64, u64)>,
}

impl UsageTracker {
    /// Resumed threads have no trustworthy cumulative baseline yet. The next
    /// complete snapshot reconstructs it by subtracting its latest response.
    pub(super) fn resumed(&mut self) {
        self.previous = None;
        for detail in self.details_mut() {
            detail.previous = None;
        }
    }

    pub(super) fn begin_turn(&mut self) {
        self.baseline_observed = false;
        self.observed = false;
        self.peak_input = None;
        self.requests = 0;
        self.turn = Some((0, 0));
        for detail in self.details_mut() {
            detail.turn = Some(0);
        }
    }

    /// Discards stale baselines after turns lacking complete cumulative usage.
    /// A new snapshot still establishes a baseline when this turn's total is
    /// unknown.
    pub(super) fn finish_turn(&mut self) {
        if !self.baseline_observed {
            self.resumed();
        }
    }

    /// Native compaction invalidates the previous request's context estimate.
    pub(super) fn clear_context(&mut self) {
        self.latest_input = None;
    }

    /// Folds one app-server line and returns whether it was a usage
    /// notification of the expected turn.
    pub(super) fn observe(&mut self, response: &Value, expected_turn: Option<&str>) -> bool {
        if matches!(
            response.get("method").and_then(Value::as_str),
            Some("item/completed")
        ) && response
            .pointer("/params/item/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "contextCompaction" | "context_compaction"))
        {
            self.clear_context();
        }
        let Some(usage) = thread_token_usage_for_turn(response, expected_turn) else {
            if !self.observed
                && let Some(last) = extract_turn_usage_for_turn(response, expected_turn)
            {
                self.latest_input = Some(last.0);
            }
            return false;
        };
        self.observed = true;
        self.requests = self.requests.saturating_add(1);
        let window = usage
            .get("modelContextWindow")
            .or_else(|| usage.get("model_context_window"))
            .and_then(Value::as_u64);
        self.context_window = window.or(self.context_window);
        let last_breakdown = usage.get("last").or_else(|| usage.get("last_token_usage"));
        self.latest_input = last_breakdown
            .and_then(|last| last.get("inputTokens").or_else(|| last.get("input_tokens")))
            .and_then(Value::as_u64);
        self.peak_input = self.peak_input.max(self.latest_input);
        let total = usage
            .get("total")
            .or_else(|| usage.get("total_token_usage"));
        if let Some(total) = total {
            let current = (
                total
                    .get("inputTokens")
                    .or_else(|| total.get("input_tokens"))
                    .and_then(Value::as_u64),
                total
                    .get("outputTokens")
                    .or_else(|| total.get("output_tokens"))
                    .and_then(Value::as_u64),
            );
            if let (Some(input), Some(output)) = current {
                self.baseline_observed = true;
                if self.previous.is_none() {
                    self.previous = preceding_total(
                        input,
                        last_counter(last_breakdown, "inputTokens", "input_tokens"),
                    )
                    .zip(preceding_total(
                        output,
                        last_counter(last_breakdown, "outputTokens", "output_tokens"),
                    ));
                }
                let delta = self.previous.map(|previous| {
                    if input < previous.0 || output < previous.1 {
                        (input, output)
                    } else {
                        (input - previous.0, output - previous.1)
                    }
                });
                self.turn = self.turn.zip(delta).map(|(turn, delta)| {
                    (
                        turn.0.saturating_add(delta.0),
                        turn.1.saturating_add(delta.1),
                    )
                });
                self.previous = Some((input, output));
            } else {
                self.baseline_observed = false;
                self.turn = None;
            }
            for detail in self.details_mut() {
                detail.observe(total, last_breakdown);
            }
        } else {
            // Last-only notifications cannot distinguish duplicate events
            // from distinct provider calls with equal usage.
            self.turn = None;
            for detail in self.details_mut() {
                detail.turn = None;
            }
            self.baseline_observed = false;
        }

        true
    }

    pub(super) fn totals(&self) -> Option<(u64, u64)> {
        self.turn
            .filter(|_| self.observed && self.previous.is_some())
    }

    /// Returns the current turn's usage under an empty model name: the
    /// cumulative counters' turn total when known, else `fallback`, the
    /// usage `turn/completed` or the latest request reported. Details are
    /// known only with the cumulative total, and a turn without any count is
    /// one unreported request.
    pub(super) fn turn_usage(&self, fallback: Option<(u64, u64)>) -> ModelUsage {
        let totals = self.totals();
        let Some((input_tokens, output_tokens)) = totals.or(fallback) else {
            return ModelUsage::unreported("", self.requests.max(1));
        };
        let detail = |counter: &DetailCounter| counter.turn.filter(|_| totals.is_some());

        ModelUsage {
            cache_read_tokens: detail(&self.cached),
            cache_write_tokens: detail(&self.cache_write),
            context: self.latest_input.map(|last_input_tokens| ContextFill {
                last_input_tokens,
                peak_input_tokens: self.peak_input.unwrap_or(last_input_tokens),
                window_tokens: self.context_window,
            }),
            input_tokens,
            output_tokens,
            reasoning_tokens: detail(&self.reasoning),
            requests: self.requests.max(1),
            ..ModelUsage::new("")
        }
    }

    /// Returns an `account/read` request asking how the app server is
    /// signed in, and remembers its id so [`Self::observe_account`] reads
    /// the answer whenever it arrives.
    pub(super) fn account_request(&mut self) -> Value {
        let request_id = format!("account-read-{}", uuid::Uuid::new_v4());
        let payload = serde_json::json!({
            "method": "account/read",
            "id": request_id,
            "params": {}
        });
        self.account_request = Some(request_id);

        payload
    }

    /// Reads the answer to the pending `account/read` request and returns
    /// whether `response` was it. A `chatgpt` account bills usage to a
    /// `ChatGPT` plan; an API key, a cloud account, or an error response
    /// leaves usage metered.
    pub(super) fn observe_account(&mut self, response: &Value) -> bool {
        if self.account_request.is_none()
            || response.get("id").and_then(Value::as_str) != self.account_request.as_deref()
        {
            return false;
        }
        self.account_request = None;
        self.billing = if response
            .pointer("/result/account/type")
            .and_then(Value::as_str)
            == Some("chatgpt")
        {
            Billing::Plan(PlanLogin::ChatGpt)
        } else {
            Billing::Metered
        };

        true
    }

    /// Returns how the app server's login bills usage.
    pub(super) fn billing(&self) -> Billing {
        self.billing
    }

    pub(super) fn record(&self) {
        ag_telemetry::current_attribute("agentty.usage.aggregate_known", self.totals().is_some());
        if let Some(cached) = self.cached.turn.filter(|_| self.totals().is_some()) {
            ag_telemetry::current_attribute(
                "gen_ai.usage.cache_read.input_tokens",
                i64::try_from(cached).unwrap_or(i64::MAX),
            );
        }
    }

    /// Returns the optional detail counters.
    fn details_mut(&mut self) -> [&mut DetailCounter; 3] {
        [&mut self.cache_write, &mut self.cached, &mut self.reasoning]
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self {
            account_request: None,
            baseline_observed: false,
            billing: Billing::Metered,
            cache_write: DetailCounter::new("cacheWriteInputTokens", "cache_write_input_tokens"),
            cached: DetailCounter::new("cachedInputTokens", "cached_input_tokens"),
            context_window: None,
            latest_input: None,
            observed: false,
            peak_input: None,
            previous: Some((0, 0)),
            reasoning: DetailCounter::new("reasoningOutputTokens", "reasoning_output_tokens"),
            requests: 0,
            turn: None,
        }
    }
}

/// One optional cumulative counter of a thread's usage snapshots, such as
/// cached input, whose turn total stays unknown once a snapshot omits it.
#[derive(Clone)]
struct DetailCounter {
    /// Snapshot field name in camel case.
    camel_case: &'static str,
    /// Thread total before the current snapshot, or `None` until a snapshot
    /// reconstructs it.
    previous: Option<u64>,
    /// Snapshot field name in snake case.
    snake_case: &'static str,
    /// Current turn's total, or `None` when unknown.
    turn: Option<u64>,
}

impl DetailCounter {
    /// Creates a counter read from the named snapshot fields, with a zero
    /// baseline for a new thread.
    fn new(camel_case: &'static str, snake_case: &'static str) -> Self {
        Self {
            camel_case,
            previous: Some(0),
            snake_case,
            turn: None,
        }
    }

    /// Adds the difference between `total` and the previous snapshot to the
    /// turn, reconstructing a missing baseline from the latest response.
    fn observe(&mut self, total: &Value, last: Option<&Value>) {
        let current = total
            .get(self.camel_case)
            .or_else(|| total.get(self.snake_case))
            .and_then(Value::as_u64);
        if self.previous.is_none() {
            self.previous = current.and_then(|current| {
                preceding_total(
                    current,
                    last_counter(last, self.camel_case, self.snake_case),
                )
            });
        }
        self.turn = self
            .turn
            .zip(self.previous)
            .zip(current)
            .map(|((turn, previous), current)| {
                turn.saturating_add(if current < previous {
                    current
                } else {
                    current - previous
                })
            });
        self.previous = current;
    }
}

/// Updates usage trackers for one app-server response line.
///
/// Per-request usage is read from modern `thread/tokenUsage/updated`
/// notifications first, then legacy `turn.usage` payloads. Cumulative totals
/// belong only to the retained baseline tracker.
pub(super) fn update_turn_usage_from_response(
    response_value: &Value,
    expected_turn_id: Option<&str>,
    completed_turn_usage: &mut Option<(u64, u64)>,
    latest_stream_usage: &mut Option<(u64, u64)>,
) {
    if thread_token_usage_for_turn(response_value, expected_turn_id).is_some() {
        *latest_stream_usage =
            extract_thread_token_usage_for_turn(response_value, expected_turn_id);

        return;
    }

    if let Some(turn_usage) = extract_turn_usage_for_turn(response_value, expected_turn_id) {
        if response_value.get("method").and_then(Value::as_str) == Some("turn/completed") {
            *completed_turn_usage = Some(turn_usage);
        } else {
            *latest_stream_usage = Some(turn_usage);
        }
    }
}

/// Extracts input/output token usage from `turn.usage` payloads.
pub(super) fn extract_turn_usage(response_value: &Value) -> Option<(u64, u64)> {
    let turn = response_value
        .get("params")
        .and_then(|params| params.get("turn"))?;

    let usage = turn.get("usage")?;

    let input_tokens = usage
        .get("inputTokens")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

/// Extracts usage for the active turn, ignoring known delegated-turn payloads.
pub(super) fn extract_turn_usage_for_turn(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<(u64, u64)> {
    if let Some(expected_turn_id) = expected_turn_id {
        let turn_id = response_value
            .get("params")
            .and_then(|params| params.get("turn"))
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str);
        if turn_id.is_some_and(|payload_turn_id| payload_turn_id != expected_turn_id) {
            return None;
        }
    }

    extract_turn_usage(response_value)
}

/// Extracts only the latest request's usage, never cumulative thread totals.
pub(super) fn extract_thread_token_usage_for_turn(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<(u64, u64)> {
    let token_usage = thread_token_usage_for_turn(response_value, expected_turn_id)?;
    let breakdown = token_usage
        .get("last")
        .or_else(|| token_usage.get("last_token_usage"))?;

    let input_tokens = breakdown
        .get("inputTokens")
        .and_then(Value::as_u64)
        .or_else(|| breakdown.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = breakdown
        .get("outputTokens")
        .and_then(Value::as_u64)
        .or_else(|| breakdown.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

/// Validates ownership before exposing counters to either usage tracker.
fn thread_token_usage_for_turn<'response>(
    response_value: &'response Value,
    expected_turn_id: Option<&str>,
) -> Option<&'response Value> {
    let method = response_value.get("method").and_then(Value::as_str)?;
    if method != "thread/tokenUsage/updated" && method != "thread/token_usage/updated" {
        return None;
    }

    let params = response_value.get("params")?;
    if let Some(expected_turn_id) = expected_turn_id {
        let payload_turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .or_else(|| params.get("turn_id").and_then(Value::as_str));
        if payload_turn_id.is_some_and(|turn_id| turn_id != expected_turn_id) {
            return None;
        }
    }

    params
        .get("tokenUsage")
        .or_else(|| params.get("token_usage"))
}

/// Reads one counter from a snapshot's latest-response breakdown.
fn last_counter(breakdown: Option<&Value>, camel_case: &str, snake_case: &str) -> Option<u64> {
    breakdown
        .and_then(|last| last.get(camel_case).or_else(|| last.get(snake_case)))
        .and_then(Value::as_u64)
}

/// Returns the cumulative counter before a snapshot's latest response. Codex
/// adds exactly that response to the previous total for every snapshot.
fn preceding_total(total: u64, last: Option<u64>) -> Option<u64> {
    last.and_then(|last| total.checked_sub(last))
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
