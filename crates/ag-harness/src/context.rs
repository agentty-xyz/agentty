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
use crate::lifecycle::ContextSummaryTrigger;
use crate::model::{ModelMessage, ModelRequest};
use crate::round_summary::{self, RoundSummaryJob, SOURCE_TEXT_LEVELS};
use crate::schema_contract::OutputSchema;
use crate::tool::{ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments, ToolDefinition};
use crate::turn::{ContextActivity, TurnError, TurnOptions};
use crate::{compaction, read};

/// Share, in percent, of the request capacity left after reserved output
/// that an over-budget request's results, reasoning, and `write` patches are
/// cleared down to. A request is reduced only once it exceeds the budget,
/// and then deeply, so the following tool rounds append to one reduced
/// prefix for several requests instead of each clearing one more result.
pub(crate) const FIT_TARGET_PERCENT: u64 = 50;

/// Share, in percent, of the request capacity left after reserved output
/// that dropping whole history turns and earlier tool rounds stops at. A
/// dropped round also loses the text the model wrote beside its calls, so
/// rounds are dropped only down to this shallower share, once clearing could
/// not reach [`FIT_TARGET_PERCENT`].
pub(crate) const DROP_TARGET_PERCENT: u64 = 70;

/// Tool results of the current turn, newest first and by whole rounds, that
/// an over-budget request keeps whole, with their rounds' reasoning and
/// patches, until every older result, reasoning, and patch is cleared. The
/// latest round is always kept, however many results it holds.
pub(crate) const KEEP_RECENT_RESULTS: usize = 5;

/// Share, in percent, of the request capacity left after reserved output
/// that one tool result may keep. A `read` result is cut to it as it runs;
/// any other result of the latest round still larger once a request exceeds
/// the budget is cut to it while the round is new, so the cut costs no cached
/// prefix. A quarter still holds a large file page, so one oversized output
/// never forces earlier rounds out of the request.
pub(crate) const RESULT_CAP_PERCENT: u64 = 25;

/// Share, in percent, of the request capacity left after reserved output
/// that the latest round's results may keep together. A `read` that runs
/// past it is deferred with the call that fetches it; once a request exceeds
/// the budget, the new round's largest results are cut to one common size
/// within it. Half leaves a fifth of the capacity below
/// [`DROP_TARGET_PERCENT`] for instructions and the older rounds' notices.
pub(crate) const LATEST_ROUND_CAP_PERCENT: u64 = 50;

/// Share, in percent, of the request capacity left after reserved output
/// that the current turn's latest earlier rounds keep whole when the rounds
/// before them are summarized; the latest round is always kept.
pub(crate) const SUMMARY_KEEP_TAIL_PERCENT: u64 = 25;

/// Share, in percent, of the request capacity left after reserved output
/// that earlier rounds not yet summarized must weigh before a request that
/// outgrew the budget asks for a summary, so the extra model request only
/// runs when it replaces enough content.
pub(crate) const SUMMARY_MIN_SOURCE_PERCENT: u64 = 20;

/// Share, in percent, of the request capacity left after reserved output
/// that one summary may use, bounded by [`compaction::MAX_SUMMARY_BYTES`].
pub(crate) const SUMMARY_MAX_PERCENT: u64 = 15;

/// Payload bytes per weight unit that convert a summary's share of the
/// capacity into its byte limit, at the default estimator's rate.
const SUMMARY_BYTES_PER_WEIGHT: u64 = 4;

/// Failed summaries after which a turn stops requesting them and only
/// reduces its requests deterministically.
pub(crate) const MAX_SUMMARY_FAILURES_PER_TURN: usize = 2;

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

/// Leading bytes of the result that stands in for a `read` whose result its
/// round had no room for, so that result never supersedes an earlier read.
const DEFERRED_READ_PREFIX: &str = r#"{"deferred":"#;

/// Notice that replaces a repeated-read result once the earlier result it
/// points to was cleared.
const CLEARED_REPEAT_NOTICE: &str = "This read repeated an earlier read whose result was since \
                                     cleared or cut to fit the context window, so it is not shown \
                                     whole. If you still need it, read it again, narrowed with \
                                     offset/limit.";

/// Notice that replaces a repeated-read result once the earlier result it
/// points to was cleared because a later read returned it again.
const SUPERSEDED_REPEAT_NOTICE: &str =
    "This read repeated an earlier read whose result was since cleared because a later read, \
     named in superseded_by, returned it again. Use that newer result instead of reading it again.";

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
/// pass it to `Harness::new` or derive it from a registration's
/// [`crate::model::ModelProfile`]. Like the profile, the budget never enters
/// request fingerprints or stored history. The budget governs
/// every provider request of a turn: the initial request keeps the most recent
/// turns that fit, each whole, with its tool traffic cleared, or as its input
/// and final output. Within a turn, requests only append until one exceeds
/// the budget; that request is reduced once, deeply, by clearing older tool
/// results that a later call superseded, capping the new round's oversized
/// tool results (a `read` result by whole lines, as when it ran, any other to
/// a head and tail excerpt), clearing the other older tool results, then
/// reasoning and write patches, keeping the newest results whole longest,
/// dropping the oldest history turns, then the current turn's oldest tool
/// rounds behind an appended note, and finally cutting the latest results
/// further and the latest write patches to their outcome. Every reduced
/// message keeps its reduced form, so the following requests append to it.
/// With [`ContextSummaryPolicy::Enabled`], earlier tool rounds may instead be
/// replaced by a model-written summary before those steps.
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

/// Whether a turn may ask the model to summarize its earlier tool rounds
/// once a request outgrows the [`ContextBudget`].
///
/// A summary costs one extra tool-free model request and replaces the
/// summarized rounds in the turn's later requests only; canonical turn
/// messages, durable journals, and replayed history keep every round. A
/// failed or rejected summary falls back to removing the rounds behind a
/// deterministic note, and a turn stops asking after
/// two failures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextSummaryPolicy {
    /// Earlier tool rounds are only elided and removed deterministically.
    #[default]
    Disabled,
    /// A request that still exceeds 50% of the capacity after clearing
    /// superseded results and capping oversized ones first summarizes the
    /// current turn's earlier tool rounds, keeping the latest round and the
    /// rounds before it within a quarter of the capacity whole, when the
    /// rounds not yet summarized weigh at least a fifth of the capacity.
    Enabled,
}

/// Injectable approximate weighing of provider request content.
///
/// Estimates cover instructions, historical messages including tool groups,
/// advertised tool definitions, and the fixed cost of each request. The
/// current ordered text/image input is admitted as the canonical user message
/// it becomes, so projection and in-turn fitting share one accounting.
/// Implementations must be deterministic and use the same units as
/// [`ContextBudget`]; estimates never claim exact provider token counts.
pub trait ContextEstimator: Send + Sync {
    /// Approximates one system, user, or historical conversation message.
    fn message_weight(&self, message: &ModelMessage) -> u64;

