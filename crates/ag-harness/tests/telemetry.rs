//! Integration coverage for the `ag-harness` request-duration metric.
#![cfg(test)]

use ag_harness::{Model, ModelRequest, OutputSchema, Qwen, QwenConfig};
use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "required": ["name"],
        "additionalProperties": false
    }))
    .expect("fixture schema should be valid")
}

fn metric_attributes(
    point: &opentelemetry_sdk::metrics::data::HistogramDataPoint<f64>,
) -> Vec<(&str, String)> {
    let mut attributes = point
        .attributes()
        .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
        .collect::<Vec<_>>();
    attributes.sort_unstable();

    attributes
}

#[tokio::test(flavor = "current_thread")]
async fn records_only_request_duration_after_provider_installation() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let model = Qwen::new(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: "qwen-plus".to_string(),
    });
    model
        .complete(ModelRequest::new("before provider installation", schema()))
        .await
        .expect("request before provider installation should succeed");
    let exporter = InMemoryMetricExporter::default();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());

    // Act
    model
        .complete(ModelRequest::new("after provider installation", schema()))
        .await
        .expect("instrumented request should succeed");
    meter_provider
        .force_flush()
        .expect("duration metric should flush");

    // Assert
    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("duration metric should be exported");
    let metrics = resource_metrics
        .iter()
        .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .collect::<Vec<_>>();
    assert_eq!(metrics.len(), 1);
    let metric = metrics.first().expect("one metric should be exported");
    assert_eq!(metric.name(), "gen_ai.client.operation.duration");
    assert!(matches!(
        metric.data(),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 1);
            assert_eq!(points[0].count(), 1);
            assert_eq!(
                metric_attributes(points[0]),
                [
                    ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                    ("gen_ai.request.model", "qwen-plus".to_string()),
                ]
            );

            true
        }
    ));
}
