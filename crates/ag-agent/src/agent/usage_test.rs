//! Tests for harness cost conversion and list-price estimates.

use std::path::Path;

use ag_contracts::{AgentRequestKind, CostBasis, CostEstimate, ModelUsage, TurnUsage};
use ag_harness::catalog::{Pricing, PricingSource};
use ag_harness::provider::ModelProvider;
use ag_harness::{Cost, Usage};
use ag_session::AgentKind;

use crate::agent::ParsedResponse;
use crate::agent::usage::{cli_turn_usage, cost_estimate, list_price, priced_turn_usage};

/// Returns one reported request's token totals under an empty model name.
fn request_usage(input_tokens: u64, output_tokens: u64) -> ModelUsage {
    ModelUsage {
        input_tokens,
        output_tokens,
        requests: 1,
        ..ModelUsage::new("")
    }
}

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
            Some(CostEstimate {
                basis: CostBasis::ListPrice,
                micro_usd: 4_200_000,
            }),
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
        ("claude-opus-5-5", AgentKind::Claude, Some(opus_list_price)),
        ("not-a-model", AgentKind::Codex, None),
        ("gemini-model", AgentKind::Antigravity, None),
    ];

    for (model, kind, expected_cost) in cases {
        // Act
        let priced = priced_turn_usage(usage.clone(), model, kind);

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
