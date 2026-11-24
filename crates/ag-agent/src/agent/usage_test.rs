//! Tests for harness cost conversion and list-price estimates.

use ag_contracts::{CostBasis, CostEstimate, ModelUsage, TurnUsage};
use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::provider::ModelProvider;
use ag_harness::{Cost, Usage};
use ag_session::AgentKind;

use crate::agent::usage::{catalog_pricing, cost_estimate, price, priced_turn_usage};

#[test]
fn cost_sources_map_to_bases() {
    // Arrange
    let host = Cost::of(&Usage::default(), Pricing::new(1, 1));
    let mut mixed = host;
    mixed.pricing = PricingSource::Mixed;

    // Act
    let bases = [host, mixed].map(|cost| cost_estimate(cost).basis);

    // Assert
    assert_eq!(bases, [CostBasis::HostPrice, CostBasis::Mixed]);
}

#[test]
fn catalog_prices_claude_cache_reads_and_writes() {
    // Arrange
    let pricing =
        catalog_pricing(ModelProvider::Anthropic, "claude-opus-5-5").expect("Opus is priced");
    let usage = ModelUsage {
        cache_read_tokens: Some(500_000),
        cache_write_tokens: Some(100_000),
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        ..ModelUsage::new("claude-opus-5-5")
    };

    // Act
    let cost = price(&usage, pricing);

    // Assert
    assert_eq!(
        cost,
        CostEstimate {
            basis: CostBasis::ListPrice,
            micro_usd: 4_200_000,
        }
    );
}

#[test]
fn priced_turn_usage_names_the_model_and_prefers_the_agent_reported_cost() {
    // Arrange
    let usage = ModelUsage {
        input_tokens: 1_000_000,
        output_tokens: 100_000,
        requests: 1,
        ..ModelUsage::new("")
    };
    let opus_list_price = price(
        &usage,
        catalog_pricing(ModelProvider::Anthropic, "claude-opus-5-5").expect("Opus is priced"),
    );
    let cases = [
        (
            "claude-opus-5-5",
            AgentKind::Claude,
            Some(12_500),
            Some(CostEstimate {
                basis: CostBasis::AgentReported,
                micro_usd: 12_500,
            }),
        ),
        (
            "claude-opus-5-5",
            AgentKind::Claude,
            None,
            Some(opus_list_price),
        ),
        ("not-a-model", AgentKind::Codex, None, None),
        ("gemini-model", AgentKind::Antigravity, None, None),
    ];

    for (model, kind, reported_cost, expected_cost) in cases {
        // Act
        let priced = priced_turn_usage(usage.clone(), model, kind, reported_cost);

        // Assert
        assert_eq!(
            priced,
            TurnUsage::single(ModelUsage {
                cost: expected_cost,
                model: model.to_string(),
                ..usage.clone()
            })
        );
    }
}

#[test]
fn unknown_models_have_no_catalog_price() {
    // Arrange, Act
    let pricing = catalog_pricing(ModelProvider::OpenAi, "not-a-model");

    // Assert
    assert_eq!(pricing, None);
}
