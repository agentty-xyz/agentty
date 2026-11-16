//! Cost of provider-reported tokens in integer micro-USD, and the usage a
//! session records for each of its turns.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::{Pricing, PricingSource};
use crate::run::Usage;
use crate::turn::TurnReport;

/// Tokens per million, the unit [`Pricing`] prices are quoted in.
const TOKENS_PER_MTOK: u128 = 1_000_000;

/// Price of some [`Usage`] in integer micro-USD, one millionth of a US
/// dollar, with where its prices came from.
///
/// [`Self::of`] prices usage as follows, given that the [`Usage`] counts
/// overlap: `cache_read` and a known `cache_write` are part of `input`, and
/// `reasoning` is part of `output`.
///
/// - Input tokens neither read from nor written to the cache bill at the input
///   price.
/// - `cache_read` tokens bill at [`Pricing::cache_read_per_mtok_micro_usd`] and
///   `cache_write` tokens at [`Pricing::cache_write_per_mtok_micro_usd`]; both
///   fall back to the input price when the pricing declares no cache price, as
///   built-in prices do not yet.
/// - Output tokens, reasoning included, bill at the output price.
///
/// The exact sum is rounded once to the nearest micro-USD, halves up, so a
/// run's cost, the sum of its requests' costs, can differ from pricing its
/// summed usage by less than one micro-USD per request. Arithmetic never
/// overflows: a cost beyond `u64::MAX` micro-USD saturates at it.
///
/// A model without [`Pricing`], such as a host model nobody priced, an
/// OpenAI-compatible server, or a plan-billed `ChatGPT` model whose marginal
/// requests are not metered, has no cost: `Option<Cost>` is `None` there.
/// On the wire it is `{"micro_usd": 1250, "pricing": "built_in"}`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Cost {
    /// Price in micro-USD.
    pub micro_usd: u64,
    /// Where the prices came from.
    pub pricing: PricingSource,
}

impl Cost {
    /// Prices `usage` with `pricing`, as the type documentation describes.
    pub fn of(usage: &Usage, pricing: Pricing) -> Self {
        let cache_write = usage.cache_write.unwrap_or(0);
        let uncached_input = usage
            .input
            .saturating_sub(usage.cache_read)
            .saturating_sub(cache_write);
        // One product is below `u128::MAX`, since both factors are `u64`.
        let scaled = [
            (uncached_input, pricing.input_per_mtok_micro_usd()),
            (usage.cache_read, pricing.cache_read_per_mtok_micro_usd()),
            (cache_write, pricing.cache_write_per_mtok_micro_usd()),
            (usage.output, pricing.output_per_mtok_micro_usd()),
        ]
        .into_iter()
        .map(|(tokens, price)| u128::from(tokens) * u128::from(price))
        .fold(0, u128::saturating_add);
        let micro_usd = scaled.saturating_add(TOKENS_PER_MTOK / 2) / TOKENS_PER_MTOK;

        Self {
            micro_usd: u64::try_from(micro_usd).unwrap_or(u64::MAX),
            pricing: pricing.source(),
        }
    }

    /// Returns the summed cost of every model request in `report`, priced
    /// one by one, or `None` without `pricing`.
    pub(crate) fn of_report(report: &TurnReport, pricing: Option<Pricing>) -> Option<Self> {
        let pricing = pricing?;

        Some(
            report
                .model_requests()
                .iter()
                .map(|request| {
                    let usage = Usage::from_request(
                        request
                            .completion()
                            .and_then(|completion| completion.usage()),
                    );

                    Self::of(&usage, pricing)
                })
                .fold(Self::zero(pricing), Self::saturating_add),
        )
    }

    /// Returns the zero cost of prices from `pricing`.
    pub(crate) fn zero(pricing: Pricing) -> Self {
        Self {
            micro_usd: 0,
            pricing: pricing.source(),
        }
    }

    /// Adds `other` to this cost, saturating, keeping this cost's source.
    #[must_use]
    pub(crate) fn saturating_add(self, other: Self) -> Self {
        Self {
            micro_usd: self.micro_usd.saturating_add(other.micro_usd),
            pricing: self.pricing,
        }
    }
}

/// Usage a store records for one turn, returned by
/// [`crate::store::SessionStore::load_usage`] and carried by
/// [`crate::store::StoredTurn::usage`].
///
/// It covers the model requests the turn completed so far, whether the turn
/// went on to complete, fail, reach a run limit, or be interrupted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TurnUsage {
    /// Cost of [`Self::usage`], or `None` when the turn's model had no
    /// [`Pricing`].
    pub cost: Option<Cost>,
    /// Tokens of the turn's completed model requests.
    pub usage: Usage,
}

impl TurnUsage {
    /// Returns the usage of a turn before its first model request: no
    /// tokens, at zero cost under `pricing`.
    pub fn new(pricing: Option<Pricing>) -> Self {
        Self {
            cost: pricing.map(Cost::zero),
            usage: Usage::default(),
        }
    }

    /// Adds one completed request's `usage` and `cost`.
    #[must_use]
    pub(crate) fn add(self, usage: Usage, cost: Option<Cost>) -> Self {
        Self {
            cost: self
                .cost
                .zip(cost)
                .map(|(total, cost)| total.saturating_add(cost)),
            usage: self.usage.saturating_add(usage),
        }
    }
}

/// Usage summed over a session's settled turns, returned by
/// [`crate::Session::usage`].
///
/// Turns recorded before this harness version stored usage, or by a store
/// that does not record it, are counted in [`Self::unknown_turns`] and add
/// nothing, so totals with unknown turns are lower bounds. Turns whose
/// model had no [`Pricing`] add their tokens but no cost and are counted in
/// [`Self::unpriced_turns`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct UsageTotals {
    /// Summed cost of the priced turns in micro-USD, saturating at
    /// `u64::MAX`, whatever each turn's [`PricingSource`].
    pub cost_micro_usd: u64,
    /// Settled turns, including unknown and unpriced ones.
    pub turns: u64,
    /// Settled turns without recorded usage.
    pub unknown_turns: u64,
    /// Settled turns with recorded usage but no cost.
    pub unpriced_turns: u64,
    /// Summed tokens of the turns with recorded usage.
    pub usage: Usage,
}

impl UsageTotals {
    /// Sums the recorded usage of `turns`, one entry per settled turn.
    pub(crate) fn from_turns(turns: &[Option<TurnUsage>]) -> Self {
        let mut totals = Self::default();
        let mut usage: Option<Usage> = None;
        for turn in turns {
            totals.turns = totals.turns.saturating_add(1);
            let Some(turn) = turn else {
                totals.unknown_turns = totals.unknown_turns.saturating_add(1);
                continue;
            };
            usage = Some(usage.map_or(turn.usage, |total| total.saturating_add(turn.usage)));
            match turn.cost {
                Some(cost) => {
                    totals.cost_micro_usd = totals.cost_micro_usd.saturating_add(cost.micro_usd);
                }
                None => totals.unpriced_turns = totals.unpriced_turns.saturating_add(1),
            }
        }
        totals.usage = usage.unwrap_or_default();

        totals
    }
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
