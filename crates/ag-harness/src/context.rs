//! Model-aware projection of bounded session history into provider requests.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::num::NonZeroU64;
use std::ops::Range;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::{ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments, ToolDefinition};
use crate::turn::{ContextActivity, TurnError, TurnOptions};

/// Share, in percent, of the request capacity left after reserved output
/// that an over-budget request is reduced to. Reducing below the budget
/// leaves room for the following tool rounds, so successive requests of a
/// turn reuse one reduced prefix instead of each clearing one more result.
pub(crate) const FIT_TARGET_PERCENT: u64 = 70;

/// Share, in percent, of the request capacity left after reserved output
/// that one tool result may keep once a request exceeds the budget; a
/// larger result is cut to a head and tail excerpt of this size before any
/// other reduction except clearing superseded results. A quarter still holds a
/// large file excerpt, while two capped results stay within
/// [`FIT_TARGET_PERCENT`] beside instructions and elided older rounds, so one
/// oversized output never forces earlier rounds out of the request.
pub(crate) const RESULT_CAP_PERCENT: u64 = 25;

/// Share, in percent, of the request capacity left after reserved output
/// that the latest round's results may keep together once a request exceeds
/// the budget; beyond it, its largest results are cut to one common size.
/// Half leaves a fifth of the capacity below [`FIT_TARGET_PERCENT`] for
/// instructions and the older rounds' elided notices.
pub(crate) const LATEST_ROUND_CAP_PERCENT: u64 = 50;

/// Placeholder that replaces cleared provider reasoning. Providers that
/// require reasoning on replayed assistant tool calls still receive a
/// non-empty value.
pub(crate) const CLEARED_REASONING: &str = "[reasoning cleared to fit the context window]";

/// Leading bytes of every cleared-result notice, so a notice loaded from
/// reduced history is never cleared again.
const CLEARED_RESULT_PREFIX: &str = r#"{"context_cleared":"#;

/// Leading bytes of the result that answers a repeated `read` without
/// running it, so that result never supersedes the read it points to.
const REPEATED_READ_PREFIX: &str = r#"{"unchanged":true,"#;

/// Outcome fields of a `bash` or `write` result kept in its cleared notice.
const OUTCOME_KEYS: [&str; 6] = [
    "execution_failure",
    "exit_code",
    "path",
    "signal",
    "status",
    "termination",
];

/// Longest outcome string value kept in a cleared notice.
const MAX_OUTCOME_VALUE_BYTES: usize = 256;

/// Multiple of the history byte limit up to which a turn too large to load
/// whole is still weighed in its elided form. A larger turn loads only in
/// brief form, so a store never reads more than this multiple of the limit
/// for one turn.
pub(crate) const ELIDED_SOURCE_LIMIT_FACTOR: usize = 8;

/// Approximate request-weight budget for one registered model configuration.
///
/// Weights are deterministic approximations produced by a [`ContextEstimator`];
/// they never claim exact provider token counts. Every harness requires one:
/// pass it to `Harness::new` or declare it in
/// [`crate::model::ModelCapabilities`], and revise the effective
/// [`crate::recovery::ExecutionIdentity`] when it changes. The budget governs
/// every provider request of a turn: the initial request keeps the most recent
/// turns that fit, each whole, with its tool traffic cleared, or as its input
/// and final output, and a request grown by in-turn tool traffic is reduced
/// before each follow-up model call by clearing older tool results that a
/// later call superseded, capping oversized tool results to head and tail
/// excerpts, clearing the other older tool results, reasoning, and write
/// patches, dropping the oldest history turns, then the current turn's oldest
/// tool rounds behind one note, and finally cutting the latest results further
/// and the latest write patches to their outcome.
/// The stored byte-based replay budget still bounds how much history is
/// loaded. Provider-reported input tokens calibrate the weights against this
/// budget in memory; until a provider reports usage, requests use 90% of the
/// capacity left after reserved output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextBudget {
    max_request_weight: NonZeroU64,
    reserved_output_weight: u64,
}

impl ContextBudget {
    /// Declares the total approximate weight available to one provider
    /// request.
    pub fn new(max_request_weight: NonZeroU64) -> Self {
        Self {
            max_request_weight,
            reserved_output_weight: 0,
        }
    }

    /// Reserves approximate weight for the model's response.
    ///
    /// # Errors
    /// Returns [`ContextBudgetError`] when the reservation leaves no request
    /// capacity.
    pub fn with_reserved_output(
        mut self,
        reserved_output_weight: u64,
    ) -> Result<Self, ContextBudgetError> {
        if reserved_output_weight >= self.max_request_weight.get() {
            return Err(ContextBudgetError::ReservedOutputExceedsBudget {
                max_request_weight: self.max_request_weight.get(),
                reserved_output_weight,
            });
        }
        self.reserved_output_weight = reserved_output_weight;

        Ok(self)
    }

    /// Returns the total approximate weight available to one request.
    pub fn max_request_weight(self) -> NonZeroU64 {
        self.max_request_weight
    }

    /// Returns the approximate weight reserved for the model's response.
    pub fn reserved_output_weight(self) -> u64 {
        self.reserved_output_weight
    }

    /// Returns this budget with `capacity` left after reserved output, at
    /// least one unit, and the same output reservation.
    pub(crate) fn with_capacity(self, capacity: u64) -> Self {
        let max_request_weight = self.reserved_output_weight.saturating_add(capacity.max(1));

        Self {
            max_request_weight: NonZeroU64::new(max_request_weight).unwrap_or(NonZeroU64::MAX),
            reserved_output_weight: self.reserved_output_weight,
        }
    }
}

/// Invalid context-budget configuration.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum ContextBudgetError {
    /// The output reservation consumes the entire request budget.
    #[error(
        "reserved output weight {reserved_output_weight} leaves no request capacity within \
         {max_request_weight}"
    )]
    ReservedOutputExceedsBudget {
        /// Declared total request weight.
        max_request_weight: u64,
        /// Rejected output reservation.
        reserved_output_weight: u64,
    },
}

/// Injectable approximate weighing of provider request content.
///
/// Estimates cover instructions, historical messages including tool groups,
/// and advertised tool definitions. The current ordered text/image input is
/// admitted as the canonical user message it becomes, so projection and
/// in-turn fitting share one accounting. Implementations must be
/// deterministic and use the same units as [`ContextBudget`]; estimates
/// never claim exact provider token counts.
pub trait ContextEstimator: Send + Sync {
    /// Approximates one system, user, or historical conversation message.
    fn message_weight(&self, message: &ModelMessage) -> u64;

    /// Approximates one advertised tool definition.
    fn tool_definition_weight(&self, tool: &ToolDefinition) -> u64;
}

/// Default byte-ratio estimator: one weight unit per four payload bytes plus
/// a fixed per-item overhead, with images at their base64 data-URL length.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeuristicContextEstimator;

impl HeuristicContextEstimator {
    const BYTES_PER_WEIGHT: u64 = 4;
    const ITEM_OVERHEAD_WEIGHT: u64 = 4;

    fn weigh_bytes(bytes: usize) -> u64 {
        u64::try_from(bytes)
            .unwrap_or(u64::MAX)
            .div_ceil(Self::BYTES_PER_WEIGHT)
            .saturating_add(Self::ITEM_OVERHEAD_WEIGHT)
    }
}

impl ContextEstimator for HeuristicContextEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        Self::weigh_bytes(message.retained_bytes())
    }

    fn tool_definition_weight(&self, tool: &ToolDefinition) -> u64 {
        let bytes = tool
            .name()
            .len()
            .saturating_add(tool.description().len())
            .saturating_add(tool.parameters().to_string().len());

        Self::weigh_bytes(bytes)
    }
}

