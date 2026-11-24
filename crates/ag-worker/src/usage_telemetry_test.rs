//! Usage telemetry tests: span attributes of usage totals and the cost and
//! prompt-cache counters.

use std::collections::BTreeMap;

use ag_contracts::{
    ApiEquivalentCost, ContextFill, CostBasis, CostEstimate, ModelUsage, PlanLogin, PricedCost,
    TurnUsage,
};
use ag_telemetry::Span;
use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{
    AggregatedMetrics, MetricData, ResourceMetrics, ScopeMetrics,
};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

use super::{record_turn_usage, record_usage_metrics, usage_attributes};
use crate::test_support::TRACER_PROVIDER_LOCK;

/// Returns usage of `model` with every detail known, estimated at `cost`.
fn detailed_usage(model: &str, cost: CostEstimate) -> ModelUsage {
    ModelUsage {
        cache_read_tokens: Some(40),
        cache_write_tokens: Some(10),
        context: Some(ContextFill::of_request(90, Some(1_000))),
        cost: Some(cost),
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: Some(5),
        requests: 2,
        unreported_requests: 0,
        ..ModelUsage::new(model)
    }
}

/// Returns the API equivalent of 300 micro-USD of usage billed to a Claude
/// subscription, an upper bound since cache reads were priced as input.
fn plan_billed() -> CostEstimate {
    CostEstimate::plan_billed(ApiEquivalentCost {
        cost: PricedCost {
            upper_bound: true,
            ..PricedCost::new(CostBasis::ListPrice, 300)
        },
        login: Some(PlanLogin::Claude),
    })
}

fn attribute_map(usage: &TurnUsage) -> BTreeMap<String, String> {
    usage_attributes(usage)
        .into_iter()
        .map(|attribute| (attribute.key.to_string(), attribute.value.to_string()))
        .collect()
}

fn expected(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// Returns the attributes two detailed models share, besides their cost.
fn two_model_totals() -> Vec<(&'static str, &'static str)> {
    vec![
        ("agentty.context.peak_tokens", "90"),
        ("agentty.usage.requests", "4"),
        ("agentty.usage.unreported_requests", "0"),
        ("gen_ai.usage.cache_creation.input_tokens", "20"),
        ("gen_ai.usage.cache_read.input_tokens", "80"),
        ("gen_ai.usage.input_tokens", "200"),
        ("gen_ai.usage.output_tokens", "40"),
        ("gen_ai.usage.reasoning.output_tokens", "10"),
    ]
}

#[test]
fn usage_attributes_report_known_totals_and_omit_unknown_ones() {
    // Arrange
    let list = CostEstimate::new(CostBasis::ListPrice, 250);
    let unpriced = ModelUsage {
        context: Some(ContextFill::of_request(300, None)),
        input_tokens: 300,
        output_tokens: 7,
        requests: 2,
        unreported_requests: 1,
        ..ModelUsage::new("local-model")
    };
    let two_models = |first: CostEstimate, second: CostEstimate| TurnUsage {
        checkpoint: None,
        models: vec![
            detailed_usage("first-model", first),
            detailed_usage("second-model", second),
        ],
    };
    let with_costs = |costs: &[(&'static str, &'static str)]| {
        let mut pairs = two_model_totals();
        pairs.extend_from_slice(costs);

        expected(&pairs)
    };
    let cases = [
        (TurnUsage::default(), expected(&[])),
        (
            TurnUsage::single(detailed_usage("priced-model", list)),
            expected(&[
                ("agentty.context.peak_tokens", "90"),
                ("agentty.usage.cost.basis", "list"),
                ("agentty.usage.cost.micro_usd", "250"),
                ("agentty.usage.cost.upper_bound", "false"),
                ("agentty.usage.requests", "2"),
                ("agentty.usage.unreported_requests", "0"),
                ("gen_ai.usage.cache_creation.input_tokens", "10"),
                ("gen_ai.usage.cache_read.input_tokens", "40"),
                ("gen_ai.usage.input_tokens", "100"),
                ("gen_ai.usage.output_tokens", "20"),
                ("gen_ai.usage.reasoning.output_tokens", "5"),
            ]),
        ),
        (
            two_models(list, CostEstimate::new(CostBasis::HostPrice, 250)),
            with_costs(&[
                ("agentty.usage.cost.basis", "mixed"),
                ("agentty.usage.cost.micro_usd", "500"),
                ("agentty.usage.cost.upper_bound", "false"),
            ]),
        ),
        (
            two_models(list, plan_billed()),
            with_costs(&[
                ("agentty.usage.api_equivalent.basis", "list"),
                ("agentty.usage.api_equivalent.login", "claude"),
                ("agentty.usage.api_equivalent.micro_usd", "300"),
                ("agentty.usage.api_equivalent.upper_bound", "true"),
                ("agentty.usage.cost.basis", "list"),
                ("agentty.usage.cost.micro_usd", "250"),
                ("agentty.usage.cost.upper_bound", "false"),
            ]),
        ),
        (
            two_models(
                plan_billed(),
                CostEstimate::plan_billed(ApiEquivalentCost {
                    cost: PricedCost::new(CostBasis::ListPrice, 1),
                    login: Some(PlanLogin::ChatGpt),
                }),
            ),
            with_costs(&[
                ("agentty.usage.api_equivalent.basis", "list"),
                ("agentty.usage.api_equivalent.micro_usd", "301"),
                ("agentty.usage.api_equivalent.upper_bound", "true"),
            ]),
        ),
        (
            TurnUsage {
                checkpoint: None,
                models: vec![detailed_usage("priced-model", list), unpriced],
            },
            expected(&[
                ("agentty.context.peak_tokens", "300"),
                ("agentty.usage.requests", "4"),
                ("agentty.usage.unreported_requests", "1"),
                ("gen_ai.usage.input_tokens", "400"),
                ("gen_ai.usage.output_tokens", "27"),
            ]),
        ),
    ];

    for (usage, expected) in cases {
        // Act
        let attributes = attribute_map(&usage);

        // Assert
        assert_eq!(attributes, expected);
    }
}

#[test]
fn usage_attributes_saturate_counts_beyond_the_exported_integer_range() {
    // Arrange
    let usage = TurnUsage::single(ModelUsage {
        input_tokens: u64::MAX,
        requests: 1,
        ..ModelUsage::new("model")
    });

    // Act
    let attributes = attribute_map(&usage);

    // Assert
    assert_eq!(
        attributes.get("gen_ai.usage.input_tokens"),
        Some(&i64::MAX.to_string())
    );
}

#[tokio::test]
async fn record_turn_usage_annotates_the_current_span() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider.clone());
    let usage = TurnUsage::single(detailed_usage(
        "priced-model",
        CostEstimate::new(CostBasis::ListPrice, 250),
    ));

    // Act
    Span::root("session.turn", Vec::new())
        .scope(async { record_turn_usage(&usage) })
        .await;
    provider.force_flush().expect("spans should flush");

    // Assert
    let spans = exporter
        .get_finished_spans()
        .expect("spans should be exported");
    let turn = spans
        .iter()
        .find(|span| span.name == "session.turn")
        .expect("turn span should be exported");
    let recorded = turn
        .attributes
        .iter()
        .map(|attribute| (attribute.key.to_string(), attribute.value.to_string()))
        .collect::<BTreeMap<_, _>>();
    for (key, value) in attribute_map(&usage) {
        assert_eq!(recorded.get(&key), Some(&value), "{key}");
    }
}

