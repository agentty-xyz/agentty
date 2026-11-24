//! Tests for per-model folding, list pricing, and context fill of harness
//! model requests.

use std::collections::BTreeMap;

use ag_contracts::{ContextFill, CostBasis, CostEstimate, ModelUsage, TurnUsage};
use ag_harness::RunEventKind;
use ag_harness::provider::ModelProvider;
use serde_json::{Value, json};

use super::UsageMeter;
use crate::agent::native::ModelSelection;

/// Returns a `model_request_completed` event answered by `model`.
fn request(model: &Value, cost: &Value, input: u64, unreported_requests: u32) -> RunEventKind {
    serde_json::from_value(json!({
        "type": "model_request_completed",
        "cost": cost,
        "model": model,
        "usage": {
            "input": input,
            "output": 100,
            "cache_read": input / 2,
            "cache_write": null,
            "reasoning": 10,
            "unreported_requests": unreported_requests
        }
    }))
    .expect("run event")
}

/// Returns a meter for a run on `key` with an empty environment.
fn meter(key: &str) -> UsageMeter {
    UsageMeter::new(
        &ModelSelection::from_key(key).expect("known model"),
        &BTreeMap::new(),
        None,
    )
}

/// Returns the context window the harness provider catalog declares for an
/// Anthropic model.
fn anthropic_window(model: &str) -> Option<u64> {
    ModelProvider::Anthropic
        .model_spec(model)
        .and_then(|spec| spec.context_window())
}

#[test]
fn run_model_requests_are_list_priced_and_fill_the_run_window() {
    // Arrange
    let mut meter = meter("anthropic/claude-opus-5-5");

    // Act
    meter.observe(&request(&Value::Null, &Value::Null, 1_000_000, 0));
    let usage = meter
        .observe(&request(&Value::Null, &Value::Null, 400_000, 0))
        .cloned()
        .expect("request folded");

    // Assert
    assert_eq!(
        usage.models,
        [ModelUsage {
            cache_read_tokens: Some(700_000),
            cache_write_tokens: None,
            context: Some(ContextFill {
                last_input_tokens: 400_000,
                peak_input_tokens: 1_000_000,
                window_tokens: anthropic_window("claude-opus-5-5"),
            }),
            cost: Some(CostEstimate {
                basis: CostBasis::ListPrice,
                micro_usd: 2_944_000,
            }),
            input_tokens: 1_400_000,
            output_tokens: 200,
            reasoning_tokens: Some(20),
            requests: 2,
            ..ModelUsage::new("claude-opus-5-5")
        }]
    );
}

#[test]
fn summary_requests_keep_their_model_price_and_window() {
    // Arrange
    let mut meter = meter("anthropic/claude-opus-5-5");
    let haiku_window = anthropic_window("claude-haiku-5-5");

    // Act
    meter.observe(&request(
        &json!("anthropic/claude-haiku-5-5"),
        &json!({"micro_usd": 42, "pricing": "host"}),
        2_000,
        0,
    ));
    meter.observe(&request(&Value::Null, &Value::Null, 1_000, 0));
    let usage = meter.finish();

    // Assert
    let summary = &usage.models[0];
    assert_eq!(summary.model, "claude-haiku-5-5");
    assert_eq!(
        summary.cost,
        Some(CostEstimate {
            basis: CostBasis::HostPrice,
            micro_usd: 42,
        })
    );
    assert_eq!(
        summary.context.and_then(|context| context.window_tokens),
        haiku_window
    );
    assert_eq!(usage.models[1].model, "claude-opus-5-5");
    assert_eq!(
        usage.context().map(|context| context.last_input_tokens),
        Some(1_000)
    );
}

#[test]
fn unreported_requests_of_uncataloged_models_stay_unpriced() {
    // Arrange
    let mut meter = meter("openai-compatible/qwen3:latest");

    // Act
    let usage = meter
        .observe(&request(&Value::Null, &Value::Null, 0, 1))
        .cloned()
        .expect("request folded");

    // Assert
    let model = &usage.models[0];
    assert_eq!(model.model, "openai-compatible/qwen3:latest");
    assert_eq!((model.requests, model.unreported_requests), (1, 1));
    assert_eq!(model.context, None);
    assert_eq!(model.cost, None);
}

#[test]
fn other_events_fold_nothing() {
    // Arrange
    let mut meter = meter("anthropic/claude-opus-5-5");
    let event: RunEventKind =
        serde_json::from_value(json!({"type": "item_started", "id": 1, "item_kind": "message"}))
            .expect("run event");

    // Act
    let usage = meter.observe(&event);

    // Assert
    assert_eq!(usage, None);
    assert_eq!(meter.finish(), TurnUsage::default());
}