/// One finished session turn loaded for replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryTurn {
    /// Persisted messages in order, starting with the turn's user input.
    pub messages: Vec<ModelMessage>,
    /// Why the turn stopped; `None` for a completed turn.
    pub stop: Option<TurnStop>,
}

impl HistoryTurn {
    /// Returns the messages replayed for this turn. A completed turn replays
    /// verbatim. A stopped turn adds a placeholder result for every tool call
    /// without a recorded result, in the request only, and ends with its stop
    /// note.
    pub(crate) fn replay(&self) -> Vec<ModelMessage> {
        let Some(stop) = &self.stop else {
            return self.messages.clone();
        };
        let mut replay = Vec::with_capacity(self.messages.len().saturating_add(1));
        let mut unanswered = Vec::new();
        for message in &self.messages {
            if let ModelMessage::ToolResult { call_id, .. } = message {
                unanswered.retain(|call: &&ToolCall| call.id() != call_id);
            } else {
                Self::push_placeholder_results(&mut replay, &mut unanswered);
                match message {
                    ModelMessage::AssistantToolCall(call) => unanswered.push(call),
                    ModelMessage::AssistantToolCalls(calls) => unanswered.extend(calls),
                    _ => {}
                }
            }
            replay.push(message.clone());
        }
        Self::push_placeholder_results(&mut replay, &mut unanswered);
        replay.push(stop.note());

        replay
    }

    /// Returns this turn with its tool traffic reduced the way an
    /// over-budget request reduces older rounds: tool results become
    /// tool-aware notices, assistant reasoning a placeholder, and `write`
    /// patches a note on their outcome. Tool calls keep their identifiers and
    /// other arguments, so tool groups stay well-formed.
    pub(crate) fn elided(&self) -> Self {
        let records = CallRecords::new(&self.messages);
        let messages = self
            .messages
            .iter()
            .map(|message| {
                [
                    Reduction::ElideResult,
                    Reduction::ClearReasoning,
                    Reduction::ShrinkWrite,
                ]
                .into_iter()
                .fold(message.clone(), |message, reduction| {
                    reduction.apply(&message, &records).unwrap_or(message)
                })
            })
            .collect();

        Self {
            messages,
            stop: self.stop.clone(),
        }
    }

    /// Returns this turn reduced to its input and, for a completed turn, its
    /// final output with reasoning cleared. A stopped turn still replays its
    /// stop note.
    pub(crate) fn brief(&self) -> Self {
        let output = self
            .messages
            .get(1..)
            .and_then(<[ModelMessage]>::last)
            .filter(|_| self.stop.is_none())
            .map(|output| {
                Reduction::ClearReasoning
                    .apply(output, &CallRecords::default())
                    .unwrap_or_else(|| output.clone())
            });

        Self {
            messages: self
                .messages
                .first()
                .cloned()
                .into_iter()
                .chain(output)
                .collect(),
            stop: self.stop.clone(),
        }
    }

    /// Returns the most complete form of this turn, full, then elided, then
    /// brief, whose retained bytes fit `max_bytes`. The elided form is only
    /// considered while the full turn stays within
    /// [`ELIDED_SOURCE_LIMIT_FACTOR`] times `max_history_bytes`.
    pub(crate) fn fit_bytes(self, max_bytes: usize, max_history_bytes: usize) -> Option<Self> {
        let full_bytes = retained_bytes(&self.messages);
        if full_bytes <= max_bytes {
            return Some(self);
        }
        let elided = (full_bytes <= max_history_bytes.saturating_mul(ELIDED_SOURCE_LIMIT_FACTOR))
            .then(|| self.elided());

        elided
            .into_iter()
            .chain([self.brief()])
            .find(|turn| retained_bytes(&turn.messages) <= max_bytes)
    }

    /// Answers each tool call still awaiting a result, so a stopped turn
    /// replays as a well-formed tool group.
    fn push_placeholder_results(replay: &mut Vec<ModelMessage>, unanswered: &mut Vec<&ToolCall>) {
        for call in unanswered.drain(..) {
            replay.push(ModelMessage::ToolResult {
                call_id: call.id().to_string(),
                content: r#"{"error":"no result was recorded before the turn stopped"}"#
                    .to_string(),
                name: call.name().to_string(),
            });
        }
    }
}

impl From<Vec<ModelMessage>> for HistoryTurn {
    /// Describes a completed turn.
    fn from(messages: Vec<ModelMessage>) -> Self {
        Self {
            messages,
            stop: None,
        }
    }
}

/// Why a replayed turn stopped and which of its effects have no recorded
/// outcome, as named by the turn's stop note.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TurnStop {
    /// Stored content-free error type, such as `cancelled` or `Tool`.
    pub error_type: String,
    /// Whether the turn failed rather than being interrupted.
    pub failed: bool,
    /// Shell sources of commands without a recorded outcome.
    pub unfinished_commands: Vec<String>,
    /// Repository paths of writes without a recorded outcome.
    pub unfinished_writes: Vec<String>,
}

impl TurnStop {
    const MAX_NAMED_EFFECTS: usize = 8;
    const MAX_NAMED_EFFECT_BYTES: usize = 200;

    /// Renders the bounded note that closes this turn's replay.
    fn note(&self) -> ModelMessage {
        let ending = if self.failed {
            format!("failed ({})", self.error_type)
        } else {
            format!("was interrupted ({})", self.error_type)
        };
        let mut note = format!(
            "[harness] The turn above {ending} before completing and shows only what it recorded."
        );
        if !self.unfinished_writes.is_empty() {
            note.push_str(" Writes with unknown outcome: ");
            note.push_str(&Self::name_effects(&self.unfinished_writes));
            note.push('.');
        }
        if !self.unfinished_commands.is_empty() {
            note.push_str(" Commands with unknown outcome: ");
            note.push_str(&Self::name_effects(&self.unfinished_commands));
            note.push('.');
        }

        ModelMessage::User(note)
    }

    fn name_effects(effects: &[String]) -> String {
        let mut named: Vec<String> = effects
            .iter()
            .take(Self::MAX_NAMED_EFFECTS)
            .map(|effect| {
                let mut end = effect.len().min(Self::MAX_NAMED_EFFECT_BYTES);
                while !effect.is_char_boundary(end) {
                    end -= 1;
                }

                format!("`{}`", &effect[..end])
            })
            .collect();
        let omitted = effects.len().saturating_sub(Self::MAX_NAMED_EFFECTS);
        if omitted > 0 {
            named.push(format!("and {omitted} more"));
        }

        named.join(", ")
    }
}

/// Admits the content every request must carry and returns the approximate
/// weight left for history.
///
/// The current input is weighed as the canonical user message the request
/// sends, so this admission matches [`ContextFit::fit`]'s per-message
/// accounting for the same content.
///
/// # Errors
/// Returns [`TurnError::ContextBudgetExceeded`] when instructions, the current
/// input, advertised tool definitions, and the reserved output cannot fit.
pub(crate) fn admit_mandatory_content(
    estimator: &dyn ContextEstimator,
    budget: ContextBudget,
    system_prompt: Option<&str>,
    input: &TurnInput,
    options: &TurnOptions,
) -> Result<u64, TurnError> {
    let mut required = budget.reserved_output_weight();
    if let Some(system_prompt) = system_prompt {
        let message = ModelMessage::System(system_prompt.to_string());
        required = required.saturating_add(estimator.message_weight(&message));
    }
    let user_message = input.clone().into_user_message();
    required = required.saturating_add(estimator.message_weight(&user_message));
    for tool in advertised_tools(options) {
        required = required.saturating_add(estimator.tool_definition_weight(&tool));
    }
    let budget_weight = budget.max_request_weight().get();
    if required > budget_weight {
        return Err(TurnError::ContextBudgetExceeded {
            budget: budget_weight,
            required,
        });
    }

    Ok(budget_weight - required)
}

