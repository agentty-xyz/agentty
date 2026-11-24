//! Conversion of harness usage and prices into the shared usage contract,
//! and list-price estimates for agents that report tokens without a price.

use ag_contracts::{AgentRequestKind, CostBasis, CostEstimate, ModelUsage, TurnUsage};
use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::provider::ModelProvider;
use ag_harness::{Cost, Usage};
use ag_session::AgentKind;

use crate::agent::ParsedResponse;
use crate::agent::claude_usage::{self, ClaudeRun};

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

/// Returns the usage of one CLI turn's processes on `model`, in the order
/// they ran: the turn's own process and its protocol repairs.
///
/// Claude Code's runs are charged their share of its `modelUsage` totals,
/// as [`claude_usage::turn_usage`] describes for `request_kind`, continuing
/// from `checkpoint`. Other agents' runs are summed at list price.
pub(crate) fn cli_turn_usage(
    kind: AgentKind,
    model: &str,
    request_kind: &AgentRequestKind,
    checkpoint: Option<&str>,
    runs: &[&ParsedResponse],
) -> TurnUsage {
    if kind == AgentKind::Claude {
        return claude_usage::turn_usage(
            model,
            request_kind,
            checkpoint,
            runs.iter().map(|run| ClaudeRun {
                main_loop: &run.usage,
                totals: run.totals.as_ref(),
            }),
        );
    }
    let mut usage = TurnUsage::default();
    for run in runs {
        usage.merge(&priced_turn_usage(run.usage.clone(), model, kind));
    }

    usage
}

/// Returns the usage an agent reported without naming its model recorded
/// under the requested `model`, priced at the catalog list price when the
/// provider `kind` runs is cataloged: Codex runs `OpenAI` models and Claude
/// Code runs Anthropic models.
pub(crate) fn priced_turn_usage(mut usage: ModelUsage, model: &str, kind: AgentKind) -> TurnUsage {
    usage.model = model.to_string();
    usage.cost = match kind {
        AgentKind::Claude => list_price(&usage, ModelProvider::Anthropic),
        AgentKind::Codex => list_price(&usage, ModelProvider::OpenAi),
        _ => None,
    };

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