    /// Approximates one advertised tool definition.
    fn tool_definition_weight(&self, tool: &ToolDefinition) -> u64;

    /// Approximates the fixed cost a provider adds to every request beyond
    /// its messages and tool definitions, such as chat-template framing, the
    /// tool-calling preamble, and an output `schema` rendered into the
    /// prompt.
    fn request_weight(&self, schema: &OutputSchema) -> u64;
}

/// Byte-rate estimator with per-model parameters: payload bytes at a rate of
/// tokens per 1,000 bytes plus fixed tokens per message, per tool definition,
/// and per request, with images at their base64 data-URL length.
///
/// Replayed provider reasoning is weighed like any payload: model clients keep
/// reasoning only for providers that require it replayed, and sessions never
/// switch history holding it to another model, so a request carries only the
/// reasoning its provider receives.
///
/// [`Self::DEFAULT`] approximates any model at four bytes per token with four
/// tokens per message and tool definition and no fixed request cost. A
/// [`crate::model::ModelProfile`] declares a model's measured parameters, and
/// harnesses built from its registration weigh requests with them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeuristicContextEstimator {
    message_tokens: u64,
    message_tokens_per_kilobyte: u64,
    output_schema_in_prompt: bool,
    request_tokens: u64,
    tool_tokens: u64,
    tool_tokens_per_kilobyte: u64,
}

impl HeuristicContextEstimator {
    /// Model-agnostic parameters: 250 tokens per 1,000 bytes for messages and
    /// tool definitions, four tokens per message and tool definition, and
    /// no fixed request cost or output schema.
    pub const DEFAULT: Self = Self {
        message_tokens: 4,
        message_tokens_per_kilobyte: 250,
        output_schema_in_prompt: false,
        request_tokens: 0,
        tool_tokens: 4,
        tool_tokens_per_kilobyte: 250,
    };

    /// Weighs message payload at `tokens_per_kilobyte` tokens per 1,000
    /// bytes plus `tokens_per_message` for each message's framing.
    #[must_use]
    pub const fn with_messages(
        mut self,
        tokens_per_kilobyte: u64,
        tokens_per_message: u64,
    ) -> Self {
        self.message_tokens = tokens_per_message;
        self.message_tokens_per_kilobyte = tokens_per_kilobyte;

        self
    }

    /// Weighs each tool definition's name, description, and parameter schema
    /// at `tokens_per_kilobyte` tokens per 1,000 bytes plus
    /// `tokens_per_definition`.
    #[must_use]
    pub const fn with_tool_definitions(
        mut self,
        tokens_per_kilobyte: u64,
        tokens_per_definition: u64,
    ) -> Self {
        self.tool_tokens = tokens_per_definition;
        self.tool_tokens_per_kilobyte = tokens_per_kilobyte;

        self
    }

    /// Adds `request_tokens` to every request for the provider's fixed
    /// framing, including any tool-calling preamble.
    #[must_use]
    pub const fn with_request_tokens(mut self, request_tokens: u64) -> Self {
        self.request_tokens = request_tokens;

        self
    }

    /// Weighs every request's output schema as one more message, for a
    /// provider that renders the schema into the prompt.
    #[must_use]
    pub const fn with_output_schema_in_prompt(mut self) -> Self {
        self.output_schema_in_prompt = true;

        self
    }

    fn weigh(bytes: usize, tokens_per_kilobyte: u64, item_tokens: u64) -> u64 {
        let bytes = u128::try_from(bytes).unwrap_or(u128::MAX);
        let tokens = bytes
            .saturating_mul(u128::from(tokens_per_kilobyte))
            .div_ceil(1_000);

        u64::try_from(tokens)
            .unwrap_or(u64::MAX)
            .saturating_add(item_tokens)
    }
}

