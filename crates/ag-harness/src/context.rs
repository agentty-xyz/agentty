//! Model-aware projection of bounded session history into provider requests.

use std::collections::VecDeque;
use std::num::NonZeroU64;

use thiserror::Error;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::{READ_NAME, Tool, ToolCall, ToolDefinition};
use crate::turn::{TurnError, TurnOptions};

/// Fraction of the request budget, as its divisor, that a grown request is
/// elided below once it no longer fits, so the requests that follow can append
/// tool rounds without eliding again.
pub(crate) const ELISION_HEADROOM_DIVISOR: u64 = 4;

/// Outcome fields of a `bash` or `write` result kept in its elision notice.
const ELIDED_OUTCOME_KEYS: [&str; 4] = ["exit_code", "signal", "status", "termination"];

/// Approximate request-weight budget for one registered model configuration.
///
/// Weights are deterministic approximations produced by a [`ContextEstimator`];
/// they never claim exact provider token counts. Every harness requires one:
/// pass it to `Harness::new` or declare it in
/// [`crate::model::ModelCapabilities`], and revise the effective
/// [`crate::recovery::ExecutionIdentity`] when it changes. The budget governs
/// every provider request of a turn: the initial request keeps the most recent
/// whole turns that fit, and a request grown by in-turn tool traffic sends its
/// oldest in-turn tool results as elision notices when it no longer fits. The
/// stored byte-based replay
/// budget still bounds how much history is loaded.
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
/// in-turn re-admission share one accounting. Implementations must be
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

    /// Returns the most complete replay weighing at most `available_weight`:
    /// the full replay, then the replay with its oldest tool results elided,
    /// then the brief replay. Returns `None` when even the brief replay does
    /// not fit.
    pub(crate) fn fitted_replay(
        &self,
        estimator: &dyn ContextEstimator,
        available_weight: u64,
    ) -> Option<Vec<ModelMessage>> {
        let mut replay = self.replay();
        let required = weigh_messages(estimator, &replay);
        if required <= available_weight {
            return Some(replay);
        }
        let (required, _) = elide_tool_results(estimator, &mut replay, required, available_weight);
        if required <= available_weight {
            return Some(replay);
        }
        let brief = self.brief_replay();

        (weigh_messages(estimator, &brief) <= available_weight).then_some(brief)
    }

    /// Returns the turn's input followed by its final output, or by its stop
    /// note for a stopped turn, dropping every tool round in between.
    pub(crate) fn brief_replay(&self) -> Vec<ModelMessage> {
        self.brief().replay()
    }

    /// Returns this turn reduced to the messages its brief replay needs: the
    /// input and, for a completed turn, the final output.
    pub(crate) fn brief(&self) -> Self {
        let output = match self.stop {
            Some(_) => None,
            None => self.messages.get(1..).and_then(<[_]>::last).cloned(),
        };

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
/// sends, so this admission matches [`fit_grown_request`]'s per-message
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

/// Fits a request that in-turn tool traffic has grown since projection.
///
/// When the whole request no longer fits, tool results are replaced oldest
/// first with a short elision notice, starting with replayed history, so a
/// turn that follows a large one can still grow. Results answering the latest
/// tool-call message always stay intact. Elision applies to the returned copy
/// only, so the turn still persists and replays every result. Returns `None`
/// when the request fits unchanged.
///
/// `elided_through` carries the message index before which an earlier request
/// of the same turn elided every result. That prefix is elided again, and
/// only a request that still does not fit and may `extend_elision` elides
/// further, down to [`ELISION_HEADROOM_DIVISOR`] headroom, advancing
/// `elided_through`, so consecutive requests share an unchanged prefix
/// instead of each eliding one more result.
///
/// # Errors
/// Returns [`TurnError::ContextBudgetExceeded`] when the messages, advertised
/// tool definitions, and reserved output cannot fit even after eliding every
/// eligible tool result.
pub(crate) fn fit_grown_request(
    estimator: &dyn ContextEstimator,
    budget: ContextBudget,
    request: &ModelRequest,
    elided_through: &mut usize,
    extend_elision: bool,
) -> Result<Option<ModelRequest>, TurnError> {
    let mut required = budget
        .reserved_output_weight()
        .saturating_add(weigh_messages(estimator, request.messages()));
    for tool in request.tools() {
        required = required.saturating_add(estimator.tool_definition_weight(tool));
    }
    let budget_weight = budget.max_request_weight().get();
    if required <= budget_weight {
        return Ok(None);
    }
    // Without a tool round, nothing precedes the latest round to elide.
    let latest_round = request
        .messages()
        .iter()
        .rposition(|message| {
            matches!(
                message,
                ModelMessage::AssistantToolCall(_) | ModelMessage::AssistantToolCalls(_)
            )
        })
        .unwrap_or(0);
    let mut fitted = request.clone();
    let (elided, unelided) =
        fitted.messages_mut()[..latest_round].split_at_mut((*elided_through).min(latest_round));
    (required, _) = elide_tool_results(estimator, elided, required, 0);
    if extend_elision && required > budget_weight {
        let target_weight = budget_weight - budget_weight / ELISION_HEADROOM_DIVISOR;
        let (remaining, examined) =
            elide_tool_results(estimator, unelided, required, target_weight);
        required = remaining;
        *elided_through = elided.len() + examined;
    }
    if required > budget_weight {
        return Err(TurnError::ContextBudgetExceeded {
            budget: budget_weight,
            required,
        });
    }

    Ok(Some(fitted))
}

/// Flattens the most recent finished turns whose combined weight fits
/// `available_weight`, dropping every turn older than the first that does
/// not, so a turn's tool groups are never split. A turn whose full replay
/// does not fit is replayed with its oldest tool results elided, or only with
/// its input and final output or stop note. Returns the projected messages
/// with the number of loaded turns that projection evicted.
pub(crate) fn select_recent_turns(
    estimator: &dyn ContextEstimator,
    turns: &VecDeque<HistoryTurn>,
    available_weight: u64,
) -> (Vec<ModelMessage>, usize) {
    let mut kept = Vec::new();
    let mut remaining = available_weight;
    for turn in turns.iter().rev() {
        let Some(replay) = turn.fitted_replay(estimator, remaining) else {
            break;
        };
        remaining -= weigh_messages(estimator, &replay);
        kept.push(replay);
    }
    let dropped_turns = turns.len().saturating_sub(kept.len());
    let messages = kept.into_iter().rev().flatten().collect();

    (messages, dropped_turns)
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

fn weigh_messages(estimator: &dyn ContextEstimator, messages: &[ModelMessage]) -> u64 {
    messages.iter().fold(0_u64, |weight, message| {
        weight.saturating_add(estimator.message_weight(message))
    })
}

/// Replaces tool results in `messages` oldest first with an elision notice
/// until `required` fits `available_weight`, skipping results the notice would
/// not shrink. Returns the remaining required weight and the number of leading
/// messages examined.
fn elide_tool_results(
    estimator: &dyn ContextEstimator,
    messages: &mut [ModelMessage],
    mut required: u64,
    available_weight: u64,
) -> (u64, usize) {
    let mut examined = 0;
    for message in messages {
        if required <= available_weight {
            break;
        }
        examined += 1;
        let ModelMessage::ToolResult {
            call_id,
            content,
            name,
        } = &*message
        else {
            continue;
        };
        let elided = ModelMessage::ToolResult {
            call_id: call_id.clone(),
            content: elided_tool_result(name, content),
            name: name.clone(),
        };
        let saved_weight = estimator
            .message_weight(message)
            .saturating_sub(estimator.message_weight(&elided));
        if saved_weight == 0 {
            continue;
        }
        required = required.saturating_sub(saved_weight);
        *message = elided;
    }

    (required, examined)
}

/// Renders the notice that stands in for one elided tool result. A `read`
/// may be repeated to recover its content; any other call already ran, so
/// its notice keeps the recorded outcome, which alone says whether the call
/// took effect, and warns against repeating it.
fn elided_tool_result(name: &str, content: &str) -> String {
    if name == READ_NAME {
        return serde_json::json!({
            "elided": "This earlier tool result was removed to keep the request within the \
                       context budget. Repeat the call only if you still need its content.",
            "original_bytes": content.len(),
        })
        .to_string();
    }
    let mut notice = serde_json::Map::new();
    notice.insert(
        "elided".to_string(),
        "This earlier tool result was removed to keep the request within the context budget. The \
         call already ran with the outcome recorded here; do not repeat it only to recover its \
         output."
            .into(),
    );
    notice.insert("original_bytes".to_string(), content.len().into());
    let result: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(content).unwrap_or_default();
    for key in ELIDED_OUTCOME_KEYS {
        if let Some(value) = result.get(key)
            && !value.is_null()
        {
            notice.insert(key.to_string(), value.clone());
        }
    }

    serde_json::Value::Object(notice).to_string()
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
