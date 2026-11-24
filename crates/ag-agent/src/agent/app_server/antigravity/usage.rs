//! Antigravity per-turn usage accounting.

use std::collections::HashMap;

use ag_contracts::{ContextFill, ModelUsage};
use serde_json::Value;

/// Token counters from one Antigravity usage object.
///
/// Antigravity reports `input_tokens` without the cached prompt and
/// `cache_read_tokens` beside it: a resumed turn's step reports fewer input
/// than cache-read tokens. Its `total_tokens` is input plus output, so
/// `thinking_tokens` is part of `output_tokens`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenUsage {
    /// Cache-read tokens, beside `input`.
    pub(crate) cache_read: u64,
    /// Input tokens without the cached prompt.
    pub(crate) input: u64,
    /// Output tokens, thinking included.
    pub(crate) output: u64,
    /// Thinking tokens.
    pub(crate) thinking: u64,
}

impl TokenUsage {
    /// Parses one provider usage object, which must report input and output.
    pub(crate) fn parse(usage: &Value) -> Option<Self> {
        let count = |field: &str| usage.get(field).and_then(Value::as_u64);

        Some(Self {
            cache_read: count("cache_read_tokens").unwrap_or(0),
            input: count("input_tokens")?,
            output: count("output_tokens")?,
            thinking: count("thinking_tokens").unwrap_or(0),
        })
    }

    /// Returns these counters as one request's usage under an empty model
    /// name, with input normalized to include the cached prompt.
    pub(crate) fn model_usage(self) -> ModelUsage {
        ModelUsage {
            cache_read_tokens: Some(self.cache_read),
            input_tokens: self.input.saturating_add(self.cache_read),
            output_tokens: self.output,
            reasoning_tokens: Some(self.thinking),
            requests: 1,
            ..ModelUsage::new("")
        }
    }

    fn saturating_sub(self, previous: Self) -> Self {
        Self {
            cache_read: self.cache_read.saturating_sub(previous.cache_read),
            input: self.input.saturating_sub(previous.input),
            output: self.output.saturating_sub(previous.output),
            thinking: self.thinking.saturating_sub(previous.thinking),
        }
    }
}

/// Collects model usage from completed steps within the active turn.
#[derive(Default)]
pub(super) struct TurnUsageTracker {
    completed_steps: HashMap<u64, TokenUsage>,
}

impl TurnUsageTracker {
    /// Records the latest usage for one completed step event.
    pub(super) fn record_step(&mut self, step_update: &Value) {
        if !step_update
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("done"))
        {
            return;
        }
        let Some(step_index) = step_update.get("step_index").and_then(Value::as_u64) else {
            return;
        };
        let Some(step_usage) = step_update.get("usage").and_then(TokenUsage::parse) else {
            return;
        };

        self.completed_steps.insert(step_index, step_usage);
    }

    /// Selects per-turn usage, under an empty model name, and advances the
    /// cumulative result baseline.
    ///
    /// Completed steps are the turn's requests, and the latest step's prompt
    /// is the context the model last saw; without them, the result's
    /// cumulative counters minus the previous result's count as one request,
    /// and a result without counters as one unreported request.
    pub(super) fn finish(
        self,
        result: &Value,
        previous_cumulative: &mut Option<TokenUsage>,
    ) -> ModelUsage {
        let cumulative = result.get("usage").and_then(TokenUsage::parse);
        let mut steps = self.completed_steps.into_iter().collect::<Vec<_>>();
        steps.sort_unstable_by_key(|(step_index, _)| *step_index);
        let step_total = steps
            .into_iter()
            .map(|(_, step)| {
                let mut request = step.model_usage();
                request.context = Some(ContextFill::of_request(request.input_tokens, None));

                request
            })
            .reduce(|mut total, request| {
                total.add(&request);

                total
            });
        let result_usage = step_total.unwrap_or_else(|| match (cumulative, *previous_cumulative) {
            (Some(cumulative), Some(previous)) => cumulative.saturating_sub(previous).model_usage(),
            (Some(cumulative), None) => cumulative.model_usage(),
            (None, _) => ModelUsage::unreported("", 1),
        });
        if cumulative.is_some() {
            *previous_cumulative = cumulative;
        }

        result_usage
    }
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