/// Monotonic record of how earlier requests of one turn were reduced, with
/// each reduced message cached by its canonical index.
///
/// Canonical turn messages only grow by appending, so each reduction is a
/// watermark into them: every eligible message before it stays reduced in
/// later requests, which therefore share one stable prefix. Watermarks
/// advance only when a request exceeds the budget again; until then a
/// request only weighs its new messages and appends them to the cached
/// reduced request.
#[derive(Debug, Default)]
pub(crate) struct ContextFit {
    capped_results: usize,
    capped_results_before: usize,
    cleared_reasoning_before: usize,
    dropped_history_turns: usize,
    dropped_rounds: DroppedRounds,
    elided_results_before: usize,
    fitted: Option<ModelRequest>,
    messages: Vec<FitMessage>,
    records: CallRecords,
    reduced_counts: [usize; 3],
    required: u64,
    restore: Vec<(usize, ModelMessage)>,
    sent_weight: u64,
    shrunk_writes_before: usize,
    superseded_results: usize,
    truncated_latest: Vec<usize>,
}

impl ContextFit {
    /// Returns the approximate weight of the request the last
    /// [`Self::fit`] returned, as sent and without reserved output.
    pub(crate) fn sent_weight(&self) -> u64 {
        self.sent_weight
    }

    /// Returns the copy of `request` to send in its place, or `None` when the
    /// canonical request fits and no earlier request was reduced, with the
    /// reductions the copy carries.
    ///
    /// A request that exceeds the budget first clears older tool results
    /// that a later call of the request superseded, oldest first, until it
    /// weighs at most [`FIT_TARGET_PERCENT`] of its capacity (see
    /// [`Self::elide_superseded`]). It then caps every remaining tool result
    /// above [`RESULT_CAP_PERCENT`] of its capacity, and the latest round's
    /// largest results while together they exceed
    /// [`LATEST_ROUND_CAP_PERCENT`], to a head and tail excerpt, and is
    /// reduced to [`FIT_TARGET_PERCENT`] of its capacity, in order: the other
    /// older tool results (oldest first, replayed history before the current
    /// turn), then assistant reasoning, then
    /// `write` patches, then whole projected history turns, oldest first,
    /// then whole earlier tool rounds of the current turn, oldest first,
    /// replaced by one note naming what they did and read. The latest
    /// tool-call round is never reduced by these steps. Only when the request
    /// still exceeds the budget are the latest round's results, largest
    /// first, cut further, and as the last resort its `write` patches
    /// replaced by a note on their recorded outcome. Tool calls keep their
    /// identifiers so groups stay well-formed.
    ///
    /// # Errors
    /// Returns [`TurnError::ContextBudgetExceeded`] when even the fully
    /// reduced request does not fit.
    pub(crate) fn fit(
        &mut self,
        estimator: &dyn ContextEstimator,
        budget: ContextBudget,
        request: &ModelRequest,
    ) -> Result<(Option<&ModelRequest>, ContextActivity), TurnError> {
        let synced = self.messages.len();
        for message in &request.messages()[synced..] {
            let weight = estimator.message_weight(message);
            self.required = self.required.saturating_add(weight);
            self.records.record(message);
            self.messages.push(FitMessage {
                changes: 0,
                reduced: None,
                span: ReadSpan::parse(message),
                weight,
            });
        }
        self.truncated_latest.clear();
        let base = request
            .tools()
            .iter()
            .fold(budget.reserved_output_weight(), |weight, tool| {
                weight.saturating_add(estimator.tool_definition_weight(tool))
            });
        let budget_weight = budget.max_request_weight().get();
        let previous = self.fitted.take();
        let unreduced = self.capped_results_before == 0
            && self.cleared_reasoning_before == 0
            && self.dropped_history_turns == 0
            && self.dropped_rounds.rounds == 0
            && self.elided_results_before == 0
            && self.shrunk_writes_before == 0;
        if previous.is_none() && unreduced && base.saturating_add(self.required) <= budget_weight {
            self.sent_weight = base.saturating_add(self.required) - budget.reserved_output_weight();

            return Ok((None, ContextActivity::default()));
        }
        let layout = Layout::new(request);
        let changed = base.saturating_add(self.required) > budget_weight
            && self.extend(estimator, budget, request, &layout, base);
        let mut fitted = match previous {
            Some(mut fitted) if !changed => {
                for (index, message) in self.restore.drain(..) {
                    fitted.messages_mut()[index] = message;
                }
                fitted
                    .messages_mut()
                    .extend_from_slice(&request.messages()[synced..]);

                fitted
            }
            _ => {
                self.restore.clear();
                self.build(request, &layout)
            }
        };
        let mut required = base.saturating_add(self.required);
        let mut truncated_results = 0;
        let mut shrunk_latest_write_calls = 0;
        if required > budget_weight {
            truncated_results = self.truncate_latest_round(
                estimator,
                request,
                &mut fitted,
                layout.latest_round,
                (&mut required, budget_weight),
            );
        }
        if required > budget_weight {
            shrunk_latest_write_calls = self.shrink_latest_writes(
                estimator,
                request,
                &mut fitted,
                layout.latest_round,
                &mut required,
            );
        }
        if required > budget_weight {
            return Err(TurnError::ContextBudgetExceeded {
                budget: budget_weight,
                required,
            });
        }
        let activity = ContextActivity::new(
            self.reduced_counts[Reduction::ClearReasoning.slot()],
            self.dropped_history_turns,
            self.dropped_rounds.calls,
            self.reduced_counts[Reduction::ElideResult.slot()],
            shrunk_latest_write_calls,
            self.reduced_counts[Reduction::ShrinkWrite.slot()],
            self.capped_results + truncated_results,
        )
        .with_superseded_results(self.superseded_results);
        self.sent_weight = required - budget.reserved_output_weight();

        Ok((Some(&*self.fitted.insert(fitted)), activity))
    }

    /// Returns whether the last request sent the current turn's canonical
    /// message at `index` whole: neither reduced, cut, nor left out. A
    /// message appended after that request is still whole.
    pub(crate) fn sends_whole(&self, index: usize) -> bool {
        index >= self.dropped_rounds.end
            && self
                .messages
                .get(index)
                .is_none_or(|entry| entry.changes == 0)
            && !self.truncated_latest.contains(&index)
    }

    /// Elides superseded older results, caps oversized tool results, then
    /// reduces messages past the watermarks, tier by tier, until the request
    /// weighs at most [`FIT_TARGET_PERCENT`] of its capacity, advances the
    /// watermarks past what it reduced, and returns whether any sent
    /// message changed.
    fn extend(
        &mut self,
        estimator: &dyn ContextEstimator,
        budget: ContextBudget,
        request: &ModelRequest,
        layout: &Layout<'_>,
        base: u64,
    ) -> bool {
        let reserved = budget.reserved_output_weight();
        let capacity = budget.max_request_weight().get() - reserved;
        let target = reserved.saturating_add(capacity.saturating_mul(FIT_TARGET_PERCENT) / 100);
        let latest_round = layout.latest_round;
        let mut changed = false;
        let mut tier = Tier {
            base,
            changed: &mut changed,
            estimator,
            messages: request.messages(),
            target,
        };
        // Superseded results go first, so none is kept as a capped excerpt.
        self.elide_superseded(&mut tier, latest_round);
        if self.cap_results(estimator, request, latest_round, capacity) {
            *tier.changed = true;
        }
        self.elided_results_before = self.reduce(
            &mut tier,
            Reduction::ElideResult,
            self.elided_results_before..latest_round,
        );
        self.cleared_reasoning_before = self.reduce(
            &mut tier,
            Reduction::ClearReasoning,
            self.cleared_reasoning_before..latest_round,
        );
        self.shrunk_writes_before = self.reduce(
            &mut tier,
            Reduction::ShrinkWrite,
            self.shrunk_writes_before..latest_round,
        );
        while base.saturating_add(self.required) > target && self.drop_history_turn(layout) {
            changed = true;
        }
        while base.saturating_add(self.required) > target
            && self.drop_round(estimator, request, layout)
        {
            changed = true;
        }

        changed
    }

