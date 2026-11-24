//! OpenTelemetry projection of the usage a turn or utility run reported:
//! span attributes with its totals, and counters of its estimated cost and
//! prompt-cache tokens.
//!
//! Unknown figures are omitted rather than reported as zero, so dashboards
//! never sum an unknown cost or cache count as a known one. The charge of
//! metered usage and the API equivalent of plan-billed usage stay apart:
//! separate span attributes, and separate `agentty.usage.cost` series whose
//! basis is `api_equivalent`.

use ag_contracts::{CostEstimate, ModelUsage, PricedCost, TurnUsage};
use ag_telemetry::{Context, KeyValue, Span, TraceContextExt as _};
use opentelemetry::global;

const ATTRIBUTE_API_EQUIVALENT_BASIS: &str = "agentty.usage.api_equivalent.basis";
const ATTRIBUTE_API_EQUIVALENT_LOGIN: &str = "agentty.usage.api_equivalent.login";
const ATTRIBUTE_API_EQUIVALENT_MICRO_USD: &str = "agentty.usage.api_equivalent.micro_usd";
const ATTRIBUTE_API_EQUIVALENT_UPPER_BOUND: &str = "agentty.usage.api_equivalent.upper_bound";
const ATTRIBUTE_CACHE_OP: &str = "agentty.cache.op";
const ATTRIBUTE_CACHE_CREATION_INPUT_TOKENS: &str = "gen_ai.usage.cache_creation.input_tokens";
const ATTRIBUTE_CACHE_READ_INPUT_TOKENS: &str = "gen_ai.usage.cache_read.input_tokens";
const ATTRIBUTE_CONTEXT_PEAK_TOKENS: &str = "agentty.context.peak_tokens";
const ATTRIBUTE_COST_BASIS: &str = "agentty.usage.cost.basis";
const ATTRIBUTE_COST_MICRO_USD: &str = "agentty.usage.cost.micro_usd";
const ATTRIBUTE_COST_UPPER_BOUND: &str = "agentty.usage.cost.upper_bound";
const ATTRIBUTE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
const ATTRIBUTE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
const ATTRIBUTE_REASONING_OUTPUT_TOKENS: &str = "gen_ai.usage.reasoning.output_tokens";
const ATTRIBUTE_REQUEST_MODEL: &str = "gen_ai.request.model";
const ATTRIBUTE_REQUESTS: &str = "agentty.usage.requests";
const ATTRIBUTE_UNREPORTED_REQUESTS: &str = "agentty.usage.unreported_requests";
const BASIS_API_EQUIVALENT: &str = "api_equivalent";
const CACHE_TOKENS_DESCRIPTION: &str =
    "Input tokens read from or written to a provider-managed prompt cache.";
const CACHE_TOKENS_METRIC: &str = "agentty.usage.cache_tokens";
const CACHE_TOKENS_UNIT: &str = "{token}";
const COST_DESCRIPTION: &str = "Estimated cost of model usage in micro-USD: the charge of metered \
                                usage, or with basis api_equivalent what plan-billed usage would \
                                cost at API prices. Provider bills stay authoritative.";
const COST_METRIC: &str = "agentty.usage.cost";
const COST_UNIT: &str = "{micro_usd}";
const INSTRUMENTATION_SCOPE: &str = "agentty";

/// Records the totals of `usage` on the current span, such as the
/// `session.turn` span a host's turn workflow runs in.
pub fn record_turn_usage(usage: &TurnUsage) {
    Context::current()
        .span()
        .set_attributes(usage_attributes(usage));
}

/// Records the totals of the usage of one agent or utility run on its
/// `span` and adds its cost and cache tokens to the usage counters.
pub(crate) fn record_run_usage(span: &Span, usage: &TurnUsage) {
    span.context()
        .span()
        .set_attributes(usage_attributes(usage));
    record_usage_metrics(usage);
}

