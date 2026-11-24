//! Tests for harness cost conversion and list-price estimates.

use std::path::Path;

use ag_contracts::{
    AgentRequestKind, CostBasis, CostEstimate, ModelUsage, PlanLogin, PricedCost, TurnUsage,
};
use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::provider::ModelProvider;
use ag_harness::{Cost, Usage};
use ag_session::AgentKind;

use crate::agent::ParsedResponse;
use crate::agent::usage::{
    Billing, cli_turn_usage, cost_estimate, list_price, price, priced_turn_usage,
};

/// Returns one reported request's token totals under an empty model name.
fn request_usage(input_tokens: u64, output_tokens: u64) -> ModelUsage {
    ModelUsage {
        input_tokens,
        output_tokens,
        requests: 1,
        ..ModelUsage::new("")
    }
}

/// Returns the charged amount of `cost`, which must have one.
fn charged(cost: Option<CostEstimate>) -> PricedCost {
    cost.and_then(CostEstimate::charge).expect("a charge")
}

#[test]
fn cost_sources_map_to_bases() {
    // Arrange
    let host = Cost::of(&Usage::default(), Pricing::new(1, 1));
    let mut mixed = host;
    mixed.pricing = PricingSource::Mixed;
    let usage = request_usage(0, 0);

    // Act
    let bases = [host, mixed].map(|cost| charged(Some(cost_estimate(cost, &usage, None))).basis);

    // Assert
    assert_eq!(bases, [CostBasis::HostPrice, CostBasis::Mixed]);
}

#[test]
fn harness_costs_with_cache_reads_are_upper_bounds_unless_a_cache_price_applies() {
    // Arrange
    let cached = ModelUsage {
        cache_read_tokens: Some(400),
        ..request_usage(1_000, 10)
    };
    let cost = Cost::of(&Usage::default(), Pricing::new(1, 1));
    let cache_priced = Pricing::new(1, 1).with_cache_read(0);

    // Act
    let unknown_pricing = charged(Some(cost_estimate(cost, &cached, None)));
    let undeclared = charged(Some(cost_estimate(cost, &cached, Some(Pricing::new(1, 1)))));
    let declared = charged(Some(cost_estimate(cost, &cached, Some(cache_priced))));

    // Assert
    assert!(unknown_pricing.upper_bound);
    assert!(undeclared.upper_bound);
    assert!(!declared.upper_bound);
}

#[test]
fn a_cli_turn_on_an_openai_model_prices_its_cached_input_at_the_cached_price() {
    // Arrange
    // About 6.7k input tokens, 3.3k of them read from the cache, and 60
    // output tokens, as a CLI turn reports them.
    let turn = |model: &str| ModelUsage {
        cache_read_tokens: Some(3_300),
        model: model.to_string(),
        ..request_usage(6_700, 60)
    };

    // Act
    let sol = charged(list_price(&turn("gpt-6.1-sol"), ModelProvider::OpenAi));
    let terra = charged(list_price(&turn("gpt-5.6-terra"), ModelProvider::OpenAi));

    // Assert
    // The 3.4k uncached input tokens bill at the $2 input price and the
    // 3.3k cached ones at each model's declared cached-input price, so
    // neither estimate is an upper bound.
    assert_eq!(
        sol,
        PricedCost {
            basis: CostBasis::ListPrice,
            micro_usd: 6_800 + 330 + 600,
            upper_bound: false,
        }
    );
    assert_eq!(terra.micro_usd, 6_800 + 660 + 720);
    assert!(!terra.upper_bound);
}