    /// Cuts each tool result not yet considered whose weight exceeds
    /// [`RESULT_CAP_PERCENT`] of `capacity` to a head and tail excerpt of
    /// that weight, and the latest round's results to one common weight
    /// that keeps them together within [`LATEST_ROUND_CAP_PERCENT`]; caches
    /// each excerpt so later requests carry it unchanged, and returns whether
    /// any result was cut.
    fn cap_results(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        latest_round: usize,
        capacity: u64,
    ) -> bool {
        let messages = request.messages();
        let result_cap = capacity.saturating_mul(RESULT_CAP_PERCENT) / 100;
        let latest_weights = (latest_round..messages.len())
            .filter(|index| matches!(messages[*index], ModelMessage::ToolResult { .. }))
            .map(|index| self.messages[index].weight.min(result_cap))
            .collect();
        let round_cap = Self::common_cap(
            latest_weights,
            capacity.saturating_mul(LATEST_ROUND_CAP_PERCENT) / 100,
        )
        .min(result_cap);
        let mut changed = false;
        // Older results were considered before any later tier could reduce
        // or drop them, so every result past the watermark is still whole.
        for (index, message) in messages.iter().enumerate().skip(self.capped_results_before) {
            let ModelMessage::ToolResult {
                call_id,
                content,
                name,
            } = message
            else {
                continue;
            };
            let cap = if index > latest_round {
                round_cap
            } else {
                result_cap
            };
            let current = self.messages[index].weight;
            if current <= cap {
                continue;
            }
            let excerpt = Self::excerpt_within(estimator, (call_id, content, name), cap);
            let weight = estimator.message_weight(&excerpt);
            if weight < current {
                self.required -= current - weight;
                self.capped_results += 1;
                let entry = &mut self.messages[index];
                entry.changes |= CAPPED_FLAG;
                entry.reduced = Some(excerpt);
                entry.weight = weight;
                changed = true;
            }
        }
        self.capped_results_before = messages.len();

        changed
    }

    /// Returns the largest weight that, applied as a cap to each of
    /// `weights`, keeps their sum within `share`; `u64::MAX` when they
    /// already fit.
    fn common_cap(mut weights: Vec<u64>, share: u64) -> u64 {
        let mut rest = weights
            .iter()
            .fold(0_u64, |sum, weight| sum.saturating_add(*weight));
        if rest <= share {
            return u64::MAX;
        }
        weights.sort_unstable_by_key(|weight| Reverse(*weight));

        weights
            .iter()
            .enumerate()
            .find_map(|(position, weight)| {
                rest -= weight;
                let capped = u64::try_from(position + 1).unwrap_or(u64::MAX);
                let cap = share.saturating_sub(rest) / capped;

                weights
                    .get(position + 1)
                    .is_none_or(|next| cap >= *next)
                    .then_some(cap)
            })
            .unwrap_or(0)
    }

    /// Returns the tool result `(call_id, content, name)` cut to the longest
    /// head and tail excerpt that weighs at most `allowed`, or to the bare
    /// notice when none does.
    fn excerpt_within(
        estimator: &dyn ContextEstimator,
        (call_id, content, name): (&str, &str, &str),
        allowed: u64,
    ) -> ModelMessage {
        let excerpt = |kept_bytes| ModelMessage::ToolResult {
            call_id: call_id.to_string(),
            content: truncated_result(content, kept_bytes),
            name: name.to_string(),
        };
        let (mut low, mut high) = (0, content.len());
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if estimator.message_weight(&excerpt(middle)) <= allowed {
                low = middle;
            } else {
                high = middle - 1;
            }
        }