#[test]
fn usage_metrics_count_charges_and_api_equivalents_apart_per_model() {
    // Arrange
    let exporter = InMemoryMetricExporter::default();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());
    let usage = TurnUsage {
        checkpoint: None,
        models: vec![
            detailed_usage(
                "priced-model",
                CostEstimate::new(CostBasis::AgentReported, 250),
            ),
            ModelUsage {
                cost: Some(plan_billed()),
                input_tokens: 3,
                requests: 1,
                ..ModelUsage::new("plan-model")
            },
            ModelUsage {
                input_tokens: 3,
                requests: 1,
                ..ModelUsage::new("unpriced-model")
            },
        ],
    };

    // Act
    record_usage_metrics(&usage);
    record_usage_metrics(&usage);
    meter_provider.force_flush().expect("metrics should flush");

    // Assert
    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("metrics should be exported");
    let mut points = BTreeMap::new();
    for metric in resource_metrics
        .iter()
        .flat_map(ResourceMetrics::scope_metrics)
        .flat_map(ScopeMetrics::metrics)
    {
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
            unreachable!("{} should be a u64 counter", metric.name());
        };
        for point in sum.data_points() {
            let mut attributes = point
                .attributes()
                .map(|attribute| format!("{}={}", attribute.key, attribute.value))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            points.insert(
                format!(
                    "{} {} {}",
                    metric.name(),
                    metric.unit(),
                    attributes.join(",")
                ),
                point.value(),
            );
        }
    }
    assert_eq!(
        points,
        BTreeMap::from([
            (
                "agentty.usage.cache_tokens {token} \
                 agentty.cache.op=read,gen_ai.request.model=priced-model"
                    .to_string(),
                80
            ),
            (
                "agentty.usage.cache_tokens {token} \
                 agentty.cache.op=write,gen_ai.request.model=priced-model"
                    .to_string(),
                20
            ),
            (
                "agentty.usage.cost {micro_usd} \
                 agentty.usage.cost.basis=agent_reported,agentty.usage.cost.upper_bound=false,\
                 gen_ai.request.model=priced-model"
                    .to_string(),
                500
            ),
            (
                "agentty.usage.cost {micro_usd} \
                 agentty.usage.cost.basis=api_equivalent,agentty.usage.cost.upper_bound=true,\
                 gen_ai.request.model=plan-model"
                    .to_string(),
                600
            ),
        ])
    );
}