#[test]
fn list_price_needs_a_priced_model_and_a_reported_request() {
    // Arrange
    let opus = ModelUsage {
        cache_read_tokens: Some(500_000),
        cache_write_tokens: Some(100_000),
        model: "claude-opus-5-5".to_string(),
        ..request_usage(1_000_000, 100_000)
    };
    let cases = [
        (
            opus.clone(),
            ModelProvider::Anthropic,
            Some(CostEstimate::new(CostBasis::ListPrice, 4_200_000)),
        ),
        (
            ModelUsage::unreported("claude-opus-5-5", 1),
            ModelProvider::Anthropic,
            None,
        ),
        (
            ModelUsage {
                model: "not-a-model".to_string(),
                ..opus
            },
            ModelProvider::OpenAi,
            None,
        ),
    ];

    for (usage, provider, expected) in cases {
        // Act
        let cost = list_price(&usage, provider);

        // Assert
        assert_eq!(cost, expected);
    }
}

#[test]
fn cache_reads_without_a_declared_cache_price_make_the_price_an_upper_bound() {
    // Arrange
    let cached = ModelUsage {
        cache_read_tokens: Some(400),
        ..request_usage(1_000, 10)
    };
    let uncached = ModelUsage {
        cache_read_tokens: Some(0),
        ..request_usage(1_000, 10)
    };
    let input_price_only = Pricing::new(1_000_000, 2_000_000);

    // Act
    let undeclared = price(&cached, input_price_only);
    let declared = price(&cached, input_price_only.with_cache_read(100_000));
    let without_reads = price(&uncached, input_price_only);
    // Only prompts above 500 tokens bill at a tier without a cache price.
    let long_tier_undeclared = price(
        &cached,
        input_price_only
            .with_cache_read(100_000)
            .with_long_context(500, input_price_only),
    );

    // Assert
    assert!(charged(Some(long_tier_undeclared)).upper_bound);
    assert_eq!(
        undeclared,
        CostEstimate::charged(PricedCost {
            basis: CostBasis::HostPrice,
            micro_usd: 1_000 + 20,
            upper_bound: true,
        })
    );
    assert_eq!(
        declared,
        CostEstimate::new(CostBasis::HostPrice, 600 + 40 + 20)
    );
    assert!(!charged(Some(without_reads)).upper_bound);
}

#[test]
fn priced_turn_usage_names_the_model_and_prices_cataloged_providers() {
    // Arrange
    let usage = request_usage(1_000_000, 100_000);
    let opus_list_price = list_price(
        &ModelUsage {
            model: "claude-opus-5-5".to_string(),
            ..usage
        },
        ModelProvider::Anthropic,
    )
    .expect("Opus is priced");
    let cases = [
        (
            "claude-opus-5-5",
            AgentKind::Claude,
            Billing::Metered,
            Some(opus_list_price),
        ),
        (
            "claude-opus-5-5",
            AgentKind::Claude,
            Billing::Plan(PlanLogin::Claude),
            Some(opus_list_price.billed_to(PlanLogin::Claude)),
        ),
        (
            "not-a-model",
            AgentKind::Codex,
            Billing::Plan(PlanLogin::ChatGpt),
            None,
        ),
        (
            "gemini-model",
            AgentKind::Antigravity,
            Billing::Metered,
            None,
        ),
    ];

    for (model, kind, billing, expected_cost) in cases {
        // Act
        let priced = priced_turn_usage(usage.clone(), model, kind, billing);

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
fn cli_turn_usage_sums_other_agents_runs_under_the_model() {
    // Arrange
    let run = |input_tokens, output_tokens| ParsedResponse {
        billing: Billing::Metered,
        content: String::new(),
        follow_up_contents: Vec::new(),
        totals: None,
        usage: request_usage(input_tokens, output_tokens),
    };
    let turn = run(100, 10);
    let repair = run(40, 4);

    // Act
    let usage = cli_turn_usage(
        AgentKind::Codex,
        "not-a-model",
        &AgentRequestKind::SessionResume,
        Path::new("/worktree"),
        None,
        &[&turn, &repair],
    );

    // Assert
    assert_eq!(
        usage,
        TurnUsage::single(ModelUsage {
            input_tokens: 140,
            output_tokens: 14,
            requests: 2,
            ..ModelUsage::new("not-a-model")
        })
    );
}