        excerpt(low)
    }

    /// Elides the tool results past the elision watermark, before the
    /// latest round, that a later call of the request superseded, oldest
    /// first, until the request weighs at most the tier's target. A result is
    /// superseded when a later call re-read the same target over at least
    /// its lines, applied a write to the file it read, or reran the same
    /// command; its notice names that call. Staleness is weighed only while
    /// a request exceeds the budget, and a result elided this way stays
    /// elided, so later requests keep sharing the reduced prefix.
    fn elide_superseded(&mut self, tier: &mut Tier<'_>, latest_round: usize) {
        let superseded = find_superseded(
            tier.messages,
            &self.messages,
            self.elided_results_before..latest_round,
        );
        for (index, supersession) in superseded {
            if tier.base.saturating_add(self.required) <= tier.target {
                return;
            }
            if self.messages[index].changes & Reduction::ElideResult.flag() != 0 {
                continue;
            }
            let Some(reduced) =
                elided_result(&tier.messages[index], &self.records, Some(&supersession))
            else {
                continue;
            };
            if self.replace(tier, index, reduced, Reduction::ElideResult) {
                self.messages[index].changes |= SUPERSEDED_FLAG;
                self.superseded_results += 1;
            }
        }
    }

    /// Applies `reduction` to cached messages in `range`, in order, until
    /// the request weighs at most the tier's target, and returns the index it
    /// stopped at.
    fn reduce(&mut self, tier: &mut Tier<'_>, reduction: Reduction, range: Range<usize>) -> usize {
        for index in range.clone() {
            if tier.base.saturating_add(self.required) <= tier.target {
                return index;
            }
            let entry = &self.messages[index];
            // A superseded result was already elided ahead of this tier.
            if entry.changes & reduction.flag() != 0 {
                continue;
            }
            // A notice describes the original result, not its capped excerpt.
            let current = match reduction {
                Reduction::ElideResult => &tier.messages[index],
                Reduction::ClearReasoning | Reduction::ShrinkWrite => {
                    entry.reduced.as_ref().unwrap_or(&tier.messages[index])
                }
            };
            if let Some(reduced) = reduction.apply(current, &self.records) {
                self.replace(tier, index, reduced, reduction);
            }
        }

        range.end
    }

    /// Caches `reduced` in place of the message at `index` when it weighs
    /// less, and returns whether it did.
    fn replace(
        &mut self,
        tier: &mut Tier<'_>,
        index: usize,
        reduced: ModelMessage,
        reduction: Reduction,
    ) -> bool {
        let weight = tier.estimator.message_weight(&reduced);
        let entry = &mut self.messages[index];
        if weight >= entry.weight {
            return false;
        }
        // Each reduction's watermark only advances, so it changes a message
        // at most once.
        self.required -= entry.weight - weight;
        self.reduced_counts[reduction.slot()] += 1;
        if entry.changes & CAPPED_FLAG != 0 {
            entry.changes &= !CAPPED_FLAG;
            self.capped_results -= 1;
        }
        entry.changes |= reduction.flag();
        entry.reduced = Some(reduced);
        entry.weight = weight;
        *tier.changed = true;

        true
    }

    /// Leaves the oldest remaining projected history turn out of the
    /// request; returns `false` when none remains.
    fn drop_history_turn(&mut self, layout: &Layout<'_>) -> bool {
        let Some(start) = layout
            .history_turn_starts
            .get(self.dropped_history_turns)
            .copied()
        else {
            return false;
        };
        self.dropped_history_turns += 1;
        let end = layout
            .history_turn_starts
            .get(self.dropped_history_turns)
            .copied()
            .unwrap_or(layout.input_index);
        self.leave_out(start..end);

        true
    }

    /// Leaves the oldest remaining earlier tool round of the current turn,
    /// its call message with every result and any request-only message
    /// before it, out of the request and refreshes the note that stands in
    /// for removed rounds; returns `false` when no round precedes the latest
    /// one, including when the current turn has no tool round.
    fn drop_round(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        layout: &Layout<'_>,
    ) -> bool {
        let messages = request.messages();
        let start = self.dropped_rounds.end.max(layout.input_index + 1);
        let Some(call_index) = messages
            .get(start..layout.latest_round)
            .and_then(|earlier| earlier.iter().position(is_tool_call_message))
            .map(|offset| start + offset)
        else {
            return false;
        };
        let end = messages[call_index + 1..layout.latest_round]
            .iter()
            .position(is_tool_call_message)
            .map_or(layout.latest_round, |offset| call_index + 1 + offset);
        self.dropped_rounds
            .record(tool_calls(&messages[call_index]), &self.records);
        self.leave_out(start..end);
        self.required = self
            .required
            .saturating_sub(self.dropped_rounds.note_weight);
        self.dropped_rounds.note_weight = estimator.message_weight(&self.dropped_rounds.note());
        self.required = self
            .required
            .saturating_add(self.dropped_rounds.note_weight);
        self.dropped_rounds.end = end;

        true
    }

    /// Removes the cached messages in `range` from the request's weight and
    /// reduction counts.
    fn leave_out(&mut self, range: Range<usize>) {
        for entry in &self.messages[range] {
            self.required = self.required.saturating_sub(entry.weight);
            if entry.changes & CAPPED_FLAG != 0 {
                self.capped_results -= 1;
            }
            if entry.changes & SUPERSEDED_FLAG != 0 {
                self.superseded_results -= 1;
            }
            for reduction in [
                Reduction::ClearReasoning,
                Reduction::ElideResult,
                Reduction::ShrinkWrite,
            ] {
                if entry.changes & reduction.flag() != 0 {
                    self.reduced_counts[reduction.slot()] -= 1;
                }
            }
        }
    }

    /// Assembles the reduced request from the cached messages, leaving out
    /// dropped history turns and dropped rounds.
    fn build(&self, request: &ModelRequest, layout: &Layout<'_>) -> ModelRequest {
        let history_start = layout
            .history_turn_starts
            .first()
            .copied()
            .unwrap_or(layout.input_index);
        let history_end = layout
            .history_turn_starts
            .get(self.dropped_history_turns)
            .copied()
            .unwrap_or(layout.input_index);
        let rounds_start = layout.input_index + 1;
        let rounds_end = self.dropped_rounds.end.max(rounds_start);
        let mut messages = Vec::with_capacity(request.messages().len());
        for (index, message) in request.messages().iter().enumerate() {
            if index == rounds_start && rounds_end > rounds_start {
                messages.push(self.dropped_rounds.note());
            }
            if (history_start..history_end).contains(&index)
                || (rounds_start..rounds_end).contains(&index)
            {
                continue;
            }
            messages.push(
                self.messages[index]
                    .reduced
                    .clone()
                    .unwrap_or_else(|| message.clone()),
            );
        }

        request.with_fitted_messages(messages)
    }

    /// Cuts the latest round's tool results in `fitted`, largest first, to
    /// the longest head and tail excerpt that brings the request within the
    /// budget, records how to restore them, and returns how many results not
    /// already capped it cut.
    fn truncate_latest_round(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        fitted: &mut ModelRequest,
        latest_round: usize,
        (required, budget_weight): (&mut u64, u64),
    ) -> usize {
        let shift = request.messages().len() - fitted.messages().len();
        let mut results: Vec<(usize, &str, &str, &str)> = request.messages()[latest_round..]
            .iter()
            .enumerate()
            .filter_map(|(offset, message)| match message {
                ModelMessage::ToolResult {
                    call_id,
                    content,
                    name,
                } => Some((
                    latest_round + offset,
                    call_id.as_str(),
                    content.as_str(),
                    name.as_str(),
                )),
                _ => None,
            })
            .collect();
        results.sort_by_key(|(index, ..)| Reverse(self.messages[*index].weight));
        let mut truncated = 0;
        for (index, call_id, content, name) in results {
            if *required <= budget_weight {
                break;
            }
            let current = self.messages[index].weight;
            let allowed = current.saturating_sub(*required - budget_weight);
            let excerpt = Self::excerpt_within(estimator, (call_id, content, name), allowed);
            let weight = estimator.message_weight(&excerpt);
            if weight < current {
                *required -= current - weight;
                self.replace_temporarily(fitted, index - shift, excerpt);
                self.truncated_latest.push(index);
                // A capped result is already counted.
                if self.messages[index].changes & CAPPED_FLAG == 0 {
                    truncated += 1;
                }
            }
        }

        truncated
    }

    /// Replaces the patches of the latest round's `write` calls in `fitted`
    /// with a note on their recorded outcome, records how to restore them,
    /// and returns how many calls it changed.
    fn shrink_latest_writes(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        fitted: &mut ModelRequest,
        latest_round: usize,
        required: &mut u64,
    ) -> usize {
        let Some(message) = request.messages().get(latest_round) else {
            return 0;
        };
        let Some(reduced) = Reduction::ShrinkWrite.apply(message, &self.records) else {
            return 0;
        };
        let weight = estimator.message_weight(&reduced);
        let current = self.messages[latest_round].weight;
        if weight >= current {
            return 0;
        }
        let changed_calls = match (message, &reduced) {
            (
                ModelMessage::AssistantToolCalls(calls),
                ModelMessage::AssistantToolCalls(reduced),
            ) => calls
                .iter()
                .zip(reduced)
                .filter(|(call, reduced)| call != reduced)
                .count(),
            _ => 1,
        };
        *required -= current - weight;
        let shift = request.messages().len() - fitted.messages().len();
        self.replace_temporarily(fitted, latest_round - shift, reduced);

        changed_calls
    }

    /// Replaces one message of `fitted` for this request only.
    fn replace_temporarily(
        &mut self,
        fitted: &mut ModelRequest,
        index: usize,
        message: ModelMessage,
    ) {
        let original = std::mem::replace(&mut fitted.messages_mut()[index], message);
        self.restore.push((index, original));
    }
}

/// Selects the most recent finished turns that fit `max_bytes` of retained
/// history, newest first, each in the most complete form that fits what is
/// left, and stops at the first turn that does not fit even in brief form.
/// A turn larger than [`ELIDED_SOURCE_LIMIT_FACTOR`] times `max_bytes` is
/// never elided. Returns the kept turns oldest first.
pub(crate) fn fit_history_bytes(
    newest_first: impl IntoIterator<Item = HistoryTurn>,
    max_bytes: usize,
) -> Vec<HistoryTurn> {
    let mut remaining = max_bytes;
    let mut kept = Vec::new();
    for turn in newest_first {
        let Some(turn) = turn.fit_bytes(remaining, max_bytes) else {
            break;
        };
        remaining -= retained_bytes(&turn.messages);
        kept.push(turn);
    }
    kept.reverse();

    kept
}

