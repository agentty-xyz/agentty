//! Catalog data unit tests for identifiers, capabilities, prices, cost
//! tiers, and specifications.

use crate::catalog::{Capabilities, ModelId, ModelSpec, Pricing, PricingSource};
use crate::model::ModelProfile;
use crate::provider::{CostTier, ModelProvider};

#[test]
fn model_ids_convert_display_and_qualify_cataloged_models() {
    // Arrange
    let from_str = ModelId::from("host/model");
    let from_string = ModelId::from("host/model".to_string());

    // Act
    let cataloged = ModelId::cataloged(ModelProvider::OpenAiCompatible, "qwen3:latest");

    // Assert
    assert_eq!(from_str, from_string);
    assert_eq!(from_str, ModelId::new("host/model"));
    assert_eq!(from_str.as_str(), "host/model");
    assert_eq!(cataloged.to_string(), "openai-compatible/qwen3:latest");
}

#[test]
fn capabilities_satisfy_only_needs_they_include() {
    // Arrange
    let vision = Capabilities::NONE
        .with_image_input(true)
        .with_tool_calls(true);
    let every = Capabilities::NONE
        .with_image_input(true)
        .with_reasoning(true)
        .with_structured_output(true)
        .with_tool_calls(true);

    // Act
    let needs = [
        Capabilities::NONE.with_image_input(true),
        Capabilities::NONE.with_reasoning(true),
        Capabilities::NONE.with_structured_output(true),
        Capabilities::NONE.with_tool_calls(true),
    ];

    // Assert
    assert_eq!(Capabilities::default(), Capabilities::NONE);
    assert!(Capabilities::NONE.satisfies(Capabilities::NONE));
    assert!(vision.satisfies(vision));
    assert_eq!(
        needs.map(|need| vision.satisfies(need)),
        [true, false, false, true]
    );
    assert!(needs.iter().all(|need| every.satisfies(*need)));
    assert!(!Capabilities::NONE.satisfies(every));
}

#[test]
fn capabilities_display_lists_set_capabilities() {
    // Arrange
    let every = Capabilities::NONE
        .with_image_input(true)
        .with_reasoning(true)
        .with_structured_output(true)
        .with_tool_calls(true);

    // Act
    let labels = [Capabilities::NONE, every].map(|capabilities| capabilities.to_string());

    // Assert
    assert_eq!(
        labels,
        [
            "none",
            "image_input, reasoning, structured_output, tool_calls"
        ]
    );
}

#[test]
fn host_pricing_falls_back_to_the_input_price_for_cache_tokens() {
    // Arrange
    let pricing = Pricing::new(2_000_000, 8_000_000);

    // Act
    let cached = pricing.with_cache_read(500_000).with_cache_write(2_500_000);

    // Assert
    assert_eq!(pricing.source(), PricingSource::Host);
    assert_eq!(pricing.input_per_mtok_micro_usd(), 2_000_000);
    assert_eq!(pricing.output_per_mtok_micro_usd(), 8_000_000);
    assert_eq!(pricing.cache_read_per_mtok_micro_usd(), 2_000_000);
    assert_eq!(pricing.cache_write_per_mtok_micro_usd(), 2_000_000);
    assert_eq!(cached.cache_read_per_mtok_micro_usd(), 500_000);
    assert_eq!(cached.cache_write_per_mtok_micro_usd(), 2_500_000);
    assert_eq!(Pricing::built_in(1, 2).source(), PricingSource::BuiltIn);
}

#[test]
fn pricing_derives_cost_tier_from_the_output_price_bounds() {
    // Arrange
    let output_prices = [0, 2_500_000, 2_500_001, 14_999_999, 15_000_000];

    // Act
    let tiers = output_prices.map(|output| Pricing::new(0, output).cost_tier());

    // Assert
    assert_eq!(
        tiers,
        [
            CostTier::Cheap,
            CostTier::Cheap,
            CostTier::Standard,
            CostTier::Standard,
            CostTier::Premium
        ]
    );
}

#[test]
fn model_spec_starts_unknown_and_records_declarations() {
    // Arrange
    let profile = ModelProfile::new(200_000).with_reserved_tokens(20_000);
    let capabilities = Capabilities::NONE.with_tool_calls(true);
    let pricing = Pricing::new(1_000_000, 3_000_000);
    let unknown = ModelSpec::new("host/model");

    // Act
    let declared = unknown
        .clone()
        .with_provider(ModelProvider::Kimi)
        .with_profile(profile)
        .with_max_output(16_000)
        .with_capabilities(capabilities)
        .with_pricing(pricing)
        .with_max_concurrency(4);

    // Assert
    assert_eq!(unknown.id().as_str(), "host/model");
    assert_eq!(unknown.provider(), None);
    assert_eq!(unknown.profile(), None);
    assert_eq!(unknown.context_window(), None);
    assert_eq!(unknown.max_output(), None);
    assert_eq!(unknown.capabilities(), Capabilities::NONE);
    assert_eq!(unknown.pricing(), None);
    assert_eq!(unknown.max_concurrency(), None);
    assert_eq!(declared.provider(), Some(ModelProvider::Kimi));
    assert_eq!(declared.profile(), Some(profile));
    assert_eq!(declared.context_window(), Some(200_000));
    assert_eq!(declared.max_output(), Some(16_000));
    assert_eq!(declared.capabilities(), capabilities);
    assert_eq!(declared.pricing(), Some(pricing));
    assert_eq!(declared.max_concurrency(), Some(4));
}
