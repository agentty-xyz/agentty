//! Model-aware projection of bounded session history into provider requests.

use std::collections::VecDeque;
use std::num::NonZeroU64;

use thiserror::Error;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::{Tool, ToolDefinition};
use crate::turn::{TurnError, TurnOptions};

/// Approximate request-weight budget for one registered model configuration.
///
/// Weights are deterministic approximations produced by a [`ContextEstimator`];
/// they never claim exact provider token counts. Declare the budget in
/// [`crate::ModelCapabilities`] to enable projection for that registration and
/// revise the registration's [`crate::ExecutionIdentity`] when it changes. The
/// budget governs every provider request of a turn: the initial request keeps
/// the most recent whole turns that fit, requests grown by in-turn tool
/// traffic are re-admitted before each follow-up model call, and budgeted
/// registrations replay projected history instead of reusing native
/// continuation. The stored byte-based replay budget still bounds how much
/// history is loaded.
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

/// Flattens the most recent complete turns whose combined weight fits
/// `available_weight`, dropping every turn older than the first that does
/// not, so a turn's tool groups are never split.
pub(crate) fn select_recent_turns(
    estimator: &dyn ContextEstimator,
    turns: &VecDeque<Vec<ModelMessage>>,
    available_weight: u64,
) -> Vec<ModelMessage> {
    let mut kept = 0_usize;
    let mut remaining = available_weight;
    for turn in turns.iter().rev() {
        let weight = turn.iter().fold(0_u64, |weight, message| {
            weight.saturating_add(estimator.message_weight(message))
        });
        if weight > remaining {
            break;
        }
        remaining -= weight;
        kept += 1;
    }
    let dropped_turns = turns.len().saturating_sub(kept);

    turns
        .iter()
        .skip(dropped_turns)
        .flat_map(|turn| turn.iter().cloned())
        .collect()
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

    tools
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
