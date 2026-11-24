//! Usage accounting for Claude Code `stream-json` output.
//!
//! Claude Code reports, per its Agent SDK cost-tracking documentation:
//!
//! - the usage of each API response on its assistant messages, which share one
//!   message id when a response calls several tools, and whose `output_tokens`
//!   is a placeholder;
//! - each turn's main-loop usage on that turn's `result`, which leaves out
//!   subagents;
//! - on each `result`, `modelUsage`: per model, the tokens and `costUSD` of the
//!   whole process so far, subagents included, plus a resumed session's earlier
//!   totals on Claude Code 2.1.277 and later.
//!
//! The Messages usage shape leaves cache reads and writes out of
//! `input_tokens`; this module counts input with both included. A turn's own
//! per-model usage is the difference between its `modelUsage` totals and the
//! totals after the session's previous resumed turn, which the host keeps as
//! an opaque checkpoint. Live updates use the main-loop usage at list price;
//! the turn result switches to Claude Code's own per-model figures.

use std::collections::{BTreeMap, HashMap, HashSet};

use ag_contracts::{AgentRequestKind, ContextFill, CostBasis, CostEstimate, ModelUsage, TurnUsage};
use ag_harness::provider::ModelProvider;
use ag_protocol::ProtocolRequestProfile;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::usage::list_price;

/// First Claude Code release whose resumed sessions restore their earlier
/// totals into `modelUsage`; earlier releases start each process at zero.
const CUMULATIVE_RESUME_VERSION: Version = Version::new(2, 1, 277);

/// Folds Claude Code stream events into the run's usage, under an empty
/// model name, and its `modelUsage` totals.
#[derive(Default)]
pub(crate) struct ClaudeUsageMeter {
    /// Fill of the latest main-loop response.
    context: Option<ContextFill>,
    /// Model of the latest main-loop response.
    context_model: Option<String>,
    /// Per-model totals of the latest result's `modelUsage`.
    model_totals: BTreeMap<String, ModelTotals>,
    /// Summed main-loop usage of the run's turn results.
    results: Option<ModelUsage>,
    /// Ids of the main-loop responses already counted.
    seen_messages: HashSet<String>,
    /// Summed usage of the main-loop responses, with placeholder output.
    steps: Option<ModelUsage>,
    /// Claude Code release from the `system/init` event.
    version: Option<Version>,
    /// Context window of each model, from a result's `modelUsage`.
    windows: HashMap<String, u64>,
}

