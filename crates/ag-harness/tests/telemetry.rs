//! Integration coverage for the `ag-harness` model-client metrics.
#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, HistogramDataPoint, Metric, MetricData};
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

async fn mount_success_response(server: &MockServer, response_model: Option<&str>, usage: bool) {
    let mut response = json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": r#"{"name":"Ada"}"#}
        }],
        "id": "sensitive-response-id",
        "system_fingerprint": "sensitive-system-fingerprint"
    });
    if let Some(response_model) = response_model {
        response["model"] = json!(response_model);
    }
    if usage {
        response["usage"] = json!({
            "completion_tokens": 7,
            "prompt_tokens": 23,
            "total_tokens": 30
        });
    }

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_invalid_output_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"unexpected":true}"#}
            }],
            "model": "invalid-output-response",
            "usage": {
                "completion_tokens": 3,
                "prompt_tokens": 11,
                "total_tokens": 14
            }
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_incomplete_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": r#"{"name":"Ada"}"#}
            }],
            "usage": {
                "completion_tokens": 5,
                "prompt_tokens": 13,
                "total_tokens": 18
            }
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn metric_attributes<T>(point: &HistogramDataPoint<T>) -> Vec<(&str, String)> {
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

fn assert_duration_metric(metrics: &[&Metric]) {
    let duration = metrics
        .iter()
        .find(|metric| metric.name() == "gen_ai.client.operation.duration")
        .expect("duration metric should be exported");
    assert_eq!(duration.description(), "GenAI operation duration.");
    assert_eq!(duration.unit(), "s");
    assert!(matches!(
        duration.data(),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 6);
            assert!(points.iter().all(|point| point.count() == 1));
            assert!(points.iter().all(|point| point.bounds().eq([
                0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24,
                20.48, 40.96, 81.92,
            ])));
            let mut attributes = points
                .iter()
                .map(|point| metric_attributes(point))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            assert_eq!(
                attributes,
                [
                    vec![
                        ("error.type", "503".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "failed".to_string()),
                    ],
                    vec![
                        ("error.type", "cancelled".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "cancelled".to_string()),
                    ],
                    vec![
                        ("error.type", "invalid_output".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "invalid-output".to_string()),
                    ],
                    vec![
                        ("error.type", "invalid_response".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "incomplete".to_string()),
                    ],
                    vec![
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "missing-usage".to_string()),
                    ],
                    vec![
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "qwen-plus".to_string()),
                    ],
                ]
            );

            true
        }
    ));
}

fn assert_token_usage_metric(metrics: &[&Metric]) {
    let token_usage = metrics
        .iter()
        .find(|metric| metric.name() == "gen_ai.client.token.usage")
        .expect("token-usage metric should be exported");
    assert_eq!(
        token_usage.description(),
        "Number of input and output tokens used."
    );
    assert_eq!(token_usage.unit(), "{token}");
    assert!(matches!(
        token_usage.data(),
        AggregatedMetrics::U64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 6);
            assert!(points.iter().all(|point| point.count() == 1));
            assert!(points.iter().all(|point| point.bounds().eq([
                1.0, 4.0, 16.0, 64.0, 256.0, 1_024.0, 4_096.0, 16_384.0, 65_536.0,
                262_144.0, 1_048_576.0, 4_194_304.0, 16_777_216.0, 67_108_864.0,
            ])));
            let mut points = points
                .iter()
                .map(|point| (metric_attributes(point), point.sum()))
                .collect::<Vec<_>>();
            points.sort_unstable();
            assert_eq!(
                points,
                [
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "incomplete".to_string()),
                            ("gen_ai.token.type", "input".to_string()),
                        ],
                        13,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "incomplete".to_string()),
                            ("gen_ai.token.type", "output".to_string()),
                        ],
                        5,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "invalid-output".to_string()),
                            ("gen_ai.token.type", "input".to_string()),
                        ],
                        11,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "invalid-output".to_string()),
                            ("gen_ai.token.type", "output".to_string()),
                        ],
                        3,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "qwen-plus".to_string()),
                            ("gen_ai.token.type", "input".to_string()),
                        ],
                        23,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "qwen-plus".to_string()),
                            ("gen_ai.token.type", "output".to_string()),
                        ],
                        7,
                    ),
                ]
            );

            true
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn records_client_metrics_after_late_provider_installation_for_all_outcomes() {
    // Arrange
    let unconfigured_server = MockServer::start().await;
    mount_success_response(&unconfigured_server, None, true).await;

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
    mount_success_response(&success_server, Some("qwen-plus-2026-08-16"), true).await;
    let missing_usage_server = MockServer::start().await;
    mount_success_response(&missing_usage_server, None, false).await;
    let invalid_output_server = MockServer::start().await;
    mount_invalid_output_response(&invalid_output_server).await;
    let incomplete_server = MockServer::start().await;
    mount_incomplete_response(&incomplete_server).await;
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
    client(&success_server, "qwen-plus")
        .complete(request("successful request"))
        .await
        .expect("instrumented request should succeed");
    client(&missing_usage_server, "missing-usage")
        .complete(request("request without provider usage"))
        .await
        .expect("instrumented request without usage should succeed");
    let invalid_output = client(&invalid_output_server, "invalid-output")
        .complete(request("request with invalid output"))
        .await
        .expect_err("invalid provider output should be returned");
    let incomplete = client(&incomplete_server, "incomplete")
        .complete(request("incomplete request"))
        .await
        .expect_err("incomplete provider response should be returned");
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
        .expect("client metrics should flush");

    // Assert
    assert!(matches!(failure, ag_harness::ModelError::Request(_)));
    assert!(matches!(
        invalid_output,
        ag_harness::ModelError::SchemaViolation { .. }
    ));
    assert!(matches!(
        incomplete,
        ag_harness::ModelError::IncompleteResponse { .. }
    ));
    assert!(cancellation.is_cancelled());
    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("client metrics should be exported");
    let metrics = resource_metrics
        .iter()
        .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .collect::<Vec<_>>();
    assert_eq!(metrics.len(), 2);
    assert_duration_metric(&metrics);
    assert_token_usage_metric(&metrics);
}
