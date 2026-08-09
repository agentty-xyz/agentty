//! Integration coverage for the `ag-harness` request-duration metric.
#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use serde_json::json;
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct PendingResponse {
    started: Arc<Notify>,
}

impl Respond for PendingResponse {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.started.notify_one();

        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(10))
            .set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            }))
    }
}

fn client(server: &MockServer, model: &str) -> ag_harness::ModelClient {
    ag_harness::ModelClient::qwen(ag_harness::QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: model.to_string(),
    })
    .expect("fixture configuration should be valid")
}

fn request(prompt: &str) -> ag_harness::ModelRequest {
    ag_harness::ModelRequest::new(
        prompt,
        ag_harness::OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        }))
        .expect("fixture schema should be valid"),
    )
}

async fn mount_success_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
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

async fn wait_for_provider(
    started: &Notify,
    request_task: &mut tokio::task::JoinHandle<
        Result<ag_harness::ModelResponse, ag_harness::ModelError>,
    >,
) -> Result<(), String> {
    tokio::select! {
        () = started.notified() => Ok(()),
        result = request_task => {
            Err(format!("pending request completed before cancellation: {result:?}"))
        }
        () = tokio::time::sleep(Duration::from_secs(5)) => {
            Err("pending request did not reach the provider before the timeout".to_string())
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn records_request_duration_after_late_provider_installation_for_all_outcomes() {
    // Arrange
    let unconfigured_server = MockServer::start().await;
    mount_success_response(&unconfigured_server).await;

    // Act
    client(&unconfigured_server, "before-installation")
        .complete(request("request before telemetry installation"))
        .await
        .expect("request before telemetry installation should succeed");

    // Arrange
    let exporter = InMemoryMetricExporter::default();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());
    let success_server = MockServer::start().await;
    mount_success_response(&success_server).await;
    let failure_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("offline"))
        .expect(1)
        .mount(&failure_server)
        .await;
    let pending_server = MockServer::start().await;
    let pending_started = Arc::new(Notify::new());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(PendingResponse {
            started: Arc::clone(&pending_started),
        })
        .expect(1)
        .mount(&pending_server)
        .await;

    // Act
    client(&success_server, "successful")
        .complete(request("successful request"))
        .await
        .expect("instrumented request should succeed");
    let failure = client(&failure_server, "failed")
        .complete(request("failing request"))
        .await
        .expect_err("instrumented provider failure should be returned");
    let pending_client = client(&pending_server, "cancelled");
    let mut pending_request =
        tokio::spawn(async move { pending_client.complete(request("cancelled request")).await });
    wait_for_provider(&pending_started, &mut pending_request)
        .await
        .expect("pending request should reach the provider");
    pending_request.abort();
    let cancellation = pending_request
        .await
        .expect_err("aborted request should be cancelled");
    meter_provider
        .force_flush()
        .expect("duration metric should flush");

    // Assert
    assert!(matches!(failure, ag_harness::ModelError::Request(_)));
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
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "cancelled".to_string()),
                    ],
                    vec![
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "failed".to_string()),
                    ],
                    vec![
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "successful".to_string()),
                    ],
                ]
            );

            true
        }
    ));
}
