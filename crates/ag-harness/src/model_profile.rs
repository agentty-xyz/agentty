use std::num::NonZeroU64;

use crate::calibration::CALIBRATED_CAPACITY_PERCENT;
use crate::context::{ContextBudget, ContextBudgetError, HeuristicContextEstimator};

/// Ten thousand, the scale of a percent of a percent.
const PERMYRIAD: u64 = 10_000;

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

    /// Reserves `max_output_tokens` plus the margin a request needs when its
    /// estimate under-counts provider tokens by up to `under_count_percent`.
    ///
    /// Requests fill at most 95% of the input budget once provider usage
    /// calibrated the estimate, and 90% before. The margin shrinks the budget
    /// so that a request filling that share still fits the window beside
    /// `max_output_tokens` while its real tokens exceed the estimate by
    /// `under_count_percent`. Calibration never predicts fewer tokens than
    /// the estimate adds, so content the estimator under-counts is the error
    /// left: declare the most the model's estimator under-counted measured
    /// requests, plus a buffer. An estimator off by no more than the unfilled
    /// share needs no margin. Denser content overflows the window instead,
    /// and the harness re-sends the request reduced. The reservation
    /// saturates instead of overflowing.
    #[must_use]
    pub const fn with_estimate_margin(
        self,
        max_output_tokens: u64,
        under_count_percent: u64,
    ) -> Self {
        let output_free_tokens = self.context_window_tokens.saturating_sub(max_output_tokens);
        let filled_permyriad =
            CALIBRATED_CAPACITY_PERCENT.saturating_mul(100_u64.saturating_add(under_count_percent));
        let input_tokens = (output_free_tokens / filled_permyriad)
            .saturating_mul(PERMYRIAD)
            .saturating_add(
                (output_free_tokens % filled_permyriad).saturating_mul(PERMYRIAD)
                    / filled_permyriad,
            );
        let margin_tokens = output_free_tokens.saturating_sub(input_tokens);

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
