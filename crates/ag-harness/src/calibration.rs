//! Calibration of approximate request weights from provider-reported usage.

use std::sync::{Arc, Mutex, PoisonError};

use crate::context::ContextBudget;
use crate::model::CompletionMetadata;
use crate::turn::TurnError;

/// Share, in percent, of the request capacity left after reserved output
/// that requests are fitted to until provider usage calibrated how tokens
/// grow with the estimate. The estimate under-counts some content, so an
/// uncalibrated estimate keeps a margin instead of filling the window.
pub(crate) const UNCALIBRATED_CAPACITY_PERCENT: u64 = 90;

/// Share, in percent, of the request capacity left after reserved output
/// that calibrated input tokens may fill. Live Muse and Qwen loops showed
/// the envelope under-predicting by up to about 5% when fitting turns
/// results into denser notices, so a calibrated request keeps that margin.
pub(crate) const CALIBRATED_CAPACITY_PERCENT: u64 = 95;

/// Smallest growth, in percent of the request capacity left after reserved
/// output, between the first calibrated request and a later one before
/// their difference measures the slope. Closer requests differ by too little
/// content for their token difference to be meaningful. A first request
/// smaller than this carries too little content for its ratio to measure the
/// slope either: providers add fixed costs the estimate may miss, which
/// dominate a small request.
pub(crate) const SLOPE_MIN_GROWTH_PERCENT: u64 = 10;

/// Smallest slope, in thousandths: content the estimate over-counts never
/// loosens the budget beyond the declared weights.
pub(crate) const MIN_SLOPE_PERMILLE: u64 = 1_000;

/// Largest slope, in thousandths, so one inconsistent usage report cannot
/// shrink the usable window below a third.
pub(crate) const MAX_SLOPE_PERMILLE: u64 = 3_000;

/// Share, in percent, of a request's estimated weight that later requests
/// are capped to after the provider rejected that request as exceeding its
/// context window. The rejection proves only that the request was too
/// large, not by how much, so each one shrinks the usable weight by a
/// quarter, as Roo Code retries an overflow at 75%; two rejections reach
/// about 56%.
pub(crate) const OVERFLOW_SHRINK_PERCENT: u64 = 75;

const PERMILLE: u64 = 1_000;

/// Provider-reported calibration of estimated request weight to input
/// tokens for one model and estimator, shared by a harness and the sessions
/// it captures.
///
/// Providers add fixed per-request costs the estimate may miss, such as chat
/// templates and rendered tool and output schemas, so one ratio fitted to a
/// small request overstates every larger one. The calibration is instead an
/// [`Envelope`]: a line through the first reported request whose slope is
/// the steepest token growth observed against it, lifted so every
/// observation lies on or below it. Until a large enough request measures
/// the slope, a small first request only offsets the estimate by its excess
/// tokens. Slope and lift only rise, since an under-estimate overflows the
/// provider window and fails the turn while an over-estimate only reduces
/// context earlier.
///
/// A provider that rejects a request as exceeding its context window
/// reports no usage, so the rejection instead caps the usable weight below
/// the rejected request (see [`Self::observe_overflow`]); the cap only
/// falls, like the envelope only rises.
#[derive(Clone, Debug, Default)]
pub(crate) struct ContextCalibration {
    state: Arc<Mutex<Calibration>>,
}

/// One consistent reading of a [`ContextCalibration`]: the usage envelope,
/// if any provider usage calibrated the estimate, and the weight cap left by
/// provider context-window rejections, if any.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Calibration {
    envelope: Option<Envelope>,
    overflow_ceiling: Option<u64>,
}

impl Calibration {
    /// Returns the usage envelope, or `None` while no provider usage
    /// calibrated the estimate.
    pub(crate) fn envelope(self) -> Option<Envelope> {
        self.envelope
    }
}

impl ContextCalibration {
    /// Returns `budget` with its request capacity reduced to the largest
    /// estimated weight whose calibrated tokens fit
    /// [`CALIBRATED_CAPACITY_PERCENT`] of it, or
    /// [`UNCALIBRATED_CAPACITY_PERCENT`] of it until provider usage
    /// calibrated how tokens grow with the estimate, and further to the
    /// overflow ceiling once a provider rejected a request as exceeding its
    /// context window; never above the declared capacity. Returns the
    /// calibration applied with it.
    pub(crate) fn effective_budget(&self, budget: ContextBudget) -> (ContextBudget, Calibration) {
        let capacity = budget.max_request_weight().get() - budget.reserved_output_weight();
        let calibration = *self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let limit = capacity.saturating_mul(Self::capacity_percent(calibration.envelope)) / 100;
        let weight = calibration
            .envelope
            .map_or(limit, |envelope| envelope.weight_within(limit));
        let weight = calibration
            .overflow_ceiling
            .map_or(weight, |ceiling| weight.min(ceiling));

        (budget.with_capacity(weight.min(capacity)), calibration)
    }

