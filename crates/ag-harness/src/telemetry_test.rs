use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, HistogramDataPoint, Metric, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};

use super::{
    AGENT_CALL_BOUNDARIES, AGENT_DURATION_BOUNDARIES_SECONDS, AGENT_DURATION_DESCRIPTION,
    AGENT_DURATION_METRIC, AGENT_INFERENCE_CALLS_DESCRIPTION, AGENT_INFERENCE_CALLS_METRIC,
    AGENT_INFERENCE_CALLS_UNIT, AGENT_TOOL_CALLS_DESCRIPTION, AGENT_TOOL_CALLS_METRIC,
    AGENT_TOOL_CALLS_UNIT, DURATION_BOUNDARIES_SECONDS, DURATION_METRIC, DURATION_UNIT,
    ERROR_REPOSITORY_REQUIRED, ERROR_REQUEST, LifecycleMetrics, RequestMetrics,
    TOOL_DURATION_DESCRIPTION, TOOL_DURATION_METRIC,
};
use crate::lifecycle::{LifecycleEmitter, ModelResponseType, ToolErrorType, TurnErrorType};
use crate::model::{ModelErrorType, ModelMetadata};

fn attributes<T>(point: &HistogramDataPoint<T>) -> Vec<(&str, String)> {
    let mut attributes = point
        .attributes()
        .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
        .collect::<Vec<_>>();
    attributes.sort_unstable();

    attributes
}

fn metric<'metrics>(metrics: &'metrics [&Metric], name: &str) -> &'metrics Metric {
    metrics
        .iter()
        .find(|metric| metric.name() == name)
        .copied()
        .expect("metric should be exported")
}

fn record_lifecycle_fixtures(lifecycle: &LifecycleEmitter) {
    lifecycle
        .start_model_request(None, 0, None)
        .expect("observer should start a standalone model request")
        .completed(None, ModelResponseType::Output);

    let successful_turn = lifecycle
        .start_turn()
        .expect("observer should start a turn");
    let successful_turn_id = successful_turn.id();
    lifecycle
        .start_model_request(None, 0, Some(successful_turn_id))
        .expect("observer should start a model request")
        .completed(None, ModelResponseType::Output);
    successful_turn.completed();

    let denied_turn = lifecycle
        .start_turn()
        .expect("observer should start a turn");
    let denied_turn_id = denied_turn.id();
    lifecycle
        .start_model_request(None, 0, Some(denied_turn_id))
        .expect("observer should start a model request")
        .failed(ModelErrorType::InvalidOutput, None);
    let mut completed_tool = lifecycle
        .request_tool(
            "completed-call".to_string(),
            "read".to_string(),
            Some(denied_turn_id),
        )
        .expect("observer should request a tool");
    completed_tool.started();
    completed_tool.completed();
    lifecycle
        .request_tool(
            "denied-call".to_string(),
            "write".to_string(),
            Some(denied_turn_id),
        )
        .expect("observer should request a denied tool")
        .denied();
    denied_turn.failed(TurnErrorType::ToolDenied);

    let failed_turn = lifecycle
        .start_turn()
        .expect("observer should start a turn");
    let failed_turn_id = failed_turn.id();
    let mut failed_tool = lifecycle
        .request_tool(
            "failed-call".to_string(),
            "read".to_string(),
            Some(failed_turn_id),
        )
        .expect("observer should request a tool");
    failed_tool.started();
    failed_tool.failed(ToolErrorType::Execution);
    failed_turn.failed(TurnErrorType::Tool);

    let cancelled_turn = lifecycle
        .start_turn()
        .expect("observer should start a turn");
    let cancelled_turn_id = cancelled_turn.id();
    drop(
        lifecycle
            .start_model_request(None, 0, Some(cancelled_turn_id))
            .expect("observer should start a model request"),
    );
    let mut cancelled_tool = lifecycle
        .request_tool(
            "cancelled-call".to_string(),
            "write".to_string(),
            Some(cancelled_turn_id),
        )
        .expect("observer should request a tool");
    cancelled_tool.started();
    drop(cancelled_tool);
    drop(cancelled_turn);

    let limited_turn = lifecycle
        .start_turn()
        .expect("observer should start a turn");
    let limited_turn_id = limited_turn.id();
    lifecycle
        .request_tool(
            "limited-call".to_string(),
            "read".to_string(),
            Some(limited_turn_id),
        )
        .expect("observer should request a limited tool")
        .failed(ToolErrorType::CallLimit);
    limited_turn.failed(TurnErrorType::ToolCallLimit);
}

fn assert_request_duration(metrics: &[&Metric]) {
    let duration = metric(metrics, DURATION_METRIC);
    assert!(matches!(
        duration.data(),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
            let point = histogram
                .data_points()
                .find(|point| {
                    point.attributes().any(|attribute| {
                        attribute.key.as_str() == "gen_ai.request.model"
                            && attribute.value.to_string() == "cancelled-unit-test"
                    })
                })
                .expect("cancelled request point should be exported");
            let mut attributes = point
                .attributes()
                .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            assert_eq!(point.count(), 1);
            assert_eq!(
                attributes,
                [
                    ("error.type", "cancelled".to_string()),
                    ("gen_ai.operation.name", "chat".to_string()),
                    ("gen_ai.provider.name", "test_provider".to_string()),
                    (
                        "gen_ai.request.model",
                        "cancelled-unit-test".to_string()
                    ),
                ]
            );

            true
        }
    ));
}