/// Returns, oldest first, the replays of the most recent finished turns whose
/// combined weight fits `available_weight`, dropping every turn older than
/// the first that does not fit, so a turn's tool groups are never split. Each
/// turn replays in full, else with its tool traffic cleared, else as its input
/// and final output or stop note. Returns the projected turns with the number
/// of loaded turns that projection evicted.
pub(crate) fn select_recent_turns(
    estimator: &dyn ContextEstimator,
    turns: &VecDeque<HistoryTurn>,
    available_weight: u64,
) -> (Vec<Vec<ModelMessage>>, usize) {
    let mut kept = Vec::new();
    let mut remaining = available_weight;
    for turn in turns.iter().rev() {
        let forms: [&dyn Fn() -> Vec<ModelMessage>; 3] =
            [&|| turn.replay(), &|| turn.elided().replay(), &|| {
                turn.brief().replay()
            }];
        let Some((replay, weight)) = forms
            .iter()
            .map(|form| {
                let replay = form();
                let weight = weigh_messages(estimator, &replay);

                (replay, weight)
            })
            .find(|(_, weight)| *weight <= remaining)
        else {
            break;
        };
        remaining -= weight;
        kept.push(replay);
    }
    let dropped_turns = turns.len().saturating_sub(kept.len());
    kept.reverse();

    (kept, dropped_turns)
}

/// Returns the definitions the engine advertises for these options, in order.
pub(crate) fn advertised_tools(options: &TurnOptions) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();
    if options.tool_policy().allows(Tool::Read) {
        tools.push(ToolDefinition::read_with_comparison_base(
            options.comparison_base(),
        ));
    }
    if options.tool_policy().allows(Tool::Write) {
        tools.push(ToolDefinition::write());
    }
    if options.tool_policy().allows(Tool::Bash) {
        tools.push(ToolDefinition::bash());
    }

    tools
}

/// One message reduction, in the order an over-budget request applies them.
#[derive(Clone, Copy)]
enum Reduction {
    ClearReasoning,
    ElideResult,
    ShrinkWrite,
}

impl Reduction {
    /// Returns `message` reduced, or `None` when this reduction does not
    /// apply or would not shrink it.
    fn apply(self, message: &ModelMessage, records: &CallRecords) -> Option<ModelMessage> {
        match (self, message) {
            (Self::ElideResult, _) => elided_result(message, records, None),
            (
                Self::ClearReasoning,
                ModelMessage::AssistantReasoning {
                    content,
                    reasoning_content,
                },
            ) if reasoning_content.len() > CLEARED_REASONING.len() => {
                Some(ModelMessage::AssistantReasoning {
                    content: content.clone(),
                    reasoning_content: CLEARED_REASONING.to_string(),
                })
            }
            (Self::ClearReasoning, _) => reduce_calls(message, cleared_reasoning_call),
            (Self::ShrinkWrite, _) => reduce_calls(message, |call| {
                shrunk_write_call(call, &records.write_outcomes)
            }),
        }
    }

    /// Position of this reduction's count among the cached counts.
    fn slot(self) -> usize {
        match self {
            Self::ClearReasoning => 0,
            Self::ElideResult => 1,
            Self::ShrinkWrite => 2,
        }
    }

    /// Change flag recorded for a message this reduction changed.
    fn flag(self) -> u8 {
        1 << self.slot()
    }
}

/// Change flag recorded for a tool result capped to an excerpt, beside the
/// [`Reduction`] flags.
const CAPPED_FLAG: u8 = 1 << 3;

/// Change flag recorded, beside [`Reduction::ElideResult`]'s, for a tool
/// result elided because a later call superseded it.
const SUPERSEDED_FLAG: u8 = 1 << 4;

/// What the calls of one request recorded, by call identifier: each `read`
/// call's target and each `write` result's `status`.
#[derive(Debug, Default)]
struct CallRecords {
    read_targets: HashMap<String, String>,
    write_outcomes: HashMap<String, String>,
}

impl CallRecords {
    fn new(messages: &[ModelMessage]) -> Self {
        let mut records = Self::default();
        for message in messages {
            records.record(message);
        }

        records
    }

    /// Remembers the read targets of a tool-call message and the outcome of
    /// a `write` result.
    fn record(&mut self, message: &ModelMessage) {
        for call in tool_calls(message) {
            if let Some(arguments) = call.read_arguments() {
                self.read_targets
                    .insert(call.id().to_string(), read_target(arguments));
            }
        }
        if let Some((call_id, status)) = write_outcome(message) {
            self.write_outcomes.insert(call_id, status);
        }
    }
}

/// Cached form of one canonical message of the current request.
#[derive(Debug)]
struct FitMessage {
    changes: u8,
    reduced: Option<ModelMessage>,
    /// Lines returned by a successful `file` or `show` read result.
    span: Option<ReadSpan>,
    weight: u64,
}

/// One-based line window that a successful `file` or `show` read returned.
#[derive(Clone, Copy, Debug, Deserialize)]
struct ReadSpan {
    end_line: Option<u64>,
    start_line: u64,
}

impl ReadSpan {
    /// Returns the window a `read` result reports, or `None` for any other
    /// message, a rejected read, or an inspection without line numbers.
    fn parse(message: &ModelMessage) -> Option<Self> {
        let ModelMessage::ToolResult { content, name, .. } = message else {
            return None;
        };
        if name != "read" {
            return None;
        }

        serde_json::from_str(content).ok()
    }

    /// Whether this window includes every line of `earlier`; an empty
    /// earlier window only needs the same or an earlier start.
    fn covers(self, earlier: Self) -> bool {
        self.start_line <= earlier.start_line
            && earlier
                .end_line
                .is_none_or(|end| self.end_line.is_some_and(|own_end| own_end >= end))
    }
}

/// The later call that made an earlier tool result stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Supersession<'a> {
    by: &'a str,
    cause: SupersessionCause,
}

/// How a later call superseded an earlier tool result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SupersessionCause {
    /// A later read returned the same target over at least the same lines.
    Reread,
    /// The same shell command ran again.
    Rerun,
    /// A later applied write changed the file the read showed.
    Write,
}

impl SupersessionCause {
    /// Returns the notice text for a result this cause superseded.
    fn notice(self) -> &'static str {
        match self {
            Self::Reread => {
                "This earlier read result was cleared to fit the context window because a later \
                 read of the same target, named in superseded_by, returned it again. Use that \
                 newer result instead of reading it again."
            }
            Self::Rerun => {
                "This earlier command result was cleared to fit the context window because the \
                 same command ran again later, in the call named in superseded_by. That newer \
                 result supersedes it; do NOT repeat the command to recover this output."
            }
            Self::Write => {
                "This earlier read result was cleared to fit the context window because a later \
                 write, named in superseded_by, changed the file it showed. If you still need the \
                 file, read it again, narrowed with offset/limit."
            }
        }
    }
}

/// Earlier tool rounds of the current turn left out of the request, and the
/// effects their stand-in note names.
#[derive(Debug, Default)]
struct DroppedRounds {
    calls: usize,
    commands: Vec<String>,
    end: usize,
    note_weight: u64,
    reads: Vec<String>,
    rounds: usize,
    writes: Vec<String>,
}

impl DroppedRounds {
    /// Counts one dropped round's calls and remembers the paths of its
    /// applied writes, the sources of its commands, and its distinct read
    /// targets.
    fn record(&mut self, calls: &[ToolCall], records: &CallRecords) {
        self.rounds += 1;
        self.calls += calls.len();
        for call in calls {
            match call.arguments() {
                ToolCallArguments::Bash(arguments) => {
                    self.commands.push(arguments.command().to_string());
                }
                ToolCallArguments::Read(arguments) => {
                    let target = read_target(arguments);
                    if !self.reads.contains(&target) {
                        self.reads.push(target);
                    }
                }
                ToolCallArguments::Write(arguments)
                    if records.write_outcomes.get(call.id()).map(String::as_str)
                        == Some("applied") =>
                {
                    self.writes.push(arguments.path().to_string());
                }
                ToolCallArguments::Write(_) | ToolCallArguments::Malformed(_) => {}
            }
        }
    }

