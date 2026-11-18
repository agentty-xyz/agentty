//! Cost of provider-reported tokens in integer micro-USD, and the usage a
//! session records for each of its turns.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::{Pricing, PricingSource};
use crate::run::Usage;
use crate::turn::{ModelRequestActivity, TurnOutcome, TurnReport};

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
/// overflows: a cost beyond `u64::MAX` micro-USD saturates at it. A sum of
/// costs priced from different sources is [`PricingSource::Mixed`].
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

    /// Returns the summed [`crate::turn::ModelRequestActivity::cost`] of
    /// every model request in `report`, each priced at the prices of the
    /// model that answered it, or `None` without the run model's `pricing`
    /// or when any request's model had none.
    pub(crate) fn of_report(report: &TurnReport, pricing: Option<Pricing>) -> Option<Self> {
        report
            .model_requests()
            .iter()
            .map(ModelRequestActivity::cost)
            .fold(pricing.map(Self::zero), Self::sum)
    }

    /// Adds a request's `cost` to a running `total`, which stays `None`
    /// once either is unpriced.
    pub(crate) fn sum(total: Option<Self>, cost: Option<Self>) -> Option<Self> {
        total
            .zip(cost)
            .map(|(total, cost)| total.saturating_add(cost))
    }

    /// Returns the zero cost of prices from `pricing`.
    pub(crate) fn zero(pricing: Pricing) -> Self {
        Self {
            micro_usd: 0,
            pricing: pricing.source(),
        }
    }

    /// Adds `other` to this cost, saturating; the sum of costs from
    /// different sources is [`PricingSource::Mixed`].
    #[must_use]
    pub(crate) fn saturating_add(self, other: Self) -> Self {
        Self {
            micro_usd: self.micro_usd.saturating_add(other.micro_usd),
            pricing: if self.pricing == other.pricing {
                self.pricing
            } else {
                PricingSource::Mixed
            },
        }
    }
}

/// Usage a store records for one turn, returned by
/// [`crate::store::SessionStore::load_usage`] and carried by
/// [`crate::store::StoredTurn::usage`].
///
/// It covers the model requests the turn completed so far, whether the turn
/// went on to complete, fail, reach a run limit, or be interrupted. A store
/// that persists it chooses its own encoding; the harness's SQLite store
/// keeps a private record that does not follow the wire shapes of
/// [`Usage`] and [`Cost`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
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
    pub(crate) fn new(pricing: Option<Pricing>) -> Self {
        Self {
            cost: pricing.map(Cost::zero),
            usage: Usage::default(),
        }
    }

    /// Returns the usage a settled turn `recorded`, or, for a turn without
    /// it, such as one stored before usage was recorded, the unpriced usage
    /// of its stored `outcome`. `None` means the turn's usage is unknown.
    pub(crate) fn settled(recorded: Option<Self>, outcome: Option<&TurnOutcome>) -> Option<Self> {
        recorded.or_else(|| {
            outcome.map(|outcome| Self {
                cost: None,
                usage: Usage::from_report(outcome.report()),
            })
        })
    }
}

/// Storage record of a [`TurnUsage`] in the SQLite store.
///
/// It is kept apart from the public wire shapes of [`Usage`], [`Cost`], and
/// [`PricingSource`], so they can change without breaking stored turns. Add
/// fields only with `#[serde(default)]` and never rename one; a change that
/// cannot decode earlier records needs a data migration.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StoredTurnUsage {
    cache_read: u64,
    cache_write: Option<u64>,
    cost: Option<StoredCost>,
    input: u64,
    output: u64,
    reasoning: u64,
    unreported_requests: u32,
}

/// Storage record of a [`Cost`].
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredCost {
    micro_usd: u64,
    pricing: StoredPricingSource,
}

/// Storage record of a [`PricingSource`].
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredPricingSource {
    BuiltIn,
    Host,
    Mixed,
}

impl From<&TurnUsage> for StoredTurnUsage {
    fn from(turn: &TurnUsage) -> Self {
        let usage = turn.usage;

        Self {
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            cost: turn.cost.map(|cost| StoredCost {
                micro_usd: cost.micro_usd,
                pricing: match cost.pricing {
                    PricingSource::BuiltIn => StoredPricingSource::BuiltIn,
                    PricingSource::Host => StoredPricingSource::Host,
                    PricingSource::Mixed => StoredPricingSource::Mixed,
                },
            }),
            input: usage.input,
            output: usage.output,
            reasoning: usage.reasoning,
            unreported_requests: usage.unreported_requests,
        }
    }
}

impl From<StoredTurnUsage> for TurnUsage {
    fn from(stored: StoredTurnUsage) -> Self {
        Self {
            cost: stored.cost.map(|cost| Cost {
                micro_usd: cost.micro_usd,
                pricing: match cost.pricing {
                    StoredPricingSource::BuiltIn => PricingSource::BuiltIn,
                    StoredPricingSource::Host => PricingSource::Host,
                    StoredPricingSource::Mixed => PricingSource::Mixed,
                },
            }),
            usage: Usage {
                input: stored.input,
                output: stored.output,
                cache_read: stored.cache_read,
                cache_write: stored.cache_write,
                reasoning: stored.reasoning,
                unreported_requests: stored.unreported_requests,
            },
        }
    }
}

/// Usage summed over a session's settled turns, returned by
/// [`crate::Session::usage`].
///
/// Turns without recorded usage or a stored outcome to derive it from, such
/// as turns recorded before this harness version stored usage without a
/// host request ID, are counted in [`Self::unknown_turns`] and add nothing,
/// so totals with unknown turns are lower bounds. Turns whose model had no
/// [`Pricing`], and older turns whose usage comes from their stored outcome,
/// add their tokens but no cost and are counted in [`Self::unpriced_turns`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct UsageTotals {
    /// Summed cost of the priced turns, saturating at `u64::MAX` micro-USD,
    /// [`PricingSource::Mixed`] when their prices came from different
    /// sources, or `None` when no turn was priced.
    pub cost: Option<Cost>,
    /// Settled turns, including unknown and unpriced ones.
    pub turns: u64,
    /// Settled turns without known usage.
    pub unknown_turns: u64,
    /// Settled turns with known usage but no cost.
    pub unpriced_turns: u64,
    /// Summed tokens of the turns with known usage.
    pub usage: Usage,
}

impl UsageTotals {
    /// Sums the usage of `turns`, one entry per settled turn.
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
                    totals.cost =
                        Some(totals.cost.map_or(cost, |total| total.saturating_add(cost)));
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
