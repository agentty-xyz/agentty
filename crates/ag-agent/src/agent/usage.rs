//! Conversion of harness usage and prices into the shared usage contract,
//! and list-price estimates for agents that report tokens without a price.

use std::path::Path;

use ag_contracts::{
    AgentRequestKind, CostBasis, CostEstimate, ModelUsage, PlanLogin, PricedCost, TurnUsage,
};
use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::provider::ModelProvider;
use ag_harness::{Cost, Usage};
use ag_session::AgentKind;

use crate::agent::ParsedResponse;
use crate::agent::claude_usage::{self, ClaudeRun};

/// Returns the shared estimate of the harness `cost` of `usage`, priced
/// with `pricing` when known. It is an upper bound when `usage` read from
/// the cache and those reads were priced at the input price: the tier of
/// `pricing` its prompt bills at declares no cache price, or the pricing is
/// unknown, which [`Cost::of`] treats the same way.
pub(crate) fn cost_estimate(
    cost: Cost,
    usage: &ModelUsage,
    pricing: Option<Pricing>,
) -> CostEstimate {
    let declares_cache_read =
        pricing.is_some_and(|pricing| pricing.for_prompt(usage.input_tokens).declares_cache_read());

    CostEstimate::charged(PricedCost {
        basis: match cost.pricing {
            PricingSource::BuiltIn => CostBasis::ListPrice,
            PricingSource::Host => CostBasis::HostPrice,
            _ => CostBasis::Mixed,
        },
        micro_usd: cost.micro_usd,
        upper_bound: usage.cache_read_tokens.unwrap_or(0) > 0 && !declares_cache_read,
    })
}

/// How an agent's provider bills its usage, as the agent's login reveals.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Billing {
    /// Charged per token, as with an API key, or not known to be otherwise.
    #[default]
    Metered,
    /// Billed a flat fee by the subscription plan of a login, so a priced
    /// figure is only its API equivalent.
    Plan(PlanLogin),
}

impl Billing {
    /// Returns `cost` as this billing presents it: unchanged when metered,
    /// and as the plan's API equivalent under a plan.
    pub(crate) fn present(self, cost: Option<CostEstimate>) -> Option<CostEstimate> {
        match self {
            Self::Metered => cost,
            Self::Plan(login) => cost.map(|cost| cost.billed_to(login)),
        }
    }
}

/// Returns the usage of one CLI turn's processes on `model`, in the order
/// they ran: the turn's own process and its protocol repairs.
///
/// Claude Code's runs are charged their share of its `modelUsage` totals,
/// as [`claude_usage::turn_usage`] describes for `request_kind` run in
/// `folder`, continuing from `checkpoint`. Other agents' runs are summed at
/// list price. Runs billed to a plan carry API-equivalent costs.
pub(crate) fn cli_turn_usage(
    kind: AgentKind,
    model: &str,
    request_kind: &AgentRequestKind,
    folder: &Path,
    checkpoint: Option<&str>,
    runs: &[&ParsedResponse],
) -> TurnUsage {
    if kind == AgentKind::Claude {
        return claude_usage::turn_usage(
            model,
            request_kind,
            folder,
            checkpoint,
            runs.iter().map(|run| ClaudeRun {
                billing: run.billing,
                main_loop: &run.usage,
                totals: run.totals.as_ref(),
            }),
        );
    }
    let mut usage = TurnUsage::default();
    for run in runs {
        usage.merge(&priced_turn_usage(
            run.usage.clone(),
            model,
            kind,
            run.billing,
        ));
    }

    usage
}

/// Returns the usage an agent reported without naming its model recorded
/// under the requested `model`, priced at the catalog list price when the
/// provider `kind` runs is cataloged: Codex runs `OpenAI` models and Claude
/// Code runs Anthropic models. Under plan `billing` the price is the
/// usage's API equivalent.
pub(crate) fn priced_turn_usage(
    mut usage: ModelUsage,
    model: &str,
    kind: AgentKind,
    billing: Billing,
) -> TurnUsage {
    usage.model = model.to_string();
    usage.cost = billing.present(match kind {
        AgentKind::Claude => list_price(&usage, ModelProvider::Anthropic),
        AgentKind::Codex => list_price(&usage, ModelProvider::OpenAi),
        _ => None,
    });

    TurnUsage::single(usage)
}

/// Returns the list price of `usage` at the prices `provider` publishes for
/// its model in the harness provider catalog, or `None` for a model the
/// catalog does not price and for usage whose every request went
/// unreported, whose charge is unknown rather than zero.
pub(crate) fn list_price(usage: &ModelUsage, provider: ModelProvider) -> Option<CostEstimate> {
    if usage.unreported_requests >= usage.requests {
        return None;
    }
    let pricing = provider.model_spec(&usage.model)?.pricing()?;

    Some(price(usage, pricing))
}

/// Prices `usage` with `pricing` as the harness prices one request: cache
/// reads and writes at their declared prices, the rest of the input at the
/// input price, and output, reasoning included, at the output price.
/// Unreported cache counts price as uncached input, and the estimate is an
/// upper bound as [`cost_estimate`] describes.
pub(crate) fn price(usage: &ModelUsage, pricing: Pricing) -> CostEstimate {
    let mut tokens = Usage::default();
    tokens.input = usage.input_tokens;
    tokens.output = usage.output_tokens;
    tokens.cache_read = usage.cache_read_tokens.unwrap_or(0);
    tokens.cache_write = usage.cache_write_tokens;

    cost_estimate(Cost::of(&tokens, pricing), usage, Some(pricing))
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
