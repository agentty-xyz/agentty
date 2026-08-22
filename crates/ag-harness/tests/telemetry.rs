//! Integration coverage for the `ag-harness` model-client metrics.
#![cfg(test)]

mod support;

use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig};
use opentelemetry_proto::tonic::common::v1::{KeyValue as ProtoKeyValue, any_value};
use opentelemetry_proto::tonic::metrics::v1::{
    AggregationTemporality, Histogram as ProtoHistogram, HistogramDataPoint as ProtoHistogramPoint,
    Metric as ProtoMetric, metric,
};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::data::{
    AggregatedMetrics, HistogramDataPoint, Metric as SdkMetric, MetricData,
};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider, Temporality,
};
use serde_json::json;
use support::otlp::{OtlpCollector, OtlpPayload, OtlpRequest};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const OTLP_CONTRACT_FIXTURE_ENV: &str = "AG_HARNESS_RUN_OTLP_CONTRACT_FIXTURE";
const OTLP_CONTRACT_TEST: &str = "exports_otlp_metric_contract_and_flushes_on_shutdown";

static TELEMETRY_TEST_LOCK: Mutex<()> = Mutex::const_new(());

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
            "message": {"content": r#"{"name":"sensitive-response-content-sentinel"}"#}
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

fn assert_duration_metric(metrics: &[&SdkMetric]) {
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

fn assert_token_usage_metric(metrics: &[&SdkMetric]) {
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

fn proto_string_attributes(attributes: &[ProtoKeyValue]) -> Vec<(&str, &str)> {
    let mut attributes = attributes
        .iter()
        .map(|attribute| {
            let value = attribute
                .value
                .as_ref()
                .and_then(|value| value.value.as_ref())
                .expect("OTLP attribute should have a value");
            let any_value::Value::StringValue(value) = value else {
                unreachable!("OTLP contract attributes should be strings");
            };

            (attribute.key.as_str(), value.as_str())
        })
        .collect::<Vec<_>>();
    attributes.sort_unstable();

    attributes
}

fn proto_histogram(metric: &ProtoMetric) -> &ProtoHistogram {
    let Some(metric::Data::Histogram(histogram)) = metric.data.as_ref() else {
        unreachable!("GenAI metric should export as a histogram");
    };

    histogram
}

fn assert_histogram_point(point: &ProtoHistogramPoint, boundaries: &[f64]) {
    assert_eq!(point.count, 1);
    assert_eq!(point.explicit_bounds, boundaries);
    assert_eq!(point.bucket_counts.len(), boundaries.len() + 1);
    assert_eq!(point.bucket_counts.iter().sum::<u64>(), point.count);
    assert!(point.start_time_unix_nano > 0);
    assert!(point.time_unix_nano >= point.start_time_unix_nano);
}

fn assert_otlp_duration(metric: &ProtoMetric, expected_models: &[&str]) {
    assert_eq!(metric.name, "gen_ai.client.operation.duration");
    assert_eq!(metric.description, "GenAI operation duration.");
    assert_eq!(metric.unit, "s");
    assert_eq!(metric.metadata.len(), 0);
    let histogram = proto_histogram(metric);
    assert_eq!(
        histogram.aggregation_temporality,
        AggregationTemporality::Cumulative as i32
    );
    assert_eq!(histogram.data_points.len(), expected_models.len());
    let mut attributes = histogram
        .data_points
        .iter()
        .map(|point| proto_string_attributes(&point.attributes))
        .collect::<Vec<_>>();
    attributes.sort_unstable();
    let mut expected_attributes = expected_models
        .iter()
        .map(|model| {
            vec![
                ("gen_ai.operation.name", "chat"),
                ("gen_ai.provider.name", "alibaba_cloud"),
                ("gen_ai.request.model", *model),
            ]
        })
        .collect::<Vec<_>>();
    expected_attributes.sort_unstable();
    assert_eq!(
        attributes, expected_attributes,
        "duration points should have the expected model identities"
    );
    for point in &histogram.data_points {
        assert_histogram_point(
            point,
            &[
                0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96,
                81.92,
            ],
        );
        assert!(point.sum.is_some_and(|sum| sum >= 0.0));
    }
}

fn assert_otlp_token_usage(metric: &ProtoMetric, expected_models: &[&str]) {
    assert_eq!(metric.name, "gen_ai.client.token.usage");
    assert_eq!(
        metric.description,
        "Number of input and output tokens used."
    );
    assert_eq!(metric.unit, "{token}");
    assert_eq!(metric.metadata.len(), 0);
    let histogram = proto_histogram(metric);
    assert_eq!(
        histogram.aggregation_temporality,
        AggregationTemporality::Cumulative as i32
    );
    assert_eq!(histogram.data_points.len(), expected_models.len() * 2);
    let mut points = histogram
        .data_points
        .iter()
        .map(|point| (proto_string_attributes(&point.attributes), point))
        .collect::<Vec<_>>();
    points.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut expected_points = expected_models
        .iter()
        .flat_map(|model| {
            [
                (
                    vec![
                        ("gen_ai.operation.name", "chat"),
                        ("gen_ai.provider.name", "alibaba_cloud"),
                        ("gen_ai.request.model", *model),
                        ("gen_ai.token.type", "input"),
                    ],
                    Some(23.0),
                ),
                (
                    vec![
                        ("gen_ai.operation.name", "chat"),
                        ("gen_ai.provider.name", "alibaba_cloud"),
                        ("gen_ai.request.model", *model),
                        ("gen_ai.token.type", "output"),
                    ],
                    Some(7.0),
                ),
            ]
        })
        .collect::<Vec<_>>();
    expected_points.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        points
            .iter()
            .map(|(attributes, point)| (attributes.clone(), point.sum))
            .collect::<Vec<_>>(),
        expected_points
    );
    for (_, point) in points {
        assert_histogram_point(
            point,
            &[
                1.0,
                4.0,
                16.0,
                64.0,
                256.0,
                1_024.0,
                4_096.0,
                16_384.0,
                65_536.0,
                262_144.0,
                1_048_576.0,
                4_194_304.0,
                16_777_216.0,
                67_108_864.0,
            ],
        );
    }
}

fn assert_otlp_metric_request(request: &OtlpRequest, expected_models: &[&str]) {
    request.assert_protobuf();
    assert_eq!(request.resource_count, 1);
    let OtlpPayload::Metrics(payload) = &request.payload else {
        unreachable!("metric exporter should only send metric payloads");
    };
    assert_eq!(payload.resource_metrics.len(), 1);
    let resource_metrics = &payload.resource_metrics[0];
    assert_eq!(resource_metrics.schema_url, "");
    let resource = resource_metrics
        .resource
        .as_ref()
        .expect("OTLP resource should be present");
    assert_eq!(
        proto_string_attributes(&resource.attributes),
        [
            ("deployment.environment.name", "test"),
            ("service.name", "ag-harness-otlp-contract-test"),
        ]
    );
    assert_eq!(resource.dropped_attributes_count, 0);
    assert_eq!(resource.entity_refs.len(), 0);
    assert_eq!(resource_metrics.scope_metrics.len(), 1);
    let scope_metrics = &resource_metrics.scope_metrics[0];
    assert_eq!(scope_metrics.schema_url, "");
    let scope = scope_metrics
        .scope
        .as_ref()
        .expect("OTLP instrumentation scope should be present");
    assert_eq!(scope.name, "ag-harness");
    assert_eq!(scope.version, "");
    assert_eq!(scope.attributes.len(), 0);
    assert_eq!(scope.dropped_attributes_count, 0);
    assert_eq!(scope_metrics.metrics.len(), 2);
    let mut metrics = scope_metrics.metrics.iter().collect::<Vec<_>>();
    metrics.sort_unstable_by_key(|metric| metric.name.as_str());
    assert_otlp_duration(metrics[0], expected_models);
    assert_otlp_token_usage(metrics[1], expected_models);
}

fn assert_secrets_absent(request: &OtlpRequest) {
    for secret in [
        "test-key",
        "sensitive-prompt-sentinel",
        "sensitive-response-content-sentinel",
        "sensitive-response-id",
        "sensitive-system-fingerprint",
        "shutdown-sensitive-prompt",
    ] {
        assert!(
            !request
                .body
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "OTLP payload should not contain fixture secret {secret:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn records_client_metrics_after_late_provider_installation_for_all_outcomes() {
    // Arrange
    let _guard = TELEMETRY_TEST_LOCK.lock().await;
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

async fn run_otlp_metric_contract_fixture() {
    // Arrange
    let _guard = TELEMETRY_TEST_LOCK.lock().await;
    let collector = OtlpCollector::start().await;
    let exporter = MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(collector.metrics_endpoint())
        .with_temporality(Temporality::Cumulative)
        .build()
        .expect("OTLP metric exporter should build");
    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_hours(1))
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(
            Resource::builder_empty()
                .with_service_name("ag-harness-otlp-contract-test")
                .with_attribute(KeyValue::new("deployment.environment.name", "test"))
                .build(),
        )
        .build();
    global::set_meter_provider(meter_provider.clone());
    let server = MockServer::start().await;
    mount_success_response(&server, Some("sensitive-response-model"), true).await;

    // Act
    client(&server, "otlp-contract-model")
        .complete(request("sensitive-prompt-sentinel"))
        .await
        .expect("instrumented request should succeed");
    meter_provider
        .force_flush()
        .expect("OTLP metric payload should force flush");

    // Assert
    let flushed_requests = collector
        .requests()
        .await
        .expect("flushed OTLP requests should decode");
    assert_eq!(flushed_requests.len(), 1);
    assert_otlp_metric_request(&flushed_requests[0], &["otlp-contract-model"]);
    assert_secrets_absent(&flushed_requests[0]);

    // Arrange
    let shutdown_server = MockServer::start().await;
    mount_success_response(&shutdown_server, None, true).await;

    // Act
    client(&shutdown_server, "shutdown-contract-model")
        .complete(request("shutdown-sensitive-prompt"))
        .await
        .expect("request before telemetry shutdown should succeed");
    meter_provider
        .shutdown()
        .expect("OTLP metric provider should shut down");

    // Assert
    let shutdown_requests = collector
        .requests()
        .await
        .expect("shutdown OTLP requests should decode");
    assert_eq!(shutdown_requests.len(), 2);
    assert_otlp_metric_request(&shutdown_requests[0], &["otlp-contract-model"]);
    assert_otlp_metric_request(
        &shutdown_requests[1],
        &["otlp-contract-model", "shutdown-contract-model"],
    );
    assert_secrets_absent(&shutdown_requests[0]);
    assert_secrets_absent(&shutdown_requests[1]);

    // Arrange
    let post_shutdown_server = MockServer::start().await;
    mount_success_response(&post_shutdown_server, None, true).await;

    // Act
    client(&post_shutdown_server, "post-shutdown-model")
        .complete(request("post-shutdown-sensitive-prompt"))
        .await
        .expect("request after telemetry shutdown should succeed");

    // Assert
    assert_eq!(
        collector
            .requests()
            .await
            .expect("post-shutdown OTLP requests should decode")
            .len(),
        2
    );
}

async fn spawn_otlp_metric_contract_fixture() -> Output {
    let executable = std::env::current_exe().expect("test executable should be available");

    Command::new(executable)
        .arg(OTLP_CONTRACT_TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(OTLP_CONTRACT_FIXTURE_ENV, "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT")
        .env_remove("OTEL_EXPORTER_OTLP_TIMEOUT")
        .env_remove("OTEL_METRIC_EXPORT_INTERVAL")
        .env("OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE", "delta")
        .output()
        .await
        .expect("OTLP contract fixture process should run")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_otlp_metric_contract_and_flushes_on_shutdown() {
    if std::env::var_os(OTLP_CONTRACT_FIXTURE_ENV).is_some() {
        run_otlp_metric_contract_fixture().await;

        return;
    }

    // Arrange & Act
    let output = spawn_otlp_metric_contract_fixture().await;

    // Assert
    assert!(
        output.status.success(),
        "OTLP contract fixture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