    /// Renders the bounded note that stands in for the dropped rounds.
    fn note(&self) -> ModelMessage {
        let mut note = format!(
            "[harness] {} earlier tool call(s) of this turn were removed with their results to \
             fit the context window. Writes and commands among them already took effect; do not \
             repeat them only to recover their output.",
            self.calls
        );
        if !self.writes.is_empty() {
            note.push_str(" Writes applied: ");
            note.push_str(&TurnStop::name_effects(&self.writes));
            note.push('.');
        }
        if !self.commands.is_empty() {
            note.push_str(" Commands run: ");
            note.push_str(&TurnStop::name_effects(&self.commands));
            note.push('.');
        }
        if !self.reads.is_empty() {
            note.push_str(" Reads whose results were removed: ");
            note.push_str(&TurnStop::name_effects(&self.reads));
            note.push_str(
                ". Re-read only what you still need, narrowed with offset/limit, instead of \
                 repeating these reads.",
            );
        }

        ModelMessage::User(note)
    }
}

/// Where the parts of one canonical request start.
struct Layout<'a> {
    history_turn_starts: &'a [usize],
    input_index: usize,
    /// Index of the current turn's latest tool-call message, or the message
    /// count when the turn has none. Request-only messages, such as output
    /// feedback, may follow it.
    latest_round: usize,
}

impl<'a> Layout<'a> {
    fn new(request: &'a ModelRequest) -> Self {
        let input_index = request.input_index();
        let messages = request.messages();
        let latest_round = messages[input_index..]
            .iter()
            .rposition(is_tool_call_message)
            .map_or(messages.len(), |offset| input_index + offset);

        Self {
            history_turn_starts: request.history_turn_starts(),
            input_index,
            latest_round,
        }
    }
}

/// Shared inputs of one reduction tier.
struct Tier<'a> {
    base: u64,
    changed: &'a mut bool,
    estimator: &'a dyn ContextEstimator,
    messages: &'a [ModelMessage],
    target: u64,
}

/// Returns the calls an assistant tool-call message carries; empty for any
/// other message.
fn tool_calls(message: &ModelMessage) -> &[ToolCall] {
    match message {
        ModelMessage::AssistantToolCall(call) => std::slice::from_ref(call),
        ModelMessage::AssistantToolCalls(calls) => calls,
        _ => &[],
    }
}

fn is_tool_call_message(message: &ModelMessage) -> bool {
    !tool_calls(message).is_empty()
}

/// Returns, oldest first, each tool result whose index lies in `range` that
/// a later call of `messages` superseded, with the nearest such call.
/// Results pair with the latest earlier call of their identifier, so
/// identifiers reused across turns stay distinct.
fn find_superseded<'a>(
    messages: &'a [ModelMessage],
    cached: &[FitMessage],
    range: Range<usize>,
) -> Vec<(usize, Supersession<'a>)> {
    let mut calls: Vec<(&ToolCall, Option<usize>)> = Vec::new();
    let mut awaiting: HashMap<&str, usize> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if let ModelMessage::ToolResult { call_id, .. } = message {
            if let Some(position) = awaiting.remove(call_id.as_str()) {
                calls[position].1 = Some(index);
            }
            continue;
        }
        for call in tool_calls(message) {
            awaiting.insert(call.id(), calls.len());
            calls.push((call, None));
        }
    }

    calls
        .iter()
        .enumerate()
        .filter_map(|(position, (call, result))| {
            let result = result.filter(|result| range.contains(result))?;
            let supersession = calls[position + 1..]
                .iter()
                .find_map(|(later, later_result)| {
                    let later_result = (*later_result)?;
                    let cause = supersession_cause(
                        (call, &cached[result]),
                        (later, &messages[later_result], &cached[later_result]),
                    )?;

                    Some(Supersession {
                        by: later.id(),
                        cause,
                    })
                })?;

            Some((result, supersession))
        })
        .collect()
}

/// Returns how the `later` call, with its result, superseded the `earlier`
/// call's result, or `None` when it did not.
fn supersession_cause(
    (earlier, earlier_cached): (&ToolCall, &FitMessage),
    (later, later_result, later_cached): (&ToolCall, &ModelMessage, &FitMessage),
) -> Option<SupersessionCause> {
    match (earlier.arguments(), later.arguments()) {
        (ToolCallArguments::Read(earlier), ToolCallArguments::Read(later))
            if !is_repeated_read(later_result)
                && rereads(earlier, earlier_cached.span, later, later_cached.span) =>
        {
            Some(SupersessionCause::Reread)
        }
        (ToolCallArguments::Read(earlier), ToolCallArguments::Write(later))
            if write_outcome(later_result).is_some_and(|(_, status)| status == "applied")
                && read_shows_path(earlier, later.path()) =>
        {
            Some(SupersessionCause::Write)
        }
        (ToolCallArguments::Bash(earlier), ToolCallArguments::Bash(later))
            if earlier.command() == later.command() =>
        {
            Some(SupersessionCause::Rerun)
        }
        _ => None,
    }
}

/// Whether the `later` read returned the `earlier` read's target again over
/// at least its result: the same action, path, query, and side, with a line
/// window covering the earlier one for `file` and `show`, or the same or a
/// larger limit otherwise. An earlier `file` or `show` read that returned no
/// window, such as a rejection, is covered by any later read of its target.
fn rereads(
    earlier: &ReadArguments,
    earlier_span: Option<ReadSpan>,
    later: &ReadArguments,
    later_span: Option<ReadSpan>,
) -> bool {
    if earlier.action() != later.action()
        || earlier.path_filter() != later.path_filter()
        || earlier.query() != later.query()
        || earlier.side() != later.side()
    {
        return false;
    }

    match earlier.action() {
        ReadAction::File | ReadAction::Show => match (earlier_span, later_span) {
            (None, _) => true,
            (Some(earlier), later) => later.is_some_and(|later| later.covers(earlier)),
        },
        ReadAction::Diff | ReadAction::List | ReadAction::Search => {
            match (earlier.limit(), later.limit()) {
                (Some(earlier), Some(later)) => later >= earlier,
                (earlier, later) => earlier == later,
            }
        }
    }
}

/// Whether a worktree write to `path` changes what `read` showed: a `file`
/// read of that path, or a `diff` whose path filter includes it.
fn read_shows_path(read: &ReadArguments, path: &str) -> bool {
    match read.action() {
        ReadAction::File => read.path() == path,
        ReadAction::Diff => read.path_filter().is_none_or(|filter| {
            path.strip_prefix(filter)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        }),
        ReadAction::List | ReadAction::Search | ReadAction::Show => false,
    }
}

/// Whether `message` answers a repeated `read` without having run it.
fn is_repeated_read(message: &ModelMessage) -> bool {
    matches!(
        message,
        ModelMessage::ToolResult { content, .. } if content.starts_with(REPEATED_READ_PREFIX)
    )
}

/// Describes what a `read` call inspected: its action, then any query,
/// path, revision side, and line window, such as
/// `file src/lib.rs offset=40 limit=20`.
fn read_target(arguments: &ReadArguments) -> String {
    let mut target = arguments.action().as_str().to_string();
    if let Some(query) = arguments.query() {
        let _ = write!(target, " {query:?}");
    }
    if let Some(path) = arguments.path_filter() {
        if arguments.query().is_some() {
            target.push_str(" in");
        }
        let _ = write!(target, " {path}");
    }
    if let Some(side) = arguments.side() {
        target.push_str(&format!(" {side:?}").to_ascii_lowercase());
    }
    if let Some(offset) = arguments.offset() {
        let _ = write!(target, " offset={offset}");
    }
    if let Some(limit) = arguments.limit() {
        let _ = write!(target, " limit={limit}");
    }

    target
}

