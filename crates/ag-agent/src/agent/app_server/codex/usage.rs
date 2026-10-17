//! Codex app-server usage extraction helpers.

use serde_json::Value;

/// Retained thread counters, separated from the latest request's context size.
#[derive(Clone)]
pub(super) struct UsageTracker {
    pub(super) latest_input: Option<u64>,
    baseline_observed: bool,
    observed: bool,
    previous: Option<(u64, u64)>,
    previous_cached: Option<u64>,
    turn: Option<(u64, u64)>,
    turn_cached: Option<u64>,
}

impl UsageTracker {
    /// Resumed threads have no trustworthy cumulative baseline yet. The next
    /// complete snapshot reconstructs it by subtracting its latest response.
    pub(super) fn resumed(&mut self) {
        self.previous = None;
        self.previous_cached = None;
    }

    pub(super) fn begin_turn(&mut self) {
        self.baseline_observed = false;
        self.observed = false;
        self.turn = Some((0, 0));
        self.turn_cached = Some(0);
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

    pub(super) fn observe(&mut self, response: &Value, expected_turn: Option<&str>) {
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
            return;
        };
        self.observed = true;
        let last_breakdown = usage.get("last").or_else(|| usage.get("last_token_usage"));
        self.latest_input = last_breakdown
            .and_then(|last| last.get("inputTokens").or_else(|| last.get("input_tokens")))
            .and_then(Value::as_u64);
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
            let cached = total
                .get("cachedInputTokens")
                .or_else(|| total.get("cached_input_tokens"))
                .and_then(Value::as_u64);
            if self.previous_cached.is_none() {
                self.previous_cached = cached.and_then(|cached| {
                    preceding_total(
                        cached,
                        last_counter(last_breakdown, "cachedInputTokens", "cached_input_tokens"),
                    )
                });
            }
            self.turn_cached = self.turn_cached.zip(self.previous_cached).zip(cached).map(
                |((turn, previous), current)| {
                    turn.saturating_add(if current < previous {
                        current
                    } else {
                        current - previous
                    })
                },
            );
            self.previous_cached = cached;
        } else {
            // Last-only notifications cannot distinguish duplicate events
            // from distinct provider calls with equal usage.
            self.turn = None;
            self.turn_cached = None;
            self.baseline_observed = false;
        }
    }

    pub(super) fn totals(&self) -> Option<(u64, u64)> {
        self.turn
            .filter(|_| self.observed && self.previous.is_some())
    }

    pub(super) fn record(&self) {
        ag_telemetry::current_attribute("agentty.usage.aggregate_known", self.totals().is_some());
        if let Some(input) = self.latest_input {
            ag_telemetry::current_attribute(
                "agentty.context.last_request.input_tokens",
                i64::try_from(input).unwrap_or(i64::MAX),
            );
        }
        if let Some(cached) = self.turn_cached.filter(|_| self.totals().is_some()) {
            ag_telemetry::current_attribute(
                "gen_ai.usage.cache_read.input_tokens",
                i64::try_from(cached).unwrap_or(i64::MAX),
            );
        }
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self {
            baseline_observed: false,
            observed: false,
            previous: Some((0, 0)),
            previous_cached: Some(0),
            latest_input: None,
            turn: None,
            turn_cached: None,
        }
    }
}

/// Resolves final turn usage by preferring `turn/completed` payload usage and
/// falling back to the last per-request usage update when completion omits it.
pub(super) fn resolve_turn_usage(
    completed_turn_usage: Option<(u64, u64)>,
    latest_stream_usage: Option<(u64, u64)>,
) -> (u64, u64) {
    completed_turn_usage
        .or(latest_stream_usage)
        .unwrap_or((0, 0))
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