impl ClaudeUsageMeter {
    /// Folds one stream event and returns whether it reported usage: a
    /// main-loop response not counted yet, or a turn result.
    pub(crate) fn observe(&mut self, event: &Value) -> bool {
        if event.get("type").and_then(Value::as_str) == Some("system")
            && let Some(version) = event
                .get("claude_code_version")
                .and_then(Value::as_str)
                .and_then(|version| Version::parse(version).ok())
        {
            self.version = Some(version);
        }
        if let Some(model_usage) = event.get("modelUsage").and_then(Value::as_object) {
            self.model_totals.clear();
            for (model, usage) in model_usage {
                if let Some(window) = usage.get("contextWindow").and_then(Value::as_u64) {
                    self.windows.insert(model.clone(), window);
                }
                if let Some(totals) = ModelTotals::parse(usage) {
                    self.model_totals.insert(model.clone(), totals);
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

    /// Returns the run's main-loop usage: its results' usage, or before any
    /// result the responses observed so far, or one unreported request when
    /// Claude reported nothing.
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

    /// Returns the run's `modelUsage` totals, or `None` before a result
    /// reports them.
    pub(crate) fn totals(&self) -> Option<AgentTotals> {
        (!self.model_totals.is_empty()).then(|| AgentTotals {
            context_model: self.context_model.clone(),
            models: self.model_totals.clone(),
            version: self.version.clone(),
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

/// The `modelUsage` totals one Claude Code process reported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentTotals {
    /// Model of the run's latest main-loop response, whose row carries the
    /// run's context fill.
    context_model: Option<String>,
    /// Cumulative totals of each model.
    models: BTreeMap<String, ModelTotals>,
    /// Claude Code release, when the run reported it.
    version: Option<Version>,
}

/// One model's cumulative counts in a result's `modelUsage`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ModelTotals {
    /// Input tokens read from the prompt cache.
    cache_read: u64,
    /// Input tokens written to the prompt cache.
    cache_write: u64,
    /// Claude Code's cost estimate in micro-USD, when reported.
    cost: Option<u64>,
    /// Input tokens without the cache.
    input: u64,
    /// Output tokens, thinking included.
    output: u64,
    /// Thinking tokens, when reported.
    thinking: Option<u64>,
}

impl ModelTotals {
    /// Totals of a model a process has not used yet.
    const UNUSED: Self = Self {
        cache_read: 0,
        cache_write: 0,
        cost: Some(0),
        input: 0,
        output: 0,
        thinking: Some(0),
    };

    /// Parses one `modelUsage` entry, which must report input and output.
    fn parse(usage: &Value) -> Option<Self> {
        let count = |field: &str| usage.get(field).and_then(Value::as_u64);

        Some(Self {
            cache_read: count("cacheReadInputTokens").unwrap_or(0),
            cache_write: count("cacheCreationInputTokens").unwrap_or(0),
            // A `costBasis` of `unknown` means no price table matched the
            // model, so its `costUSD` is no estimate.
            cost: usage
                .get("costUSD")
                .filter(|_| usage.get("costBasis").and_then(Value::as_str) != Some("unknown"))
                .and_then(Value::as_f64)
                .and_then(micro_usd),
            input: count("inputTokens")?,
            output: count("outputTokens")?,
            thinking: count("thinkingTokens"),
        })
    }

    /// Returns these totals minus the `earlier` ones, or `None` when a total
    /// went down, as it does when the totals restarted.
    fn since(self, earlier: Self) -> Option<Self> {
        let optional = |current: Option<u64>, earlier: Option<u64>| match (current, earlier) {
            (Some(current), Some(earlier)) => current.checked_sub(earlier).map(Some),
            _ => Some(None),
        };

        Some(Self {
            cache_read: self.cache_read.checked_sub(earlier.cache_read)?,
            cache_write: self.cache_write.checked_sub(earlier.cache_write)?,
            cost: optional(self.cost, earlier.cost)?,
            input: self.input.checked_sub(earlier.input)?,
            output: self.output.checked_sub(earlier.output)?,
            thinking: optional(self.thinking, earlier.thinking)?,
        })
    }

    /// Returns whether these counts include any token.
    fn has_tokens(self) -> bool {
        self.input > 0 || self.output > 0 || self.cache_read > 0 || self.cache_write > 0
    }

    /// Returns these counts as `model`'s usage of one request, priced at
    /// Claude Code's estimate when it reported one, else at list price.
    fn model_usage(self, model: &str) -> ModelUsage {
        let mut usage = ModelUsage {
            cache_read_tokens: Some(self.cache_read),
            cache_write_tokens: Some(self.cache_write),
            input_tokens: self
                .input
                .saturating_add(self.cache_read)
                .saturating_add(self.cache_write),
            output_tokens: self.output,
            reasoning_tokens: self.thinking,
            requests: 1,
            ..ModelUsage::new(model)
        };
        usage.cost = match self.cost {
            Some(micro_usd) => Some(CostEstimate {
                basis: CostBasis::AgentReported,
                micro_usd,
            }),
            None => list_price(&usage, ModelProvider::Anthropic),
        };

        usage
    }
}

/// One Claude Code process of a turn: its main-loop usage and its
/// `modelUsage` totals, when reported.
pub(crate) struct ClaudeRun<'a> {
    /// Main-loop usage, under an empty model name.
    pub(crate) main_loop: &'a ModelUsage,
    /// `modelUsage` totals of the process.
    pub(crate) totals: Option<&'a AgentTotals>,
}

/// Returns the usage of one turn's Claude Code processes on `model`, in the
/// order they ran, and the checkpoint the next continued turn needs.
///
/// A process that continued the conversation (a `SessionResume` request) is
/// charged the difference from `checkpoint`, the totals after the
/// conversation's previous turn. Any other process, such as a session's
/// first turn or a utility prompt, starts at zero and is charged its own
/// totals. A process whose share cannot be trusted, because it reported no
/// totals, its release is unknown, no checkpoint precedes it, or a total
/// went down, is charged its main-loop usage at list price instead. The
/// totals of a session turn's processes, started or continued, become the
/// next checkpoint; other requests never set one.
pub(crate) fn turn_usage<'a>(
    model: &str,
    request_kind: &AgentRequestKind,
    checkpoint: Option<&str>,
    runs: impl IntoIterator<Item = ClaudeRun<'a>>,
) -> TurnUsage {
    let resumed = request_kind.is_resume();
    let session_turn = request_kind.protocol_profile() == ProtocolRequestProfile::SessionTurn;
    let mut previous = checkpoint.and_then(|checkpoint| serde_json::from_str(checkpoint).ok());
    let mut usage = TurnUsage::default();
    for run in runs {
        let models = run
            .totals
            .and_then(|totals| run_share(totals, resumed, previous.as_ref()))
            .unwrap_or_else(|| {
                let mut main_loop = ModelUsage {
                    model: model.to_string(),
                    ..run.main_loop.clone()
                };
                main_loop.cost = list_price(&main_loop, ModelProvider::Anthropic);

                vec![main_loop]
            });
        let context_model = run
            .totals
            .and_then(|totals| totals.context_model.as_deref())
            .filter(|context_model| models.iter().any(|usage| usage.model == *context_model))
            .unwrap_or(model);
        for mut model_usage in models {
            if model_usage.model == context_model {
                model_usage.context = run.main_loop.context;
                model_usage.requests = model_usage.requests.max(run.main_loop.requests);
            }
            usage.record(&model_usage);
        }
        if session_turn && let Some(totals) = run.totals {
            usage.checkpoint = serde_json::to_string(&totals.models).ok();
            previous = Some(totals.models.clone());
        }
    }

    usage
}

/// Returns each model's share of one process's `totals`, or `None` when it
/// cannot be trusted. Models without new work are left out.
fn run_share(
    totals: &AgentTotals,
    resumed: bool,
    previous: Option<&BTreeMap<String, ModelTotals>>,
) -> Option<Vec<ModelUsage>> {
    let restores_earlier_totals = resumed && totals.version.as_ref()? >= &CUMULATIVE_RESUME_VERSION;
    let empty = BTreeMap::new();
    let previous = if restores_earlier_totals {
        previous?
    } else {
        &empty
    };

    totals
        .models
        .iter()
        .map(|(model, current)| {
            let earlier = previous.get(model).copied().unwrap_or(ModelTotals::UNUSED);

            current
                .since(earlier)
                .map(|share| share.has_tokens().then(|| share.model_usage(model)))
        })
        .collect::<Option<Vec<_>>>()
        .map(|shares| shares.into_iter().flatten().collect())
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
