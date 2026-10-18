use std::num::NonZeroU64;

use crate::context::{ContextBudget, ContextBudgetError, HeuristicContextEstimator};

/// Host-declared description of one model's context, in provider tokens.
///
/// The harness reads every registered model through its profile and never
/// branches on model identity: registration derives the [`ContextBudget`]
/// bounding each provider request from the profile, one weight unit per
/// token, and weighs request content with the profile's
/// [`HeuristicContextEstimator`]. Construction is `const`, so hosts can
/// declare one profile per model as a constant.
///
/// A profile only shapes how new requests are fitted: request fingerprints,
/// stored history, and recorded outcomes never depend on it, and in-memory
/// calibration restarts with every harness. Revising a profile therefore
/// needs no new [`crate::recovery::ExecutionIdentity`] revision, which would
/// instead strand every session stored under the previous one; resumed
/// sessions fit their next request under the revised profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelProfile {
    context_window_tokens: u64,
    estimator: HeuristicContextEstimator,
    reserved_tokens: u64,
}

impl ModelProfile {
    /// Declares a model's total context window with no tokens reserved.
    pub const fn new(context_window_tokens: u64) -> Self {
        Self {
            context_window_tokens,
            estimator: HeuristicContextEstimator::DEFAULT,
            reserved_tokens: 0,
        }
    }

    /// Weighs the model's request content with `estimator`, whose parameters
    /// should come from the model's measured token counts; profiles start
    /// with [`HeuristicContextEstimator::DEFAULT`].
    #[must_use]
    pub const fn with_estimator(mut self, estimator: HeuristicContextEstimator) -> Self {
        self.estimator = estimator;

        self
    }

    /// Reserves `reserved_tokens` of the window for the response and provider
    /// overhead, keeping them out of every request's input.
    #[must_use]
    pub const fn with_reserved_tokens(mut self, reserved_tokens: u64) -> Self {
        self.reserved_tokens = reserved_tokens;

        self
    }

    /// Reserves `max_output_tokens` plus a quarter of the window, for a model
    /// whose input is measured by an approximate estimator.
    ///
    /// [`HeuristicContextEstimator::DEFAULT`] estimates one token per
    /// four bytes, which under-counts real provider tokens: live probes
    /// measured up to 29% more tokens than estimated on varied prose and 17%
    /// on repository content. Holding back a quarter of the window keeps an
    /// input that fills the budget beside the reserved output while real
    /// tokens exceed the estimate by at least a third, and by more as the
    /// output share grows. The reservation saturates instead of overflowing.
    #[must_use]
    pub const fn with_estimate_margin(self, max_output_tokens: u64) -> Self {
        let margin_tokens = self.context_window_tokens / 4;

        self.with_reserved_tokens(max_output_tokens.saturating_add(margin_tokens))
    }

    /// Returns the declared total context window in tokens.
    pub const fn context_window_tokens(self) -> u64 {
        self.context_window_tokens
    }

    /// Returns the estimator weighing the model's request content.
    pub const fn estimator(self) -> HeuristicContextEstimator {
        self.estimator
    }

    /// Returns the tokens reserved for the response and provider overhead.
    pub const fn reserved_tokens(self) -> u64 {
        self.reserved_tokens
    }

    /// Returns the input tokens left after the reservation, or zero when the
    /// reservation covers the whole window.
    pub const fn input_token_budget(self) -> u64 {
        self.context_window_tokens
            .saturating_sub(self.reserved_tokens)
    }

    /// Derives the request budget bounding every provider request, with the
    /// reservation held back as reserved output.
    ///
    /// # Errors
    /// Returns [`ContextBudgetError`] when the reservation leaves no input
    /// capacity, including for an empty window.
    pub fn context_budget(self) -> Result<ContextBudget, ContextBudgetError> {
        let Some(context_window) = NonZeroU64::new(self.context_window_tokens) else {
            return Err(ContextBudgetError::ReservedOutputExceedsBudget {
                max_request_weight: 0,
                reserved_output_weight: self.reserved_tokens,
            });
        };

        ContextBudget::new(context_window).with_reserved_output(self.reserved_tokens)
    }
}

#[cfg(test)]
#[path = "model_profile_test.rs"]
mod tests;