/// Returns a `write` result's call identifier and recorded `status`.
fn write_outcome(message: &ModelMessage) -> Option<(String, String)> {
    let ModelMessage::ToolResult {
        call_id,
        content,
        name,
    } = message
    else {
        return None;
    };
    if name != "write" {
        return None;
    }
    let result = serde_json::from_str::<Value>(content).ok()?;

    Some((call_id.clone(), result.get("status")?.as_str()?.to_string()))
}

/// Applies `reduce_call` to each call of an assistant tool-call message and
/// returns the message when any call changed.
fn reduce_calls(
    message: &ModelMessage,
    reduce_call: impl Fn(&ToolCall) -> Option<ToolCall>,
) -> Option<ModelMessage> {
    match message {
        ModelMessage::AssistantToolCall(call) => {
            reduce_call(call).map(ModelMessage::AssistantToolCall)
        }
        ModelMessage::AssistantToolCalls(calls) => {
            let reduced: Vec<Option<ToolCall>> = calls.iter().map(&reduce_call).collect();

            reduced.iter().any(Option::is_some).then(|| {
                ModelMessage::AssistantToolCalls(
                    reduced
                        .into_iter()
                        .zip(calls)
                        .map(|(reduced, call)| reduced.unwrap_or_else(|| call.clone()))
                        .collect(),
                )
            })
        }
        _ => None,
    }
}

/// Rebuilds a tool call with its provider reasoning replaced by
/// [`CLEARED_REASONING`], keeping its identifier and arguments.
fn cleared_reasoning_call(call: &ToolCall) -> Option<ToolCall> {
    call.reasoning_content()
        .filter(|reasoning| reasoning.len() > CLEARED_REASONING.len())?;

    ToolCall::from_json(
        call.id().to_string(),
        call.name(),
        &call.arguments_json().ok()?,
        Some(CLEARED_REASONING.to_string()),
    )
    .ok()
}

/// Rebuilds a `write` call with its patch replaced by a note on the outcome
/// its result recorded, keeping its identifier, path, and reasoning.
fn shrunk_write_call(
    call: &ToolCall,
    write_outcomes: &HashMap<String, String>,
) -> Option<ToolCall> {
    let arguments = call.write_arguments()?;
    let patch = match write_outcomes.get(call.id()).map(String::as_str) {
        Some("applied") => {
            "[patch cleared to fit the context window; this write was applied, so read the file \
             for its current content]"
        }
        Some("rejected") => {
            "[patch cleared to fit the context window; this write was rejected and changed nothing]"
        }
        _ => "[patch cleared to fit the context window; see this call's result]",
    };
    if arguments.patch().len() <= patch.len() {
        return None;
    }

    ToolCall::from_json(
        call.id().to_string(),
        call.name(),
        &json!({"patch": patch, "path": arguments.path()}).to_string(),
        call.reasoning_content().map(str::to_string),
    )
    .ok()
}

/// Returns `message` as the notice that stands in for its cleared result,
/// naming the later call that superseded it, if any; `None` for any other
/// message, a result already cleared, or one the notice would not shrink.
fn elided_result(
    message: &ModelMessage,
    records: &CallRecords,
    superseded: Option<&Supersession<'_>>,
) -> Option<ModelMessage> {
    let ModelMessage::ToolResult {
        call_id,
        content,
        name,
    } = message
    else {
        return None;
    };
    if content.starts_with(CLEARED_RESULT_PREFIX) {
        return None;
    }
    let notice = cleared_result_notice(
        name,
        content,
        records.read_targets.get(call_id).map(String::as_str),
        superseded,
    );

    (notice.len() < content.len()).then(|| ModelMessage::ToolResult {
        call_id: call_id.clone(),
        content: notice,
        name: name.clone(),
    })
}

/// Renders the notice that stands in for one cleared tool result. A `read`
/// may be repeated to recover its content, so its notice names the target
/// when `read_target` is known; any other call already ran, so its notice
/// keeps the recorded outcome fields and warns against repeating it. A
/// superseded result's notice says why and names the superseding call.
fn cleared_result_notice(
    name: &str,
    content: &str,
    read_target: Option<&str>,
    superseded: Option<&Supersession<'_>>,
) -> String {
    let is_read = name == "read";
    let text = match superseded {
        Some(supersession) => supersession.cause.notice(),
        None if is_read => {
            "This earlier read result was cleared to fit the context window. If you still need it, \
             read it again, narrowed with offset/limit."
        }
        None => {
            "This earlier tool result was cleared to fit the context window. The call already ran \
             with the outcome recorded here; do NOT repeat it to recover its output."
        }
    };
    let mut notice = Map::new();
    notice.insert("context_cleared".to_string(), text.into());
    notice.insert("original_bytes".to_string(), content.len().into());
    if let Some(supersession) = superseded {
        notice.insert("superseded_by".to_string(), supersession.by.into());
    }
    if is_read {
        if let Some(target) = read_target {
            notice.insert("read".to_string(), target.into());
        }

        return Value::Object(notice).to_string();
    }
    if let Ok(Value::Object(result)) = serde_json::from_str::<Value>(content) {
        for key in OUTCOME_KEYS {
            let Some(value) = result.get(key) else {
                continue;
            };
            let short_scalar = match value {
                Value::Bool(_) | Value::Number(_) => true,
                Value::String(text) => text.len() <= MAX_OUTCOME_VALUE_BYTES,
                Value::Array(_) | Value::Null | Value::Object(_) => false,
            };
            if short_scalar {
                notice.insert(key.to_string(), value.clone());
            }
        }
    }

    Value::Object(notice).to_string()
}

/// Renders the short result that answers a `read` repeating the earlier
/// call `same_as_call` of the current turn, whose result the request still
/// carries whole, with no write or command run since.
pub(crate) fn repeated_read_result(same_as_call: &str) -> String {
    let note = "This read repeats an earlier read of this turn whose result is shown above, and \
                nothing was written or run since, so that result is unchanged. Use it instead of \
                reading again.";

    // Fields are written in this order so the result starts with
    // `REPEATED_READ_PREFIX`.
    format!(
        "{REPEATED_READ_PREFIX}\"same_as_call\":{},\"note\":{}}}",
        Value::from(same_as_call),
        Value::from(note)
    )
}

/// Renders a tool result cut to its first and last `kept_bytes`, split
/// evenly, as JSON that tells the model how to request the rest.
fn truncated_result(content: &str, kept_bytes: usize) -> String {
    let head_end = content.floor_char_boundary(kept_bytes / 2);
    let tail_start = content
        .ceil_char_boundary(content.len() - (kept_bytes - kept_bytes / 2))
        .max(head_end);

    json!({
        "context_truncated": "This tool result was cut to its head and tail to fit the context \
                              window. Request a narrower result (offset/limit, a narrower path or \
                              search) if you need the omitted middle.",
        "head": &content[..head_end],
        "omitted_bytes": tail_start - head_end,
        "tail": &content[tail_start..],
    })
    .to_string()
}

fn weigh_messages(estimator: &dyn ContextEstimator, messages: &[ModelMessage]) -> u64 {
    messages.iter().fold(0_u64, |weight, message| {
        weight.saturating_add(estimator.message_weight(message))
    })
}

pub(crate) fn retained_bytes(messages: &[ModelMessage]) -> usize {
    messages.iter().fold(0, |bytes, message| {
        bytes.saturating_add(message.retained_bytes())
    })
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
