//! Usage accounting for Claude Code `stream-json` output.
//!
//! Claude Code reports, per its cost-tracking documentation, the usage of
//! each API response on its assistant messages, which share one message id
//! when a response calls several tools, and each turn's main-loop usage on
//! that turn's `result`. Both use the Anthropic Messages usage shape, whose
//! `input_tokens` leave out `cache_read_input_tokens` and
//! `cache_creation_input_tokens`; this module counts input with both
//! included. An assistant message's `output_tokens` is a placeholder until
//! the turn's `result` reports the real count.

use std::collections::{BTreeMap, HashMap, HashSet};

use ag_contracts::{ContextFill, ModelUsage};
use serde_json::Value;

/// Folds Claude Code stream events into the run's usage, under an empty
/// model name.
#[derive(Default)]
pub(crate) struct ClaudeUsageMeter {
    /// Fill of the latest main-loop response.
    context: Option<ContextFill>,
    /// Model of the latest main-loop response.
    context_model: Option<String>,
    /// Claude Code's own running cost estimate of each model, in micro-USD,
    /// from the latest result's `modelUsage`.
    reported_costs: BTreeMap<String, u64>,
    /// Summed usage of the run's turn results.
    results: Option<ModelUsage>,
    /// Ids of the main-loop responses already counted.
    seen_messages: HashSet<String>,
    /// Summed usage of the main-loop responses, with placeholder output.
    steps: Option<ModelUsage>,
    /// Context window of each model, from a result's `modelUsage`.
    windows: HashMap<String, u64>,
}

impl ClaudeUsageMeter {
    /// Folds one stream event and returns whether it reported usage: a
    /// main-loop response not counted yet, or a turn result.
    pub(crate) fn observe(&mut self, event: &Value) -> bool {
        if let Some(model_usage) = event.get("modelUsage").and_then(Value::as_object) {
            for (model, usage) in model_usage {
                if let Some(window) = usage.get("contextWindow").and_then(Value::as_u64) {
                    self.windows.insert(model.clone(), window);
                }
                if let Some(cost) = usage
                    .get("costUSD")
                    .and_then(Value::as_f64)
                    .and_then(micro_usd)
                {
                    self.reported_costs.insert(model.clone(), cost);
                }
            }
        }
        if let Some(usage) = event.get("usage") {
            fold(&mut self.results, &response_usage(usage));

            return true;
        }

        self.observe_response(event)
    }

    /// Folds one stream line: a single event, or an array of events as one
    /// complete `json` payload carries them.
    pub(crate) fn observe_payload(&mut self, payload: &Value) {
        match payload.as_array() {
            Some(events) => {
                for event in events {
                    self.observe(event);
                }
            }
            None => {
                self.observe(payload);
            }
        }
    }

    /// Returns the run's usage: its results' usage, or before any result
    /// the responses observed so far, or one unreported request when Claude
    /// reported nothing.
    pub(crate) fn usage(&self) -> ModelUsage {
        let mut usage = self
            .results
            .clone()
            .or_else(|| self.steps.clone())
            .unwrap_or_else(|| ModelUsage::unreported("", 1));
        let responses = u32::try_from(self.seen_messages.len()).unwrap_or(u32::MAX);
        usage.requests = usage.requests.max(responses);
        usage.context = self.context.map(|context| ContextFill {
            window_tokens: self
                .context_model
                .as_ref()
                .and_then(|model| self.windows.get(model))
                .copied(),
            ..context
        });

        usage
    }

    /// Returns Claude Code's own cost estimate of every model, subagents
    /// included, in micro-USD, or `None` before a result reports one.
    ///
    /// The estimate is cumulative: it counts every turn of the process, and
    /// a resumed session's earlier spend that Claude Code restores, so a
    /// turn's own cost is the difference from the previous turn's estimate.
    pub(crate) fn reported_cost(&self) -> Option<u64> {
        (!self.reported_costs.is_empty()).then(|| {
            self.reported_costs
                .values()
                .fold(0, |total: u64, cost| total.saturating_add(*cost))
        })
    }

    /// Counts one main-loop assistant response once per message id. Responses
    /// of subagents, which name the tool call that started them, are left
    /// out, as their turn results leave them out.
    fn observe_response(&mut self, event: &Value) -> bool {
        if event.get("type").and_then(Value::as_str) != Some("assistant")
            || event
                .get("parent_tool_use_id")
                .is_some_and(|parent| !parent.is_null())
        {
            return false;
        }
        let Some(message) = event.get("message") else {
            return false;
        };
        let (Some(id), Some(usage)) = (
            message.get("id").and_then(Value::as_str),
            message.get("usage"),
        ) else {
            return false;
        };
        if !self.seen_messages.insert(id.to_string()) {
            return false;
        }
        let response = response_usage(usage);
        let fill = ContextFill::of_request(response.input_tokens, None);
        self.context = Some(self.context.map_or(fill, |context| context.then(fill)));
        self.context_model = message
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string);
        fold(&mut self.steps, &response);

        true
    }
}

/// Converts one Messages usage object into one request's usage, with input
/// counting cache reads and writes.
fn response_usage(usage: &Value) -> ModelUsage {
    let count = |field: &str| usage.get(field).and_then(Value::as_u64);
    let input = count("input_tokens");
    let output = count("output_tokens");
    let cache_read = count("cache_read_input_tokens");
    let cache_write = count("cache_creation_input_tokens");

    ModelUsage {
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        input_tokens: input
            .unwrap_or(0)
            .saturating_add(cache_read.unwrap_or(0))
            .saturating_add(cache_write.unwrap_or(0)),
        output_tokens: output.unwrap_or(0),
        requests: 1,
        unreported_requests: u32::from(input.is_none() || output.is_none()),
        ..ModelUsage::new("")
    }
}

/// Converts a dollar amount into whole micro-USD, rounded, or `None` for a
/// negative or non-finite amount.
fn micro_usd(dollars: f64) -> Option<u64> {
    format!("{:.0}", dollars * 1_000_000.0).parse().ok()
}

/// Adds `next` to `total`, starting the total when empty.
fn fold(total: &mut Option<ModelUsage>, next: &ModelUsage) {
    match total {
        Some(total) => total.add(next),
        None => *total = Some(next.clone()),
    }
}

#[cfg(test)]
#[path = "claude_usage_test.rs"]
mod tests;
