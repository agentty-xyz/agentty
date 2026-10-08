//! Model-aware projection of bounded session history into provider requests.

use std::collections::VecDeque;
use std::num::NonZeroU64;

use thiserror::Error;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::{Tool, ToolCall, ToolDefinition};
use crate::turn::{TurnError, TurnOptions};

/// Approximate request-weight budget for one registered model configuration.
///
/// Weights are deterministic approximations produced by a [`ContextEstimator`];
/// they never claim exact provider token counts. Every harness requires one:
/// pass it to `Harness::new` or declare it in
/// [`crate::model::ModelCapabilities`], and revise the effective
/// [`crate::recovery::ExecutionIdentity`] when it changes. The budget governs
/// every provider request of a turn: the initial request keeps the most recent
/// whole turns that fit, and requests grown by in-turn tool traffic are
/// re-admitted before each follow-up model call. The stored byte-based replay
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

    /// Returns a stopped turn's input and stop note, replayed when its full
    /// replay does not fit; `None` for a completed turn.
    pub(crate) fn brief_replay(&self) -> Option<Vec<ModelMessage>> {
        let stop = self.stop.as_ref()?;

        Some(
            self.messages
                .first()
                .cloned()
                .into_iter()
                .chain([stop.note()])
                .collect(),
        )
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
/// sends, so this admission matches [`admit_grown_request`]'s per-message
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

/// Re-admits a request that in-turn tool traffic has grown since projection.
///
/// # Errors
/// Returns [`TurnError::ContextBudgetExceeded`] when the accumulated
/// messages, advertised tool definitions, and reserved output no longer fit.
pub(crate) fn admit_grown_request(
    estimator: &dyn ContextEstimator,
    budget: ContextBudget,
    request: &ModelRequest,
) -> Result<(), TurnError> {
    let mut required = budget.reserved_output_weight();
    for message in request.messages() {
        required = required.saturating_add(estimator.message_weight(message));
    }
    for tool in request.tools() {
        required = required.saturating_add(estimator.tool_definition_weight(tool));
    }
    let budget_weight = budget.max_request_weight().get();
    if required > budget_weight {
        return Err(TurnError::ContextBudgetExceeded {
            budget: budget_weight,
            required,
        });
    }

    Ok(())
}

/// Flattens the most recent finished turns whose combined weight fits
/// `available_weight`, dropping every turn older than the first that does
/// not, so a turn's tool groups are never split. A stopped turn whose full
/// replay does not fit keeps only its input and stop note. Returns the
/// projected messages with the number of loaded turns that projection
/// evicted.
pub(crate) fn select_recent_turns(
    estimator: &dyn ContextEstimator,
    turns: &VecDeque<HistoryTurn>,
    available_weight: u64,
) -> (Vec<ModelMessage>, usize) {
    let weigh = |messages: &[ModelMessage]| {
        messages.iter().fold(0_u64, |weight, message| {
            weight.saturating_add(estimator.message_weight(message))
        })
    };
    let mut kept = Vec::new();
    let mut remaining = available_weight;
    for turn in turns.iter().rev() {
        let full = turn.replay();
        let replay = if weigh(&full) <= remaining {
            Some(full)
        } else {
            turn.brief_replay()
                .filter(|brief| weigh(brief) <= remaining)
        };
        let Some(replay) = replay else {
            break;
        };
        remaining -= weigh(&replay);
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

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
