//! Fixtures shared by adapter unit suites.

use ag_contracts::{ModelUsage, TurnUsage};

/// Returns one request's usage of `model` with only token totals reported.
pub(crate) fn token_usage(model: &str, input_tokens: u64, output_tokens: u64) -> TurnUsage {
    TurnUsage::single(ModelUsage {
        input_tokens,
        output_tokens,
        requests: 1,
        ..ModelUsage::new(model)
    })
}
