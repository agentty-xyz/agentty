//! Model-aware projection of bounded session history into provider requests.

use std::collections::VecDeque;
use std::num::NonZeroU64;

use thiserror::Error;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelRequest};
use crate::stopped_turn::StoppedTurn;
use crate::tool::{Tool, ToolDefinition};
use crate::turn::{TurnError, TurnOptions};

/// Approximate request-weight budget for one registered model configuration.
///
/// Weights are deterministic approximations produced by a [`ContextEstimator`];
/// they never claim exact provider token counts. Declare the budget in
/// [`crate::model::ModelCapabilities`] to enable projection for that
/// registration and revise the registration's
/// [`crate::recovery::ExecutionIdentity`] when it changes. The budget governs
/// every provider request of a turn: the initial request keeps the most recent
/// whole turns that fit, reducing a stopped turn that does not fit to its input
/// and stop note, requests grown by in-turn tool traffic are re-admitted
/// before each follow-up model call, and budgeted registrations replay
/// projected history instead of reusing native continuation. The stored
/// byte-based replay budget still bounds how much history is loaded.
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

/// Flattens the most recent turns that fit `available_weight` as selected by
/// [`fit_recent_turns`]. Returns the projected messages with the number of
/// loaded turns that projection evicted entirely.
pub(crate) fn select_recent_turns(
    estimator: &dyn ContextEstimator,
    turns: &VecDeque<Vec<ModelMessage>>,
    available_weight: u64,
) -> (Vec<ModelMessage>, usize) {
    let kept = fit_recent_turns(turns.iter().map(Vec::as_slice), available_weight, |turn| {
        turn.iter().fold(0_u64, |weight, message| {
            weight.saturating_add(estimator.message_weight(message))
        })
    });
    let dropped_turns = turns.len().saturating_sub(kept.len());

    (kept.into_iter().flatten().collect(), dropped_turns)
}

/// Returns the bytes a replayed turn counts against the stored history
/// budget, including a stopped turn's note.
pub(crate) fn turn_bytes(turn: &[ModelMessage]) -> usize {
    turn.iter().fold(0_usize, |bytes, message| {
        bytes.saturating_add(message.retained_bytes())
    })
}

/// Keeps the most recent turns within the stored history byte budget as
/// selected by [`fit_recent_turns`], counting [`turn_bytes`].
pub(crate) fn fit_history_bytes<T>(
    turns: impl DoubleEndedIterator<Item = T>,
    max_history_bytes: usize,
) -> Vec<Vec<ModelMessage>>
where
    T: AsRef<[ModelMessage]> + Into<Vec<ModelMessage>>,
{
    let weight = |bytes: usize| u64::try_from(bytes).unwrap_or(u64::MAX);

    fit_recent_turns(turns, weight(max_history_bytes), |turn| {
        weight(turn_bytes(turn))
    })
}

/// Keeps the most recent turns whose combined `turn_weight` fits
/// `available_weight`, oldest first, so a turn's tool groups are never split.
///
/// A stopped turn that does not fit whole keeps only its input and stop note
/// when those fit, and selection continues with older turns. Any other turn
/// that does not fit drops itself and every older turn.
pub(crate) fn fit_recent_turns<T>(
    turns: impl DoubleEndedIterator<Item = T>,
    available_weight: u64,
    turn_weight: impl Fn(&[ModelMessage]) -> u64,
) -> Vec<Vec<ModelMessage>>
where
    T: AsRef<[ModelMessage]> + Into<Vec<ModelMessage>>,
{
    let mut kept = Vec::new();
    let mut remaining = available_weight;
    for turn in turns.rev() {
        let weight = turn_weight(turn.as_ref());
        if weight <= remaining {
            remaining -= weight;
            kept.push(turn.into());
            continue;
        }
        let Some(reduced) = StoppedTurn::input_and_note(turn.as_ref()) else {
            break;
        };
        let weight = turn_weight(&reduced);
        if weight > remaining {
            break;
        }
        remaining -= weight;
        kept.push(reduced);
    }
    kept.reverse();

    kept
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
