//! App-server attempt usage attribute tests.

use std::collections::BTreeMap;

use ag_contracts::{
    ApiEquivalentCost, ContextFill, CostBasis, CostEstimate, ModelUsage, PlanLogin, PricedCost,
    TurnUsage,
};
use ag_telemetry::Span;
use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

use crate::app_server::retry::record_attempt_usage;
use crate::telemetry::TRACER_PROVIDER_LOCK;

/// Returns usage of `model` with a cache write, a list-price cost, and a
/// context fill of `input_tokens` in `window_tokens`.
fn priced_usage(model: &str, input_tokens: u64, window_tokens: Option<u64>) -> ModelUsage {
    ModelUsage {
        cache_write_tokens: Some(4),
        context: Some(ContextFill::of_request(input_tokens, window_tokens)),
        cost: Some(CostEstimate::new(CostBasis::ListPrice, 30)),
        input_tokens,
        output_tokens: 2,
        requests: 1,
        ..ModelUsage::new(model)
    }
}

#[tokio::test]
async fn attempt_spans_record_cache_writes_cost_and_latest_context_when_known() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider.clone());
    let unpriced = ModelUsage {
        context: Some(ContextFill::of_request(70, None)),
        input_tokens: 70,
        requests: 1,
        ..ModelUsage::new("unpriced")
    };
    let cases = [
        (TurnUsage::default(), Vec::new()),
        (
            TurnUsage {
                checkpoint: None,
                models: vec![
                    priced_usage("first", 50, Some(100)),
                    priced_usage("second", 60, Some(200)),
                ],
            },
            vec![
                ("agentty.context.input_tokens", "60"),
                ("agentty.context.window_tokens", "200"),
                ("agentty.usage.cost.basis", "list"),
                ("agentty.usage.cost.micro_usd", "60"),
                ("agentty.usage.cost.upper_bound", "false"),
                ("gen_ai.usage.cache_creation.input_tokens", "8"),
            ],
        ),
        (
            TurnUsage::single(ModelUsage {
                cost: Some(
                    CostEstimate::new(CostBasis::ListPrice, 45).billed_to(PlanLogin::ChatGpt),
                ),
                ..priced_usage("plan", 50, None)
            }),
            vec![
                ("agentty.context.input_tokens", "50"),
                ("agentty.usage.api_equivalent.basis", "list"),
                ("agentty.usage.api_equivalent.login", "chatgpt"),
                ("agentty.usage.api_equivalent.micro_usd", "45"),
                ("agentty.usage.api_equivalent.upper_bound", "false"),
                ("gen_ai.usage.cache_creation.input_tokens", "4"),
            ],
        ),
        (
            TurnUsage::single(ModelUsage {
                cost: Some(CostEstimate::plan_billed(ApiEquivalentCost {
                    cost: PricedCost::new(CostBasis::ListPrice, 5),
                    login: None,
                })),
                ..priced_usage("plans", 50, None)
            }),
            vec![
                ("agentty.context.input_tokens", "50"),
                ("agentty.usage.api_equivalent.basis", "list"),
                ("agentty.usage.api_equivalent.micro_usd", "5"),
                ("agentty.usage.api_equivalent.upper_bound", "false"),
                ("gen_ai.usage.cache_creation.input_tokens", "4"),
            ],
        ),
        (
            TurnUsage {
                checkpoint: None,
                models: vec![priced_usage("first", 50, Some(100)), unpriced],
            },
            vec![("agentty.context.input_tokens", "70")],
        ),
    ];

    for (usage, expected) in cases {
        // Act
        Span::root("agent.attempt", Vec::new())
            .scope(async { record_attempt_usage(&usage) })
            .await;
        provider.force_flush().expect("spans should flush");

        // Assert
        let spans = exporter.get_finished_spans().expect("spans");
        exporter.reset();
        let recorded = spans[0]
            .attributes
            .iter()
            .map(|attribute| (attribute.key.to_string(), attribute.value.to_string()))
            .filter(|(key, _)| key != "agentty.outcome")
            .collect::<BTreeMap<_, _>>();
        let expected = expected
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(recorded, expected);
    }
}
