//! Attribute names and values for the estimated cost of model usage, shared
//! by every span and metric that exports one.
//!
//! The charge of metered usage and the API equivalent of plan-billed usage
//! use separate attributes, so the two are never summed into one figure.

use opentelemetry::KeyValue;

/// Price basis of an estimated charge, such as `list` or `host`; on the
/// `agentty.usage.cost` counter, `api_equivalent` marks API equivalents.
pub const ATTRIBUTE_COST_BASIS: &str = "agentty.usage.cost.basis";

/// Whether an estimated amount is an upper bound, its cache reads priced at
/// the input price.
pub const ATTRIBUTE_COST_UPPER_BOUND: &str = "agentty.usage.cost.upper_bound";

const ATTRIBUTE_API_EQUIVALENT_BASIS: &str = "agentty.usage.api_equivalent.basis";
const ATTRIBUTE_API_EQUIVALENT_LOGIN: &str = "agentty.usage.api_equivalent.login";
const ATTRIBUTE_API_EQUIVALENT_MICRO_USD: &str = "agentty.usage.api_equivalent.micro_usd";
const ATTRIBUTE_API_EQUIVALENT_UPPER_BOUND: &str = "agentty.usage.api_equivalent.upper_bound";
const ATTRIBUTE_COST_MICRO_USD: &str = "agentty.usage.cost.micro_usd";

/// One estimated amount of model usage in integer micro-USD.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CostAmount {
    /// Stable label of where the prices came from, such as `list`.
    pub basis: &'static str,
    /// Amount in micro-USD.
    pub micro_usd: u64,
    /// Whether the amount is an upper bound.
    pub upper_bound: bool,
}

/// Returns the attributes of an estimated cost: the `charge` of metered
/// usage under `agentty.usage.cost.*`, and the `api_equivalent` of
/// plan-billed usage, with its plan `login` when known, under
/// `agentty.usage.api_equivalent.*`. Absent amounts add no attributes.
pub fn cost_attributes(
    charge: Option<CostAmount>,
    api_equivalent: Option<CostAmount>,
    login: Option<&'static str>,
) -> Vec<KeyValue> {
    let amount = |amount: CostAmount, [micro_usd, basis, upper_bound]: [&'static str; 3]| {
        [
            KeyValue::new(
                micro_usd,
                i64::try_from(amount.micro_usd).unwrap_or(i64::MAX),
            ),
            KeyValue::new(basis, amount.basis),
            KeyValue::new(upper_bound, amount.upper_bound),
        ]
    };
    let mut attributes = Vec::new();
    if let Some(charge) = charge {
        attributes.extend(amount(
            charge,
            [
                ATTRIBUTE_COST_MICRO_USD,
                ATTRIBUTE_COST_BASIS,
                ATTRIBUTE_COST_UPPER_BOUND,
            ],
        ));
    }
    if let Some(api_equivalent) = api_equivalent {
        attributes.extend(amount(
            api_equivalent,
            [
                ATTRIBUTE_API_EQUIVALENT_MICRO_USD,
                ATTRIBUTE_API_EQUIVALENT_BASIS,
                ATTRIBUTE_API_EQUIVALENT_UPPER_BOUND,
            ],
        ));
        if let Some(login) = login {
            attributes.push(KeyValue::new(ATTRIBUTE_API_EQUIVALENT_LOGIN, login));
        }
    }

    attributes
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;