impl Default for HeuristicContextEstimator {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ContextEstimator for HeuristicContextEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        Self::weigh(
            message.retained_bytes(),
            self.message_tokens_per_kilobyte,
            self.message_tokens,
        )
    }

    fn tool_definition_weight(&self, tool: &ToolDefinition) -> u64 {
        let bytes = tool
            .name()
            .len()
            .saturating_add(tool.description().len())
            .saturating_add(tool.parameters().to_string().len());

        Self::weigh(bytes, self.tool_tokens_per_kilobyte, self.tool_tokens)
    }

    fn request_weight(&self, schema: &OutputSchema) -> u64 {
        let schema_weight = if self.output_schema_in_prompt {
            Self::weigh(
                schema.value().to_string().len(),
                self.message_tokens_per_kilobyte,
                self.message_tokens,
            )
        } else {
            0
        };

        self.request_tokens.saturating_add(schema_weight)
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
    /// patches a note on their outcome. Tool calls keep their identifiers,
    /// other arguments, and assistant text, so tool groups stay well-formed
    /// and the model's notes on what it found survive.
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
/// Returns [`TurnError::ContextBudgetExceeded`] when the fixed request cost,
/// instructions, the current input, advertised tool definitions, and the
/// reserved output cannot fit.
pub(crate) fn admit_mandatory_content(
    estimator: &dyn ContextEstimator,
    budget: ContextBudget,
    system_prompt: Option<&str>,
    input: &TurnInput,
    options: &TurnOptions,
) -> Result<u64, TurnError> {
    let mut required = budget
        .reserved_output_weight()
        .saturating_add(estimator.request_weight(options.schema()));
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
/// Canonical turn messages only grow by appending, and every reduction is
/// sticky: a message is reduced at most once, when a request exceeds the
/// budget, and keeps that form in every later request. Results and call
/// messages are reduced oldest first behind watermarks, dropped rounds are
/// named by notes appended after the earlier ones, and the latest round's
/// cut results stay cut. A request that fits therefore only appends its new
/// messages to the previous request's reduced copy, so requests between two
/// reductions share one byte-identical prefix, and each reduction rewrites
/// only from its first newly reduced message.
#[derive(Debug, Default)]
pub(crate) struct ContextFit {
    /// Weight of the canonical messages synced so far, before any reduction.
    canonical_weight: u64,
    capped_results: usize,
    /// Canonical index before which every result was considered for
    /// capping by a fit that exceeded the budget.
    capped_results_before: usize,
    /// Canonical index before which every call message's reasoning and
    /// `write` patches were considered for clearing.
    cleared_calls_before: usize,
    dropped_history_turns: usize,
    dropped_rounds: DroppedRounds,
    elided_results_before: usize,
    fitted: Option<ModelRequest>,
    /// Index of the round whose `write` patches were shrunk as the last
    /// resort while it was the latest, with how many calls changed.
    latest_shrunk_writes: Option<(usize, usize)>,
    messages: Vec<FitMessage>,
    records: CallRecords,
    /// Whether any request of this turn was reduced.
    reduced: bool,
    /// Canonical index of the latest result synced for each call
    /// identifier.
    result_indices: HashMap<String, usize>,
    reduced_counts: [usize; 3],
    required: u64,
    sent_weight: u64,
    /// Canonical index through which earlier rounds were already offered to
    /// a summary request, successful or not.
    summary_attempted_through: usize,
    summary_failures: usize,
    superseded_results: usize,
    unreduced_weight: u64,
}

/// What a turn does next to send its grown request within the budget.
#[derive(Debug)]
pub(crate) enum FitStep<'fit> {
    /// Send this reduced copy, or the canonical request when `None`, with
    /// the reductions it carries.
    Send(Option<&'fit ModelRequest>, ContextActivity),
    /// Summarize earlier tool rounds first, then fit again.
    Summarize(RoundSummaryJob),
}

impl ContextFit {
    /// Returns the approximate weight of the request the last
    /// [`Self::fit`] returned, as sent and without reserved output.
    pub(crate) fn sent_weight(&self) -> u64 {
        self.sent_weight
    }

    /// Returns what the request the last [`Self::fit`] saw would weigh with
    /// no reduction, without reserved output.
    pub(crate) fn unreduced_weight(&self) -> u64 {
        self.unreduced_weight
    }

    /// Returns how many summaries of this turn failed or were rejected.
    pub(crate) fn summary_failures(&self) -> usize {
        self.summary_failures
    }

    /// Returns the copy of `request` to send in its place, or `None` when the
    /// canonical request fits and no earlier request was reduced, with the
    /// reductions the copy carries.
    ///
    /// A request within the budget is sent as the previous request's reduced
    /// copy with the new messages appended. A request that exceeds the budget
    /// first clears older tool results that a later call of the request
    /// superseded, oldest first, until it weighs at most
    /// [`FIT_TARGET_PERCENT`] of its capacity (see [`Self::elide_superseded`]).
    /// It then caps each result the latest round added above
    /// [`RESULT_CAP_PERCENT`] of its capacity, and the round's largest results
    /// while together they exceed [`LATEST_ROUND_CAP_PERCENT`] (see
    /// [`Self::excerpt_within`]), and is reduced to [`FIT_TARGET_PERCENT`] of
    /// its capacity, oldest first (replayed history before the current
    /// turn): the other tool results become notices, then assistant reasoning
    /// and `write` patches are cleared together, one message at a time; the
    /// rounds holding the newest [`KEEP_RECENT_RESULTS`] results are cleared
    /// the same way only after everything older. Whole projected history
    /// turns, then whole earlier tool rounds of the current turn, oldest
    /// first, are then dropped down to [`DROP_TARGET_PERCENT`], each fit's
    /// dropped rounds named by one note appended after the earlier ones. The
    /// latest tool-call round is never reduced by these steps. Only when the
    /// request still exceeds the budget are the latest round's results,
    /// largest first, cut further, and as the last resort its `write` patches
    /// replaced by a note on their recorded outcome. Tool calls keep their
    /// identifiers so groups stay well-formed, and every reduced message keeps
    /// its reduced form in later requests.
    ///
    /// With a `summary` trigger, a request that still exceeds
    /// [`FIT_TARGET_PERCENT`] after superseded results are cleared and
    /// oversized results capped instead returns [`FitStep::Summarize`] for
    /// the current turn's earlier rounds (see [`Self::summary_job`]); the
    /// caller installs the summary or records its failure and fits again.
    /// [`ContextSummaryTrigger::ContextOverflow`] asks for that summary even
    /// when the request fits.
    ///
    /// # Errors
    /// Returns [`TurnError::ContextBudgetExceeded`] when even the fully
    /// reduced request does not fit.
    pub(crate) fn fit(
        &mut self,
        estimator: &dyn ContextEstimator,
        budget: ContextBudget,
        request: &ModelRequest,
        summary: Option<ContextSummaryTrigger>,
    ) -> Result<FitStep<'_>, TurnError> {
        let synced = self.sync(estimator, request);
        self.dropped_rounds.open = false;
        let fixed = budget
            .reserved_output_weight()
            .saturating_add(estimator.request_weight(request.schema()));
        let base = request.tools().iter().fold(fixed, |weight, tool| {
            weight.saturating_add(estimator.tool_definition_weight(tool))
        });
        let budget_weight = budget.max_request_weight().get();
        self.unreduced_weight =
            base.saturating_add(self.canonical_weight) - budget.reserved_output_weight();
        let previous = self.fitted.take();
        let forced = summary == Some(ContextSummaryTrigger::ContextOverflow);
        if !forced
            && previous.is_none()
            && !self.reduced
            && base.saturating_add(self.required) <= budget_weight
        {
            self.sent_weight = base.saturating_add(self.required) - budget.reserved_output_weight();

            return Ok(FitStep::Send(None, ContextActivity::default()));
        }
        let layout = Layout::new(request);
        let over = base.saturating_add(self.required) > budget_weight;
        let mut changed =
            over && self.clear_stale_and_cap(estimator, budget, request, &layout, base);
        if let Some(trigger) = summary
            && (forced
                || (over
                    && base.saturating_add(self.required)
                        > Self::target(budget, FIT_TARGET_PERCENT)))
            && let Some(job) = self.summary_job(estimator, budget, request, &layout, trigger)
        {
            // The taken copy is rebuilt after the summary, carrying every
            // reduction made so far.
            self.reduced |= changed;

            return Ok(FitStep::Summarize(job));
        }
        if over && self.extend(estimator, budget, request, &layout, base) {
            changed = true;
        }
        if base.saturating_add(self.required) > budget_weight
            && self.cut_latest_round(estimator, request, layout.latest_round, base, budget_weight)
        {
            changed = true;
        }
        if base.saturating_add(self.required) > budget_weight
            && self.shrink_latest_writes(estimator, request, layout.latest_round)
        {
            changed = true;
        }
        let required = base.saturating_add(self.required);
        if required > budget_weight {
            return Err(TurnError::ContextBudgetExceeded {
                budget: budget_weight,
                required,
            });
        }
        self.reduced |= changed;
        let fitted = match previous {
            Some(mut fitted) if !changed => {
                fitted
                    .messages_mut()
                    .extend_from_slice(&request.messages()[synced..]);

                fitted
            }
            _ => self.build(request, &layout),
        };
        let shrunk_latest_write_calls = self
            .latest_shrunk_writes
            .filter(|(round, _)| *round == layout.latest_round)
            .map_or(0, |(_, calls)| calls);
        let activity = ContextActivity::new(
            self.reduced_counts[Reduction::ClearReasoning.slot()],
            self.dropped_history_turns,
            self.dropped_rounds.calls - self.dropped_rounds.summarized_calls,
            self.reduced_counts[Reduction::ElideResult.slot()],
            shrunk_latest_write_calls,
            self.reduced_counts[Reduction::ShrinkWrite.slot()]
                - usize::from(shrunk_latest_write_calls > 0),
            self.capped_results,
        )
        .with_superseded_results(self.superseded_results)
        .with_summarized_tool_calls(self.dropped_rounds.summarized_calls);
        self.sent_weight = required - budget.reserved_output_weight();

        Ok(FitStep::Send(Some(&*self.fitted.insert(fitted)), activity))
    }

    /// Weighs and caches the canonical messages `request` appended since the
    /// last fit, and returns how many were cached before.
    fn sync(&mut self, estimator: &dyn ContextEstimator, request: &ModelRequest) -> usize {
        let synced = self.messages.len();
        for (index, message) in request.messages().iter().enumerate().skip(synced) {
            let weight = estimator.message_weight(message);
            self.canonical_weight = self.canonical_weight.saturating_add(weight);
            self.required = self.required.saturating_add(weight);
            self.records.record(message);
            let repeats = repeated_call(message)
                .and_then(|call_id| self.result_indices.get(&call_id).copied());
            if let ModelMessage::ToolResult { call_id, .. } = message {
                self.result_indices.insert(call_id.clone(), index);
            }
            self.messages.push(FitMessage {
                changes: 0,
                reduced: None,
                repeats,
                span: ReadSpan::parse(message),
                weight,
            });
        }

        synced
    }

    /// Replaces the earlier rounds `job` covers with `summary` in this and
    /// later requests, behind one note that carries the summary and names
    /// every dropped round's applied writes, commands, and read targets in
    /// place of the earlier notes. Returns how many tool calls the summary
    /// stands in for, or `None`, changing nothing, when the note would not
    /// weigh less than the rounds and the notes it replaces.
    pub(crate) fn install_summary(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        job: &RoundSummaryJob,
        summary: Value,
    ) -> Option<usize> {
        let range = job.range();
        let mut note = RoundNote::merged(&self.dropped_rounds.notes);
        let calls_before = note.calls;
        for message in &request.messages()[range.clone()] {
            let calls = tool_calls(message);
            if !calls.is_empty() {
                note.record(calls, &self.records);
            }
        }
        let summarized_calls = note.calls - calls_before;
        note.summary = Some(summary.clone());
        note.weight = estimator.message_weight(&note.message());
        let notes_weight = self.dropped_rounds.weight();
        let replaced = self.weight_of(range.clone()).saturating_add(notes_weight);
        if note.weight >= replaced {
            return None;
        }
        self.leave_out(range.clone());
        self.required = self
            .required
            .saturating_sub(notes_weight)
            .saturating_add(note.weight);
        self.dropped_rounds.notes = vec![note];
        self.dropped_rounds.calls += summarized_calls;
        self.dropped_rounds.summarized_calls += summarized_calls;
        self.dropped_rounds.summary = Some(summary);
        self.dropped_rounds.end = range.end;
        self.reduced = true;
        self.clear_repeats(
            estimator,
            request.messages(),
            |pointed| range.contains(&pointed),
            None,
        );
        // Left-out messages are never reduced again, or their savings would
        // count twice.
        for watermark in [
            &mut self.capped_results_before,
            &mut self.cleared_calls_before,
            &mut self.elided_results_before,
        ] {
            *watermark = (*watermark).max(range.end);
        }

        Some(summarized_calls)
    }

    /// Records that a summary failed or was rejected; after
    /// [`MAX_SUMMARY_FAILURES_PER_TURN`] failures no further summary is
    /// requested.
    pub(crate) fn summary_failed(&mut self) {
        self.summary_failures += 1;
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
    }

    /// Returns the capacity `budget` leaves after reserved output.
    fn capacity(budget: ContextBudget) -> u64 {
        budget.max_request_weight().get() - budget.reserved_output_weight()
    }

    /// Returns the weight a reduction step stops at: `percent` of the
    /// capacity, plus reserved output.
    fn target(budget: ContextBudget, percent: u64) -> u64 {
        budget
            .reserved_output_weight()
            .saturating_add(Self::capacity(budget).saturating_mul(percent) / 100)
    }

    /// Elides superseded older results, then caps the oversized results of
    /// the latest round that no earlier over-budget fit considered, the
    /// reductions that precede any summary, and returns whether any sent
    /// message changed. An older result is never capped: it was sent whole,
    /// so capping it would rewrite the cached prefix only to rewrite it again
    /// once the result is elided.
    fn clear_stale_and_cap(
        &mut self,
        estimator: &dyn ContextEstimator,
        budget: ContextBudget,
        request: &ModelRequest,
        layout: &Layout<'_>,
        base: u64,
    ) -> bool {
        let mut changed = false;
        let mut tier = Tier {
            base,
            changed: &mut changed,
            estimator,
            messages: request.messages(),
            target: Self::target(budget, FIT_TARGET_PERCENT),
        };
        // Superseded results go first, so none is kept as a capped excerpt.
        self.elide_superseded(&mut tier, layout.latest_round);
        if self.cap_results(
            estimator,
            request,
            layout.latest_round,
            Self::capacity(budget),
        ) {
            changed = true;
        }

        changed
    }

    /// Returns a job summarizing the current turn's earlier tool rounds that
    /// no summary or dropped-round note covers yet, or `None` when this turn
    /// already failed [`MAX_SUMMARY_FAILURES_PER_TURN`] summaries, no whole
    /// round precedes the kept tail, a proactive summary would cover less than
    /// [`SUMMARY_MIN_SOURCE_PERCENT`] of the capacity not yet offered to a
    /// summary, or even the tightest source does not fit the budget.
    ///
    /// A proactive summary keeps the latest round and the rounds before it
    /// within [`SUMMARY_KEEP_TAIL_PERCENT`] of the capacity whole; a
    /// context-overflow summary keeps only the latest round. The source
    /// carries the task, any earlier summary, and each covered round with
    /// superseded results as their notices, its text cut to the first of
    /// [`SOURCE_TEXT_LEVELS`] that fits, then without its oldest rounds,
    /// which the note still names.
    fn summary_job(
        &mut self,
        estimator: &dyn ContextEstimator,
        budget: ContextBudget,
        request: &ModelRequest,
        layout: &Layout<'_>,
        trigger: ContextSummaryTrigger,
    ) -> Option<RoundSummaryJob> {
        if self.summary_failures >= MAX_SUMMARY_FAILURES_PER_TURN {
            return None;
        }
        let messages = request.messages();
        let capacity = Self::capacity(budget);
        let start = self.dropped_rounds.end.max(layout.input_index + 1);
        let mut round_starts: Vec<usize> = (start..layout.latest_round)
            .filter(|index| is_tool_call_message(&messages[*index]))
            .collect();
        let tail_start = match trigger {
            ContextSummaryTrigger::Proactive => self.tail_start(
                &round_starts,
                layout.latest_round,
                capacity.saturating_mul(SUMMARY_KEEP_TAIL_PERCENT) / 100,
            ),
            ContextSummaryTrigger::ContextOverflow => layout.latest_round,
        };
        round_starts.retain(|round_start| *round_start < tail_start);
        if round_starts.is_empty() {
            return None;
        }
        let unoffered = self.weight_of(start.max(self.summary_attempted_through)..tail_start);
        if trigger == ContextSummaryTrigger::Proactive
            && unoffered < capacity.saturating_mul(SUMMARY_MIN_SOURCE_PERCENT) / 100
        {
            return None;
        }
        // The first round also carries any request-only message before it.
        round_starts[0] = start;
        let rounds: Vec<Vec<ModelMessage>> = round_starts
            .iter()
            .zip(round_starts.iter().skip(1).chain([&tail_start]))
            .map(|(round_start, round_end)| {
                self.summary_source_messages(request, *round_start..*round_end)
            })
            .collect();
        let rounds: Vec<&[ModelMessage]> = rounds.iter().map(Vec::as_slice).collect();
        let max_summary_bytes = usize::try_from(
            (capacity.saturating_mul(SUMMARY_MAX_PERCENT) / 100)
                .saturating_mul(SUMMARY_BYTES_PER_WEIGHT),
        )
        .unwrap_or(usize::MAX)
        .min(compaction::MAX_SUMMARY_BYTES);
        let schema = compaction::summary_schema().ok()?;
        let tightest = SOURCE_TEXT_LEVELS[SOURCE_TEXT_LEVELS.len() - 1];
        let attempts = SOURCE_TEXT_LEVELS
            .iter()
            .map(|level| (0, *level))
            .chain((1..rounds.len()).map(|skipped| (skipped, tightest)));
        for (skipped, max_text_bytes) in attempts {
            let source = round_summary::render_source(
                &messages[layout.input_index],
                self.dropped_rounds.summary.as_ref(),
                &rounds[skipped..],
                max_text_bytes,
                max_summary_bytes,
            );
            let job = RoundSummaryJob::new(
                start..tail_start,
                trigger,
                source,
                max_summary_bytes,
                schema.clone(),
            );
            let required = weigh_messages(estimator, job.request().messages())
                .saturating_add(estimator.request_weight(job.request().schema()))
                .saturating_add(budget.reserved_output_weight());
            if required <= budget.max_request_weight().get() {
                self.summary_attempted_through = tail_start;

                return Some(job);
            }
        }

        None
    }

    /// Returns where the kept tail of earlier rounds starts: the latest
    /// round always stays, and each round before it, newest first, while
    /// together they weigh at most `keep`.
    fn tail_start(&self, round_starts: &[usize], latest_round: usize, keep: u64) -> usize {
        let mut tail_start = latest_round;
        let mut kept = 0_u64;
        for round_start in round_starts.iter().rev() {
            let weight = self.weight_of(*round_start..tail_start);
            if kept.saturating_add(weight) > keep {
                break;
            }
            kept = kept.saturating_add(weight);
            tail_start = *round_start;
        }

        tail_start
    }

    /// Returns the cached weight of the messages in `range`.
    fn weight_of(&self, range: Range<usize>) -> u64 {
        self.messages[range]
            .iter()
            .fold(0_u64, |weight, entry| weight.saturating_add(entry.weight))
    }

    /// Returns the canonical messages in `range` as a summary sees them: a
    /// superseded result as the notice that names its newer call, and every
    /// other message whole.
    fn summary_source_messages(
        &self,
        request: &ModelRequest,
        range: Range<usize>,
    ) -> Vec<ModelMessage> {
        range
            .map(|index| {
                let entry = &self.messages[index];
                match &entry.reduced {
                    Some(reduced) if entry.changes & SUPERSEDED_FLAG != 0 => reduced.clone(),
                    _ => request.messages()[index].clone(),
                }
            })
            .collect()
    }

    /// Reduces messages past the watermarks until the request weighs at most
    /// [`FIT_TARGET_PERCENT`] of its capacity, then drops whole history turns
    /// and earlier rounds until it weighs at most [`DROP_TARGET_PERCENT`],
    /// advances the watermarks past what it reduced, and returns whether any
    /// sent message changed.
    ///
    /// Results before the recent rounds (see [`Self::recent_start`]) become
    /// notices first, then those rounds' calls lose their reasoning and
    /// `write` patches, and only then are the recent rounds before the latest
    /// one reduced the same way.
    fn extend(
        &mut self,
        estimator: &dyn ContextEstimator,
        budget: ContextBudget,
        request: &ModelRequest,
        layout: &Layout<'_>,
        base: u64,
    ) -> bool {
        let recent_start = self.recent_start(request.messages(), layout);
        let latest_round = layout.latest_round;
        let mut changed = false;
        let mut tier = Tier {
            base,
            changed: &mut changed,
            estimator,
            messages: request.messages(),
            target: Self::target(budget, FIT_TARGET_PERCENT),
        };
        for end in [recent_start, latest_round] {
            self.elided_results_before =
                self.elide_results(&mut tier, self.elided_results_before, end);
            self.cleared_calls_before = self.clear_calls(&mut tier, self.cleared_calls_before, end);
        }
        let drop_target = Self::target(budget, DROP_TARGET_PERCENT);
        while base.saturating_add(self.required) > drop_target && self.drop_history_turn(layout) {
            changed = true;
        }
        while base.saturating_add(self.required) > drop_target
            && self.drop_round(estimator, request, layout)
        {
            changed = true;
        }
        // With every earlier round dropped, the notes are what remains.
        if base.saturating_add(self.required) > drop_target && self.dropped_rounds.notes.len() > 1 {
            self.merge_notes(estimator);
            changed = true;
        }

        changed
    }

    /// Replaces the dropped-round notes with one note naming everything they
    /// name and carrying the latest summary, which bounds what the notes of a
    /// long turn weigh.
    fn merge_notes(&mut self, estimator: &dyn ContextEstimator) {
        let mut note = RoundNote::merged(&self.dropped_rounds.notes);
        note.summary.clone_from(&self.dropped_rounds.summary);
        note.weight = estimator.message_weight(&note.message());
        self.required = self
            .required
            .saturating_sub(self.dropped_rounds.weight())
            .saturating_add(note.weight);
        self.dropped_rounds.notes = vec![note];
    }

    /// Returns where the current turn's recent rounds start: the latest
    /// round, and each round before it, newest first, until together they
    /// hold [`KEEP_RECENT_RESULTS`] tool results.
    fn recent_start(&self, messages: &[ModelMessage], layout: &Layout<'_>) -> usize {
        let floor = self.dropped_rounds.end.max(layout.input_index + 1);
        let results = |range: Range<usize>| {
            messages[range]
                .iter()
                .filter(|message| matches!(message, ModelMessage::ToolResult { .. }))
                .count()
        };
        let mut start = layout.latest_round;
        let mut kept = results(start..messages.len());
        let round_starts: Vec<usize> = (floor..layout.latest_round)
            .filter(|index| is_tool_call_message(&messages[*index]))
            .collect();
        for round_start in round_starts.into_iter().rev() {
            if kept >= KEEP_RECENT_RESULTS {
                break;
            }
            kept += results(round_start..start);
            start = round_start;
        }

        start
    }

    /// Cuts the latest round's tool results that no earlier over-budget fit
    /// considered to one common weight that keeps the round's results
    /// together within [`LATEST_ROUND_CAP_PERCENT`] of `capacity`, and each
    /// to at most [`RESULT_CAP_PERCENT`] (see [`Self::excerpt_within`]);
    /// caches each excerpt so later requests carry it unchanged, clears the
    /// repeated-read results pointing to a cut result, and returns whether
    /// any result was cut. A fit that only retries a rejected request still
    /// considers its latest round. Older results are only ever elided, so
    /// none is rewritten twice.
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
        let cap = Self::common_cap(
            latest_weights,
            capacity.saturating_mul(LATEST_ROUND_CAP_PERCENT) / 100,
        )
        .min(result_cap);
        let start = self.capped_results_before.max(latest_round);
        self.capped_results_before = messages.len();
        let mut cut = Vec::new();
        for (index, message) in messages.iter().enumerate().skip(start) {
            let ModelMessage::ToolResult {
                call_id,
                content,
                name,
            } = message
            else {
                continue;
            };
            let entry = &self.messages[index];
            let current = entry.weight;
            if current <= cap || entry.changes & Reduction::ElideResult.flag() != 0 {
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
                cut.push(index);
            }
        }
        self.clear_repeats(estimator, messages, |pointed| cut.contains(&pointed), None);

        !cut.is_empty()
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

    /// Returns the tool result `(call_id, content, name)` cut to weigh at
    /// most `allowed`: a `read` result to its leading whole lines or items,
    /// marked as cut, as when the read ran, and any other result, or a read
    /// result no whole line of which fits, to the longest head and tail
    /// excerpt that fits, or to the bare notice when none does.
    fn excerpt_within(
        estimator: &dyn ContextEstimator,
        (call_id, content, name): (&str, &str, &str),
        allowed: u64,
    ) -> ModelMessage {
        let result = |content: String| ModelMessage::ToolResult {
            call_id: call_id.to_string(),
            content,
            name: name.to_string(),
        };
        if name == Tool::Read.name()
            && let Some(page) = read::fit_tool_result(content, &|candidate| {
                estimator.message_weight(&result(candidate.to_string())) <= allowed
            })
        {
            return result(page);
        }
        let excerpt = |kept_bytes| result(truncated_result(content, kept_bytes));
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
            if self.elide(tier, index, Some(&supersession)) {
                self.messages[index].changes |= SUPERSEDED_FLAG;
                self.superseded_results += 1;
            }
        }
    }

    /// Elides the tool results from `start` to `end`, oldest first, that are
    /// not yet elided, until the request weighs at most the tier's target,
    /// and returns the index it stopped at, never before `start`.
    fn elide_results(&mut self, tier: &mut Tier<'_>, start: usize, end: usize) -> usize {
        for index in start..end {
            if tier.base.saturating_add(self.required) <= tier.target {
                return index;
            }
            if self.messages[index].changes & Reduction::ElideResult.flag() == 0 {
                self.elide(tier, index, None);
            }
        }

        start.max(end)
    }

    /// Replaces the tool result at `index` with the notice that stands in
    /// for it (see [`elided_result`]), and every repeated-read result, whole
    /// or cut, pointing to it with a notice that it is no longer shown;
    /// returns whether the result was elided.
    fn elide(
        &mut self,
        tier: &mut Tier<'_>,
        index: usize,
        supersession: Option<&Supersession<'_>>,
    ) -> bool {
        // A notice describes the original result, not its capped excerpt.
        let Some(reduced) = elided_result(&tier.messages[index], &self.records, supersession)
        else {
            return false;
        };
        if !self.replace(tier, index, reduced, &[Reduction::ElideResult]) {
            return false;
        }
        self.clear_repeats(
            tier.estimator,
            tier.messages,
            |pointed| pointed == index,
            supersession
                .filter(|supersession| supersession.cause == SupersessionCause::Reread)
                .map(|supersession| supersession.by),
        );

        true
    }

    /// Replaces every repeated-read result still in the request, and not yet
    /// cleared, that points to an earlier result `pointed` selects with a
    /// notice that the result is no longer shown whole, naming
    /// `superseded_by` when a later read returned it again.
    fn clear_repeats(
        &mut self,
        estimator: &dyn ContextEstimator,
        messages: &[ModelMessage],
        pointed: impl Fn(usize) -> bool,
        superseded_by: Option<&str>,
    ) {
        let repeats: Vec<(usize, &str)> = (self.dropped_rounds.end..messages.len())
            .filter_map(|repeat| match (&self.messages[repeat], &messages[repeat]) {
                (entry, ModelMessage::ToolResult { call_id, .. })
                    if entry.changes & Reduction::ElideResult.flag() == 0
                        && entry.repeats.is_some_and(&pointed) =>
                {
                    Some((repeat, call_id.as_str()))
                }
                _ => None,
            })
            .collect();
        for (repeat, call_id) in repeats {
            let notice = cleared_repeat_result(call_id, &self.records, superseded_by);
            let weight = estimator.message_weight(&notice);
            let entry = &mut self.messages[repeat];
            self.required = self
                .required
                .saturating_sub(entry.weight)
                .saturating_add(weight);
            self.reduced_counts[Reduction::ElideResult.slot()] += 1;
            if entry.changes & CAPPED_FLAG != 0 {
                entry.changes &= !CAPPED_FLAG;
                self.capped_results -= 1;
            }
            entry.changes |= Reduction::ElideResult.flag();
            entry.reduced = Some(notice);
            entry.weight = weight;
        }
    }

    /// Clears the reasoning and `write` patches of the call messages, and
    /// the reasoning of other assistant messages, from `start` to `end`,
    /// oldest first, each message once with every clearing that applies,
    /// until the request weighs at most the tier's target, and returns the
    /// index it stopped at, never before `start`.
    fn clear_calls(&mut self, tier: &mut Tier<'_>, start: usize, end: usize) -> usize {
        for index in start..end {
            if tier.base.saturating_add(self.required) <= tier.target {
                return index;
            }
            // Only the last resort shrinks a call message earlier, while its
            // round is the latest, and it leaves the reasoning.
            let entry = &self.messages[index];
            let message = entry.reduced.as_ref().unwrap_or(&tier.messages[index]);
            let pending = |reduction: Reduction| entry.changes & reduction.flag() == 0;
            let reasoning = pending(Reduction::ClearReasoning)
                .then(|| Reduction::ClearReasoning.apply(message, &self.records))
                .flatten();
            let writes = pending(Reduction::ShrinkWrite)
                .then(|| {
                    Reduction::ShrinkWrite
                        .apply(reasoning.as_ref().unwrap_or(message), &self.records)
                })
                .flatten();
            let (reduced, reductions): (ModelMessage, &[Reduction]) = match (reasoning, writes) {
                (Some(_), Some(both)) => {
                    (both, &[Reduction::ClearReasoning, Reduction::ShrinkWrite])
                }
                (Some(reasoning), None) => (reasoning, &[Reduction::ClearReasoning]),
                (None, Some(writes)) => (writes, &[Reduction::ShrinkWrite]),
                (None, None) => continue,
            };
            self.replace(tier, index, reduced, reductions);
        }

        start.max(end)
    }

    /// Caches `reduced` in place of the message at `index`, recorded as
    /// `reductions`, when it weighs less, and returns whether it did.
    fn replace(
        &mut self,
        tier: &mut Tier<'_>,
        index: usize,
        reduced: ModelMessage,
        reductions: &[Reduction],
    ) -> bool {
        let weight = tier.estimator.message_weight(&reduced);
        let entry = &mut self.messages[index];
        if weight >= entry.weight {
            return false;
        }
        self.required -= entry.weight - weight;
        if entry.changes & CAPPED_FLAG != 0 {
            entry.changes &= !CAPPED_FLAG;
            self.capped_results -= 1;
        }
        for reduction in reductions {
            self.reduced_counts[reduction.slot()] += 1;
            entry.changes |= reduction.flag();
        }
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
    /// before it, out of the request and names it in this fit's note, which
    /// follows the notes of earlier fits unchanged; returns `false` when no
    /// round precedes the latest one, including when the current turn has no
    /// tool round.
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
        let calls = tool_calls(&messages[call_index]);
        self.leave_out(start..end);
        let mut note = if self.dropped_rounds.open {
            self.dropped_rounds.notes.pop().unwrap_or_default()
        } else {
            RoundNote::default()
        };
        self.required = self.required.saturating_sub(note.weight);
        note.record(calls, &self.records);
        note.weight = estimator.message_weight(&note.message());
        self.required = self.required.saturating_add(note.weight);
        self.dropped_rounds.notes.push(note);
        self.dropped_rounds.open = true;
        self.dropped_rounds.calls += calls.len();
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
    /// dropped history turns and dropped rounds, whose notes follow the
    /// current input in the order they were added.
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
            if index == rounds_start {
                messages.extend(self.dropped_rounds.notes.iter().map(RoundNote::message));
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

    /// Cuts the latest round's tool results, largest first, to the longest
    /// excerpt (see [`Self::excerpt_within`]) that brings the request within
    /// `budget_weight`, caches each cut so later requests carry it
    /// unchanged, clears the repeated-read results pointing to a cut result,
    /// and returns whether any result was cut.
    fn cut_latest_round(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        latest_round: usize,
        base: u64,
        budget_weight: u64,
    ) -> bool {
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
        let mut cut = Vec::new();
        // Passes repeat while they cut, since a cleared repeated-read result
        // can outweigh the note it replaced.
        loop {
            let cut_before = cut.len();
            for &(index, call_id, content, name) in &results {
                let required = base.saturating_add(self.required);
                if required <= budget_weight {
                    break;
                }
                let current = self.messages[index].weight;
                let allowed = current.saturating_sub(required - budget_weight);
                let excerpt = Self::excerpt_within(estimator, (call_id, content, name), allowed);
                let weight = estimator.message_weight(&excerpt);
                // A repeated-read result cleared with the result it points to
                // keeps its notice.
                let cleared = self.messages[index].changes & Reduction::ElideResult.flag() != 0;
                if weight < current && !cleared {
                    self.required -= current - weight;
                    let entry = &mut self.messages[index];
                    if entry.changes & CAPPED_FLAG == 0 {
                        entry.changes |= CAPPED_FLAG;
                        self.capped_results += 1;
                    }
                    entry.reduced = Some(excerpt);
                    entry.weight = weight;
                    cut.push(index);
                    self.clear_repeats(
                        estimator,
                        request.messages(),
                        |pointed| pointed == index,
                        None,
                    );
                }
            }
            if cut.len() == cut_before || base.saturating_add(self.required) <= budget_weight {
                break;
            }
        }

        !cut.is_empty()
    }

    /// Replaces the patches of the latest round's `write` calls with a note
    /// on their recorded outcome, keeping their reasoning, caches the result
    /// so later requests carry it unchanged, and returns whether it changed
    /// the round.
    fn shrink_latest_writes(
        &mut self,
        estimator: &dyn ContextEstimator,
        request: &ModelRequest,
        latest_round: usize,
    ) -> bool {
        let Some(message) = request.messages().get(latest_round) else {
            return false;
        };
        let Some(reduced) = Reduction::ShrinkWrite.apply(message, &self.records) else {
            return false;
        };
        let weight = estimator.message_weight(&reduced);
        // A patch already shrunk weighs the same again.
        let current = self.messages[latest_round].weight;
        if weight >= current {
            return false;
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
        self.required -= current - weight;
        self.reduced_counts[Reduction::ShrinkWrite.slot()] += 1;
        let entry = &mut self.messages[latest_round];
        entry.changes |= Reduction::ShrinkWrite.flag();
        entry.reduced = Some(reduced);
        entry.weight = weight;
        self.latest_shrunk_writes = Some((latest_round, changed_calls));

        true
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

/// Returns the oldest of `oldest_first` turns, keyed by position, that fit
/// `max_bytes` in order, each in the most complete form that fits what is
/// left, stopping at the first turn that fits in no form. The first turn is
/// always kept, in brief form when no form fits, so compaction can advance
/// past it.
pub(crate) fn fit_oldest_history_bytes(
    oldest_first: impl IntoIterator<Item = (i64, HistoryTurn)>,
    max_bytes: usize,
) -> Vec<(i64, HistoryTurn)> {
    let mut remaining = max_bytes;
    let mut kept = Vec::new();
    for (position, turn) in oldest_first {
        let brief = kept.is_empty().then(|| turn.brief());
        let Some(turn) = turn.fit_bytes(remaining, max_bytes).or(brief) else {
            break;
        };
        remaining = remaining.saturating_sub(retained_bytes(&turn.messages));
        kept.push((position, turn));
    }

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
    /// Canonical index of the earlier result a repeated-read result points
    /// to.
    repeats: Option<usize>,
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

/// Earlier tool rounds of the current turn left out of the request, the
/// notes that stand in for them, and any summary of what they established.
#[derive(Clone, Debug, Default)]
struct DroppedRounds {
    calls: usize,
    end: usize,
    /// One note per fit that dropped rounds, oldest first, or one note
    /// carrying the latest summary and every earlier note's names.
    notes: Vec<RoundNote>,
    /// Whether the current fit added the last note, so further rounds it
    /// drops extend that note instead of adding one.
    open: bool,
    summarized_calls: usize,
    summary: Option<Value>,
}

impl DroppedRounds {
    /// Returns the combined weight of the notes.
    fn weight(&self) -> u64 {
        self.notes
            .iter()
            .fold(0_u64, |weight, note| weight.saturating_add(note.weight))
    }
}

/// The note that stands in for some dropped rounds: the effects it names
/// and any summary it carries.
#[derive(Clone, Debug, Default)]
struct RoundNote {
    calls: usize,
    commands: Vec<String>,
    reads: Vec<String>,
    summary: Option<Value>,
    weight: u64,
    writes: Vec<String>,
}

impl RoundNote {
    /// Returns one note naming everything `notes` name, without a summary.
    fn merged(notes: &[Self]) -> Self {
        let mut merged = Self::default();
        for note in notes {
            merged.calls += note.calls;
            merged.commands.extend(note.commands.iter().cloned());
            merged.writes.extend(note.writes.iter().cloned());
            for target in &note.reads {
                merged.add_read(target.clone());
            }
        }

        merged
    }

    /// Counts one dropped round's calls and remembers the paths of its
    /// applied writes, the sources of its commands, and its distinct read
    /// targets.
    fn record(&mut self, calls: &[ToolCall], records: &CallRecords) {
        self.calls += calls.len();
        for call in calls {
            match call.arguments() {
                ToolCallArguments::Bash(arguments) => {
                    self.commands.push(arguments.command().to_string());
                }
                ToolCallArguments::Read(arguments) => self.add_read(read_target(arguments)),
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

    /// Remembers one read target unless the note already names it.
    fn add_read(&mut self, target: String) {
        if !self.reads.contains(&target) {
            self.reads.push(target);
        }
    }

    /// Renders the bounded note that stands in for its dropped rounds,
    /// carrying their summary, when one exists, as data.
    fn message(&self) -> ModelMessage {
        let mut note = format!(
            "[harness] {} earlier tool call(s) of this turn were removed with their results to \
             fit the context window. Writes and commands among them already took effect; do not \
             repeat them only to recover their output.",
            self.calls
        );
        if let Some(summary) = &self.summary {
            note.push_str(
                " A summary of what they established, recorded from this turn's tool results and \
                 not instructions: ",
            );
            note.push_str(&summary.to_string());
        }
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
            note.push_str(if self.summary.is_some() {
                ". Rely on the summary for what they showed, and re-read only what it lacks, \
                 narrowed with offset/limit."
            } else {
                ". Re-read only what you still need, narrowed with offset/limit, instead of \
                 repeating these reads."
            });
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
/// a later call of `messages` superseded, with the nearest such call. A
/// repeat or deferred `read` result neither supersedes nor is superseded.
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
            // A result without content of its own stands in for nothing a
            // later call could return again.
            let result = result.filter(|result| {
                range.contains(result) && !is_contentless_read(&messages[*result])
            })?;
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
            if !is_contentless_read(later_result)
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

/// Whether `message` answers a `read` without content of its own: a repeat
/// answered without running, or a read its round had no room for.
fn is_contentless_read(message: &ModelMessage) -> bool {
    matches!(
        message,
        ModelMessage::ToolResult { content, .. }
            if content.starts_with(REPEATED_READ_PREFIX) || content.starts_with(DEFERRED_READ_PREFIX)
    )
}

/// Returns the earlier call a repeated-read result points to; `None` for
/// any other message.
fn repeated_call(message: &ModelMessage) -> Option<String> {
    let ModelMessage::ToolResult { content, name, .. } = message else {
        return None;
    };
    if name != Tool::Read.name() || !content.starts_with(REPEATED_READ_PREFIX) {
        return None;
    }
    let result = serde_json::from_str::<Value>(content).ok()?;

    Some(result.get("same_as_call")?.as_str()?.to_string())
}

/// Returns the notice that replaces the result of the repeated read
/// `call_id` once the earlier result it points to is no longer shown whole,
/// naming the read's target and any later call that returned it again.
fn cleared_repeat_result(
    call_id: &str,
    records: &CallRecords,
    superseded_by: Option<&str>,
) -> ModelMessage {
    let mut notice = Map::new();
    let text = superseded_by.map_or(CLEARED_REPEAT_NOTICE, |_| SUPERSEDED_REPEAT_NOTICE);
    notice.insert("context_cleared".to_string(), text.into());
    notice.extend(
        records
            .read_targets
            .get(call_id)
            .map(|target| ("read".to_string(), Value::from(target.as_str()))),
    );
    notice.extend(superseded_by.map(|by| ("superseded_by".to_string(), Value::from(by))));

    ModelMessage::ToolResult {
        call_id: call_id.to_string(),
        content: Value::Object(notice).to_string(),
        name: Tool::Read.name().to_string(),
    }
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
/// [`CLEARED_REASONING`], keeping its identifier, arguments, and assistant
/// text.
fn cleared_reasoning_call(call: &ToolCall) -> Option<ToolCall> {
    call.reasoning_content()
        .filter(|reasoning| reasoning.len() > CLEARED_REASONING.len())?;

    ToolCall::from_json(
        call.id().to_string(),
        call.name(),
        &call.arguments_json().ok()?,
        Some(CLEARED_REASONING.to_string()),
    )
    .and_then(|cleared| cleared.with_assistant_text(call.assistant_text().map(str::to_string)))
    .ok()
}

/// Rebuilds a `write` call with its patch replaced by a note on the outcome
/// its result recorded, keeping its identifier, path, reasoning, and
/// assistant text.
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
    .and_then(|shrunk| shrunk.with_assistant_text(call.assistant_text().map(str::to_string)))
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

/// Renders the result that stands in for a `read` with `arguments` whose
/// result its round had no room for: the round's earlier results already
/// fill [`LATEST_ROUND_CAP_PERCENT`] of the capacity. It repeats the call's
/// exact arguments, so the model can fetch the result in a later response.
pub(crate) fn deferred_read_result(arguments: &ReadArguments) -> String {
    let note = "Not shown: this response's earlier results fill one round's share of the context \
                window. Call read with these arguments in a later response.";
    let call = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string());

    // Fields are written in this order so the result starts with
    // `DEFERRED_READ_PREFIX`.
    format!(
        "{DEFERRED_READ_PREFIX}{},\"call\":{call}}}",
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