    /// Restates a [`TurnError::ContextBudgetExceeded`] measured against the
    /// effective budget derived from `budget` and `calibration` in the units
    /// of the declared `budget`: the budget as declared, and the calibrated
    /// tokens of the required weight grown by the margin the capacity keeps,
    /// or the weight itself when larger. A weight above the overflow ceiling
    /// is also scaled by how far the ceiling cut the declared capacity, so
    /// the restated requirement exceeds the declared budget whenever the
    /// ceiling rejected it. Returns any other error unchanged.
    pub(crate) fn declared_failure(
        budget: ContextBudget,
        calibration: Calibration,
        error: TurnError,
    ) -> TurnError {
        let TurnError::ContextBudgetExceeded { required, .. } = error else {
            return error;
        };
        let reserved = budget.reserved_output_weight();
        let capacity = budget.max_request_weight().get() - reserved;
        let weight = required.saturating_sub(reserved);
        let tokens = calibration
            .envelope
            .map_or(weight, |envelope| envelope.tokens(weight));
        let tokens = tokens
            .saturating_mul(100)
            .div_ceil(Self::capacity_percent(calibration.envelope));
        let ceiling_scaled = calibration
            .overflow_ceiling
            .filter(|ceiling| weight > *ceiling)
            .map_or(0, |ceiling| {
                let scaled = (u128::from(weight) * u128::from(capacity))
                    .div_ceil(u128::from(ceiling.max(1)));

                u64::try_from(scaled).unwrap_or(u64::MAX)
            });

        TurnError::ContextBudgetExceeded {
            budget: budget.max_request_weight().get(),
            required: reserved.saturating_add(tokens.max(weight).max(ceiling_scaled)),
        }
    }

    /// Adds the input tokens `completion` reports for a sent request of
    /// `estimated_weight`, without reserved output, to the calibration.
    /// Ignores responses without reported input tokens and empty requests.
    pub(crate) fn observe(
        &self,
        budget: ContextBudget,
        estimated_weight: u64,
        completion: Option<&CompletionMetadata>,
    ) {
        let Some(tokens) = completion
            .and_then(CompletionMetadata::usage)
            .and_then(|usage| usage.input_tokens())
        else {
            return;
        };
        if estimated_weight == 0 {
            return;
        }
        let capacity = budget.max_request_weight().get() - budget.reserved_output_weight();
        let min_growth = (capacity.saturating_mul(SLOPE_MIN_GROWTH_PERCENT) / 100).max(1);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let updated = match state.envelope {
            Some(current) => current.observe(estimated_weight, tokens, min_growth),
            None => Envelope::first(estimated_weight, tokens, min_growth),
        };
        state.envelope = Some(updated);
    }

    /// Records that the provider rejected a sent request of
    /// `rejected_weight`, without reserved output, as exceeding its context
    /// window: later requests are capped to [`OVERFLOW_SHRINK_PERCENT`] of
    /// that weight, or to a lower cap an earlier rejection left. For any
    /// rejected request heavier than one weight unit the cap lies strictly
    /// below it, so a request refitted under it is smaller or fails typed,
    /// and the cap lasts until the
    /// model or estimator changes, since a declared window the provider
    /// rejected stays wrong for later turns.
    pub(crate) fn observe_overflow(&self, rejected_weight: u64) {
        let ceiling = rejected_weight.saturating_mul(OVERFLOW_SHRINK_PERCENT) / 100;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.overflow_ceiling = Some(
            state
                .overflow_ceiling
                .map_or(ceiling, |current| current.min(ceiling)),
        );
    }

    /// Share of the capacity a request may fill under `envelope`.
    fn capacity_percent(envelope: Option<Envelope>) -> u64 {
        if envelope.is_some_and(|envelope| envelope.slope != Slope::Offset) {
            CALIBRATED_CAPACITY_PERCENT
        } else {
            UNCALIBRATED_CAPACITY_PERCENT
        }
    }
}

