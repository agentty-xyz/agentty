//! Integration coverage for the `ag-harness` request-duration metric.
#![cfg(test)]

use std::{future, io};

use ag_harness::{Model, ModelBackend, ModelError, ModelMetadata, ModelRequest, OutputSchema};
use async_trait::async_trait;
use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use serde_json::json;

#[derive(Clone, Copy)]
enum StubOutcome {
    Failure,
    Pending,
    Success,
}

struct StubBackend {
    model: &'static str,
    outcome: StubOutcome,
}

#[async_trait]
impl ModelBackend for StubBackend {
    fn metadata(&self) -> ModelMetadata<'_> {
        ModelMetadata::new("stub_provider", self.model)
    }

    async fn generate(&self, _request: &ModelRequest) -> Result<String, ModelError> {
        match self.outcome {
            StubOutcome::Failure => Err(ModelError::request(io::Error::other("offline"))),
            StubOutcome::Pending => future::pending().await,
            StubOutcome::Success => Ok(r#"{"name":"Ada"}"#.to_string()),
        }
    }
}

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
    StubBackend {
        model: "before-installation",
        outcome: StubOutcome::Success,
    }
    .complete(ModelRequest::new("before provider installation", schema()))
    .await
    .expect("request before provider installation should succeed");
    let exporter = InMemoryMetricExporter::default();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());

    // Act
    StubBackend {
        model: "successful",
        outcome: StubOutcome::Success,
    }
    .complete(ModelRequest::new("after provider installation", schema()))
    .await
    .expect("instrumented request should succeed");
    let failure = StubBackend {
        model: "failed",
        outcome: StubOutcome::Failure,
    }
    .complete(ModelRequest::new("failing request", schema()))
    .await
    .expect_err("instrumented provider failure should be returned");
    let pending_request = tokio::spawn(
        StubBackend {
            model: "cancelled",
            outcome: StubOutcome::Pending,
        }
        .complete(ModelRequest::new("cancelled request", schema())),
    );
    tokio::task::yield_now().await;
    pending_request.abort();
    let cancellation = pending_request
        .await
        .expect_err("aborted request should be cancelled");
    meter_provider
        .force_flush()
        .expect("duration metric should flush");

    // Assert
    assert!(matches!(failure, ModelError::Request(_)));
    assert!(cancellation.is_cancelled());
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
            assert_eq!(points.len(), 3);
            assert!(points.iter().all(|point| point.count() == 1));
            let mut attributes = points
                .iter()
                .map(|point| metric_attributes(point))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            assert_eq!(
                attributes,
                [
                    vec![
                        ("gen_ai.provider.name", "stub_provider".to_string()),
                        ("gen_ai.request.model", "cancelled".to_string()),
                    ],
                    vec![
                        ("gen_ai.provider.name", "stub_provider".to_string()),
                        ("gen_ai.request.model", "failed".to_string()),
                    ],
                    vec![
                        ("gen_ai.provider.name", "stub_provider".to_string()),
                        ("gen_ai.request.model", "successful".to_string()),
                    ],
                ]
            );

            true
        }
    ));
}