fn assert_agent_duration(metrics: &[&Metric]) {
    let agent_duration = metric(metrics, AGENT_DURATION_METRIC);
    assert_eq!(agent_duration.description(), AGENT_DURATION_DESCRIPTION);
    assert_eq!(agent_duration.unit(), DURATION_UNIT);
    assert!(matches!(
        agent_duration.data(),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 5);
            assert!(points.iter().all(|point| point.count() == 1));
            assert!(points.iter().all(|point| point.bounds().eq(
                AGENT_DURATION_BOUNDARIES_SECONDS
            )));
            let mut attributes = points
                .iter()
                .map(|point| attributes(point))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            assert_eq!(
                attributes,
                [
                    vec![],
                    vec![("error.type", "cancelled".to_string())],
                    vec![("error.type", "tool_call_limit".to_string())],
                    vec![("error.type", "tool_denied".to_string())],
                    vec![("error.type", "tool_execution_error".to_string())],
                ]
            );

            true
        }
    ));
}

fn assert_call_histogram(metric: &Metric, description: &str, expected_sum: u64, unit: &str) {
    assert_eq!(metric.description(), description);
    assert_eq!(metric.unit(), unit);
    assert!(matches!(
        metric.data(),
        AggregatedMetrics::U64(MetricData::Histogram(histogram)) if {
            let point = histogram
                .data_points()
                .next()
                .expect("call-count point should be exported");
            assert_eq!(point.count(), 5);
            assert_eq!(point.sum(), expected_sum);
            assert!(point.bounds().eq(AGENT_CALL_BOUNDARIES));
            assert_eq!(attributes(point), []);

            true
        }
    ));
}

fn assert_agent_calls(metrics: &[&Metric]) {
    let inference_calls = metric(metrics, AGENT_INFERENCE_CALLS_METRIC);
    assert_call_histogram(
        inference_calls,
        AGENT_INFERENCE_CALLS_DESCRIPTION,
        3,
        AGENT_INFERENCE_CALLS_UNIT,
    );
    let tool_calls = metric(metrics, AGENT_TOOL_CALLS_METRIC);
    assert_call_histogram(
        tool_calls,
        AGENT_TOOL_CALLS_DESCRIPTION,
        5,
        AGENT_TOOL_CALLS_UNIT,
    );
}

fn assert_tool_duration(metrics: &[&Metric]) {
    let tool_duration = metric(metrics, TOOL_DURATION_METRIC);
    assert_eq!(tool_duration.description(), TOOL_DURATION_DESCRIPTION);
    assert_eq!(tool_duration.unit(), DURATION_UNIT);
    assert!(matches!(
        tool_duration.data(),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 3);
            assert!(points.iter().all(|point| point.count() == 1));
            assert!(points.iter().all(|point| point.bounds().eq(
                DURATION_BOUNDARIES_SECONDS
            )));
            let mut attributes = points
                .iter()
                .map(|point| attributes(point))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            assert_eq!(
                attributes,
                [
                    vec![
                        ("error.type", "cancelled".to_string()),
                        ("gen_ai.tool.name", "write".to_string()),
                        ("gen_ai.tool.type", "function".to_string()),
                    ],
                    vec![
                        ("error.type", "tool_execution_error".to_string()),
                        ("gen_ai.tool.name", "read".to_string()),
                        ("gen_ai.tool.type", "function".to_string()),
                    ],
                    vec![
                        ("gen_ai.tool.name", "read".to_string()),
                        ("gen_ai.tool.type", "function".to_string()),
                    ],
                ]
            );

            true
        }
    ));
}

#[test]
fn records_model_agent_and_tool_metric_contracts() {
    // Arrange
    let exporter = InMemoryMetricExporter::default();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());
    let metadata = ModelMetadata::new("test_provider", "cancelled-unit-test")
        .expect("fixture metadata should be valid");
    let request_metrics = RequestMetrics::start(&metadata);
    let lifecycle = LifecycleEmitter::new(LifecycleMetrics::default());

    // Act
    drop(request_metrics);
    record_lifecycle_fixtures(&lifecycle);
    meter_provider.force_flush().expect("metrics should flush");

    // Assert
    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("metrics should be exported");
    let metrics = resource_metrics
        .iter()
        .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .collect::<Vec<_>>();
    assert_eq!(metrics.len(), 5);
    assert_eq!(
        TurnErrorType::Model(ModelErrorType::Request).as_str(),
        ERROR_REQUEST
    );
    assert_eq!(
        TurnErrorType::RepositoryRequired.as_str(),
        ERROR_REPOSITORY_REQUIRED
    );
    assert_request_duration(&metrics);
    assert_agent_duration(&metrics);
    assert_agent_calls(&metrics);
    assert_tool_duration(&metrics);
}