/// Upper envelope of calibrated input tokens over estimated request weight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Envelope {
    base_tokens: u64,
    base_weight: u64,
    lift: u64,
    slope: Slope,
    slope_permille: u64,
}

/// What an [`Envelope`]'s slope rests on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Slope {
    /// Assumed one token per unit: the first report was too small to
    /// measure a ratio, so its excess tokens only offset the estimate.
    Offset,
    /// The first report's ratio through the origin, which over-states every
    /// larger request with the same content mix.
    Ratio,
    /// Measured from token growth between reports.
    Measured,
}

impl Envelope {
    /// Returns the calibrated input tokens of a request of `weight`.
    pub(crate) fn tokens(self, weight: u64) -> u64 {
        let base = self.base_tokens.saturating_add(self.lift);
        if weight >= self.base_weight {
            base.saturating_add(Self::scale(weight - self.base_weight, self.slope_permille))
        } else {
            base.saturating_sub(Self::scale(self.base_weight - weight, self.slope_permille))
        }
    }

    /// Starts an envelope from the first report. The fixed cost and the
    /// slope are not yet separable: a report of at least `min_growth` starts
    /// from the ratio through it, which over-states larger requests, and a
    /// smaller one, whose tokens may be mostly fixed cost, from one token per
    /// unit through it, which offsets the estimate by its excess.
    fn first(weight: u64, tokens: u64, min_growth: u64) -> Self {
        let (slope, slope_permille) = if weight >= min_growth {
            (Slope::Ratio, Self::clamped_ratio(tokens, weight))
        } else {
            (Slope::Offset, MIN_SLOPE_PERMILLE)
        };

        Self {
            base_tokens: tokens,
            base_weight: weight,
            lift: 0,
            slope,
            slope_permille,
        }
    }

    /// Returns this envelope covering one more report. A request at least
    /// `min_growth` larger than the first measures the slope: the first
    /// such measurement replaces the starting slope and its lift, and later
    /// ones only raise it. The lift then rises to cover the report.
    fn observe(mut self, weight: u64, tokens: u64, min_growth: u64) -> Self {
        if weight >= self.base_weight.saturating_add(min_growth) {
            let slope = Self::clamped_ratio(
                tokens.saturating_sub(self.base_tokens),
                weight - self.base_weight,
            );
            if self.slope == Slope::Measured {
                self.slope_permille = self.slope_permille.max(slope);
            } else {
                self.lift = 0;
                self.slope = Slope::Measured;
                self.slope_permille = slope;
            }
        }
        self.lift = self
            .lift
            .max(tokens.saturating_sub(Self { lift: 0, ..self }.tokens(weight)));

        self
    }

    /// Returns the largest weight whose calibrated tokens fit `capacity`.
    fn weight_within(self, capacity: u64) -> u64 {
        let base = self.base_tokens.saturating_add(self.lift);

        if capacity >= base {
            let room = u128::from(capacity - base) * u128::from(PERMILLE);
            let above = room / u128::from(self.slope_permille);

            self.base_weight
                .saturating_add(u64::try_from(above).unwrap_or(u64::MAX))
        } else {
            // The fewest units below the base whose rounded-up tokens remove
            // more than all but one token of the excess.
            let excess = u128::from(base - capacity - 1) * u128::from(PERMILLE);
            let below = excess / u128::from(self.slope_permille) + 1;

            self.base_weight
                .saturating_sub(u64::try_from(below).unwrap_or(u64::MAX))
        }
    }

    /// Returns `tokens / weight` in thousandths, rounded up and clamped to
    /// [`MIN_SLOPE_PERMILLE`] and [`MAX_SLOPE_PERMILLE`].
    fn clamped_ratio(tokens: u64, weight: u64) -> u64 {
        let ratio = (u128::from(tokens) * u128::from(PERMILLE)).div_ceil(u128::from(weight));

        u64::try_from(ratio.min(u128::from(MAX_SLOPE_PERMILLE)))
            .unwrap_or(MAX_SLOPE_PERMILLE)
            .max(MIN_SLOPE_PERMILLE)
    }

    /// Returns `weight` scaled by `slope_permille`, rounded up.
    fn scale(weight: u64, slope_permille: u64) -> u64 {
        let tokens =
            (u128::from(weight) * u128::from(slope_permille)).div_ceil(u128::from(PERMILLE));

        u64::try_from(tokens).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
#[path = "calibration_test.rs"]
mod tests;
