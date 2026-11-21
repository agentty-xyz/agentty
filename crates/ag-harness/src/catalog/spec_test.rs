//! Catalog data unit tests for identifiers, capabilities, prices, cost
//! tiers, and specifications.

use std::num::NonZeroU32;

use crate::catalog::{Capabilities, CostTier, ModelId, ModelSpec, Pricing, PricingSource};
use crate::model::ModelProfile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use crate::provider::ModelProvider;

#[test]
fn model_ids_convert_and_display() {
    // Arrange
    let from_str = ModelId::from("host/model");

    // Act
    let from_string = ModelId::from("host/model".to_string());

    // Assert
    assert_eq!(from_str, from_string);
    assert_eq!(from_str, ModelId::new("host/model"));
    assert_eq!(from_str.as_str(), "host/model");
    assert_eq!(from_str.to_string(), "host/model");
}

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn cataloged_model_ids_qualify_the_model_with_its_provider() {
    // Act
    let cataloged = ModelId::cataloged(ModelProvider::OpenAiCompatible, "qwen3:latest");

    // Assert
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
fn capabilities_report_each_declared_flag() {
    // Arrange
    let vision = Capabilities::NONE
        .with_image_input(true)
        .with_tool_calls(true);

    // Act
    let flags = |capabilities: Capabilities| {
        [
            capabilities.image_input(),
            capabilities.reasoning(),
            capabilities.structured_output(),
            capabilities.tool_calls(),
        ]
    };

    // Assert
    assert_eq!(flags(vision), [true, false, false, true]);
    assert_eq!(
        flags(
            Capabilities::NONE
                .with_reasoning(true)
                .with_structured_output(true)
        ),
        [false, true, true, false]
    );
    assert_eq!(
        flags(vision.with_image_input(false)),
        [false, false, false, true]
    );
}

#[test]
fn cost_tiers_display_lowercase_labels() {
    // Act
    let labels = [
        CostTier::Local,
        CostTier::Plan,
        CostTier::Cheap,
        CostTier::Standard,
        CostTier::Premium,
    ]
    .map(|tier| tier.to_string());

    // Assert
    assert_eq!(labels, ["local", "plan", "cheap", "standard", "premium"]);
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
fn long_context_prices_apply_only_above_their_threshold() {
    // Arrange
    let built_in = Pricing::built_in(100_000, 500_000);
    let declared_cache = Pricing::new(100_000, 500_000).with_cache_read(10_000);

    // Act
    let tiered = built_in.with_long_context(100_000, 500_000, 2_500_000);
    let short = tiered.for_prompt(100_000);
    let long = tiered.for_prompt(100_001);

    // Assert
    assert_eq!(tiered.source(), PricingSource::Host);
    assert_eq!(built_in.for_prompt(u64::MAX), built_in);
    assert_eq!(
        (
            short.input_per_mtok_micro_usd(),
            short.output_per_mtok_micro_usd()
        ),
        (100_000, 500_000)
    );
    assert_eq!(
        (
            long.input_per_mtok_micro_usd(),
            long.output_per_mtok_micro_usd()
        ),
        (500_000, 2_500_000)
    );
    // Undeclared cache prices follow the long-context input price, and the
    // result declares no further tier.
    assert_eq!(long.cache_read_per_mtok_micro_usd(), 500_000);
    assert_eq!(long.for_prompt(0), long);
    assert_eq!(tiered.cost_tier(), CostTier::Cheap);
    let declared_long = declared_cache
        .with_long_context(100_000, 500_000, 2_500_000)
        .for_prompt(200_000);
    assert_eq!(declared_long.cache_read_per_mtok_micro_usd(), 10_000);
}

#[test]
fn changing_a_built_in_cache_price_marks_the_prices_host_supplied() {
    // Arrange
    let built_in = Pricing::built_in(2_000_000, 8_000_000);

    // Act
    let cache_read = built_in.with_cache_read(200_000);
    let cache_write = built_in.with_cache_write(2_500_000);

    // Assert
    assert_eq!(cache_read.source(), PricingSource::Host);
    assert_eq!(cache_write.source(), PricingSource::Host);
    assert_eq!(cache_read.input_per_mtok_micro_usd(), 2_000_000);
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
        .with_profile(profile)
        .with_max_output(16_000)
        .with_capabilities(capabilities)
        .with_pricing(pricing)
        .with_max_concurrency(NonZeroU32::MIN.saturating_add(3));

    // Assert
    assert_eq!(unknown.id().as_str(), "host/model");
    assert_eq!(unknown.profile(), None);
    assert_eq!(unknown.context_window(), None);
    assert_eq!(unknown.max_output(), None);
    assert_eq!(unknown.capabilities(), Capabilities::NONE);
    assert_eq!(unknown.pricing(), None);
    assert_eq!(unknown.max_concurrency(), None);
    assert_eq!(declared.profile(), Some(profile));
    assert_eq!(declared.context_window(), Some(200_000));
    assert_eq!(declared.max_output(), Some(16_000));
    assert_eq!(declared.capabilities(), capabilities);
    assert_eq!(declared.pricing(), Some(pricing));
    assert_eq!(declared.max_concurrency().map(NonZeroU32::get), Some(4));
}

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn model_spec_records_its_built_in_provider() {
    // Arrange
    let unknown = ModelSpec::new("host/model");

    // Act
    let declared = unknown.clone().with_provider(ModelProvider::Kimi);

    // Assert
    assert_eq!(unknown.provider(), None);
    assert_eq!(declared.provider(), Some(ModelProvider::Kimi));
}