/// Returns the span attributes of the totals of `usage` over every model:
/// token counts, request counts, the estimated cost when every model's is
/// known, and the peak context fill. Usage of no model has none.
fn usage_attributes(usage: &TurnUsage) -> Vec<KeyValue> {
    let Some(total) = usage.total() else {
        return Vec::new();
    };
    let mut attributes = vec![
        count(ATTRIBUTE_INPUT_TOKENS, total.input_tokens),
        count(ATTRIBUTE_OUTPUT_TOKENS, total.output_tokens),
        count(ATTRIBUTE_REQUESTS, u64::from(total.requests)),
        count(
            ATTRIBUTE_UNREPORTED_REQUESTS,
            u64::from(total.unreported_requests),
        ),
    ];
    attributes.extend(
        [
            (ATTRIBUTE_CACHE_READ_INPUT_TOKENS, total.cache_read_tokens),
            (
                ATTRIBUTE_CACHE_CREATION_INPUT_TOKENS,
                total.cache_write_tokens,
            ),
            (ATTRIBUTE_REASONING_OUTPUT_TOKENS, total.reasoning_tokens),
            (
                ATTRIBUTE_CONTEXT_PEAK_TOKENS,
                total.context.map(|context| context.peak_input_tokens),
            ),
        ]
        .into_iter()
        .filter_map(|(key, tokens)| tokens.map(|tokens| count(key, tokens))),
    );
    if let Some(cost) = total.cost {
        attributes.extend(cost_attributes(cost));
    }

    attributes
}

/// Returns the attributes of an estimated `cost`: its charge under
/// `agentty.usage.cost.*` and its API equivalent, with the plan login when
/// known, under `agentty.usage.api_equivalent.*`.
fn cost_attributes(cost: CostEstimate) -> Vec<KeyValue> {
    let priced = |cost: PricedCost, keys: [&'static str; 3]| {
        let [micro_usd, basis, upper_bound] = keys;

        [
            count(micro_usd, cost.micro_usd),
            KeyValue::new(basis, cost.basis.as_str()),
            KeyValue::new(upper_bound, cost.upper_bound),
        ]
    };
    let mut attributes = Vec::new();
    if let Some(charge) = cost.charge() {
        attributes.extend(priced(
            charge,
            [
                ATTRIBUTE_COST_MICRO_USD,
                ATTRIBUTE_COST_BASIS,
                ATTRIBUTE_COST_UPPER_BOUND,
            ],
        ));
    }
    if let Some(equivalent) = cost.api_equivalent() {
        attributes.extend(priced(
            equivalent.cost,
            [
                ATTRIBUTE_API_EQUIVALENT_MICRO_USD,
                ATTRIBUTE_API_EQUIVALENT_BASIS,
                ATTRIBUTE_API_EQUIVALENT_UPPER_BOUND,
            ],
        ));
        if let Some(login) = equivalent.login {
            attributes.push(KeyValue::new(
                ATTRIBUTE_API_EQUIVALENT_LOGIN,
                login.as_str(),
            ));
        }
    }

    attributes
}

/// Adds each model's known estimated cost and prompt-cache tokens in
/// `usage` to the `agentty.usage.cost` and `agentty.usage.cache_tokens`
/// counters of the global meter provider. A charge counts under its price
/// basis and an API equivalent under the `api_equivalent` basis, so the
/// two never sum into one series.
fn record_usage_metrics(usage: &TurnUsage) {
    let meter = global::meter(INSTRUMENTATION_SCOPE);
    let cost_counter = meter
        .u64_counter(COST_METRIC)
        .with_description(COST_DESCRIPTION)
        .with_unit(COST_UNIT)
        .build();
    let cache_counter = meter
        .u64_counter(CACHE_TOKENS_METRIC)
        .with_description(CACHE_TOKENS_DESCRIPTION)
        .with_unit(CACHE_TOKENS_UNIT)
        .build();
    for model_usage in &usage.models {
        let ModelUsage {
            cache_read_tokens,
            cache_write_tokens,
            cost,
            model,
            ..
        } = model_usage;
        let model = KeyValue::new(ATTRIBUTE_REQUEST_MODEL, model.clone());
        let priced = cost.iter().flat_map(|cost| {
            [
                cost.charge().map(|charge| (charge, charge.basis.as_str())),
                cost.api_equivalent()
                    .map(|equivalent| (equivalent.cost, BASIS_API_EQUIVALENT)),
            ]
        });
        for (cost, basis) in priced.flatten() {
            cost_counter.add(
                cost.micro_usd,
                &[
                    model.clone(),
                    KeyValue::new(ATTRIBUTE_COST_BASIS, basis),
                    KeyValue::new(ATTRIBUTE_COST_UPPER_BOUND, cost.upper_bound),
                ],
            );
        }
        for (operation, tokens) in [("read", cache_read_tokens), ("write", cache_write_tokens)] {
            if let Some(tokens) = tokens {
                cache_counter.add(
                    *tokens,
                    &[model.clone(), KeyValue::new(ATTRIBUTE_CACHE_OP, operation)],
                );
            }
        }
    }
}

/// Returns a count attribute, saturating at the largest exported integer.
fn count(key: &'static str, value: u64) -> KeyValue {
    KeyValue::new(key, i64::try_from(value).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[path = "usage_telemetry_test.rs"]
mod tests;
