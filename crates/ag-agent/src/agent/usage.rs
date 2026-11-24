//! Conversion of harness usage and prices into the shared usage contract,
//! and list-price estimates for agents that report tokens without a price.

use ag_contracts::{CostBasis, CostEstimate, ModelUsage, TurnUsage};
use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::provider::ModelProvider;
use ag_harness::{Cost, Usage};
use ag_session::AgentKind;

/// Returns the shared estimate of a harness cost.
pub(crate) fn cost_estimate(cost: Cost) -> CostEstimate {
    CostEstimate {
        basis: match cost.pricing {
            PricingSource::BuiltIn => CostBasis::ListPrice,
            PricingSource::Host => CostBasis::HostPrice,
            _ => CostBasis::Mixed,
        },
        micro_usd: cost.micro_usd,
    }
}

/// Returns the list price of `model` served by `provider` from the harness
/// provider catalog, or `None` for a model it does not price.
pub(crate) fn catalog_pricing(provider: ModelProvider, model: &str) -> Option<Pricing> {
    provider.model_spec(model)?.pricing()
}

/// Returns the usage an agent reported without naming its model recorded
/// under the requested `model` and priced: at `reported_cost`, the agent's
/// own estimate of this usage in micro-USD, when it has one, else at the
/// catalog list price when the provider `kind` runs is cataloged. Claude
/// Code runs Anthropic models and Codex runs `OpenAI` models.
pub(crate) fn priced_turn_usage(
    mut usage: ModelUsage,
    model: &str,
    kind: AgentKind,
    reported_cost: Option<u64>,
) -> TurnUsage {
    let provider = match kind {
        AgentKind::Claude => Some(ModelProvider::Anthropic),
        AgentKind::Codex => Some(ModelProvider::OpenAi),
        _ => None,
    };
    usage.model = model.to_string();
    usage.cost = match reported_cost {
        Some(micro_usd) => Some(CostEstimate {
            basis: CostBasis::AgentReported,
            micro_usd,
        }),
        None => provider
            .and_then(|provider| catalog_pricing(provider, model))
            .map(|pricing| price(&usage, pricing)),
    };

    TurnUsage::single(usage)
}

/// Prices `usage` with `pricing` as the harness prices one request: cache
/// reads and writes at their declared prices, the rest of the input at the
/// input price, and output, reasoning included, at the output price.
/// Unreported cache counts price as uncached input.
pub(crate) fn price(usage: &ModelUsage, pricing: Pricing) -> CostEstimate {
    let mut tokens = Usage::default();
    tokens.input = usage.input_tokens;
    tokens.output = usage.output_tokens;
    tokens.cache_read = usage.cache_read_tokens.unwrap_or(0);
    tokens.cache_write = usage.cache_write_tokens;

    cost_estimate(Cost::of(&tokens, pricing))
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
