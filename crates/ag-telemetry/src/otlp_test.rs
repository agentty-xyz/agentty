//! OTLP export tests for initialization, endpoint handling, trace, metric,
//! and event log export, and diagnostics.

use std::future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry::global;
use opentelemetry::logs::Severity;
use opentelemetry::trace::{Span as _, Tracer as _, TracerProvider as _};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::SeverityNumber;
use opentelemetry_proto::tonic::metrics::v1::metric;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{LogBatch, LogExporter as SdkLogExporter};
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::trace::{SpanData, SpanExporter as SdkSpanExporter};
use prost::Message as _;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt as _;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{CountedExporter, Diagnostics, OtlpError, OtlpExport, Service};

const SERVICE: Service = Service {
    name: "otlp-test",
    user_id: None,
    version: "1.2.3",
};

#[tokio::test]
async fn initialization_worker_failure_returns_a_bounded_error() {
    // Arrange / Act
    let result = OtlpExport::initialize(|| {
        std::panic::resume_unwind(Box::new("private initialization detail"))
    })
    .await;

    // Assert
    assert_eq!(
        result.err().expect("worker failure"),
        OtlpError::Initialization
    );
}

#[test]
fn exporter_configuration_failure_does_not_echo_the_endpoint() {
    // Arrange / Act
    let error = OtlpExport::build("http://host/private invalid endpoint", None, None, SERVICE)
        .err()
        .expect("invalid exporter configuration");

    // Assert
    assert_eq!(error, OtlpError::Configuration);
    assert!(!error.to_string().contains("private"));
}

#[tokio::test]
async fn absent_endpoint_never_constructs_an_exporter() {
    // Arrange / Act
    let export = OtlpExport::start(None, SERVICE)
        .await
        .expect("disabled startup");

    // Assert
    assert!(export.is_none());
}

#[tokio::test]
async fn invalid_endpoints_fail_without_echoing_credentials() {
    // Arrange
    let endpoints = [
        "",
        "localhost:4318",
        "http://localhost:4318",
        "http://localhost:4318/",
        "file:///private",
        "ftp://host/traces",
        "http://user:secret@host/traces",
        "https://host/traces#secret",
    ];

    // Act / Assert
    assert_eq!(
        OtlpExport::validate("http://localhost:4318/v1/traces"),
        Ok(())
    );
    for endpoint in endpoints {
        assert_eq!(OtlpExport::validate(endpoint), Err(OtlpError::Endpoint));
        let error = OtlpExport::start(Some(endpoint), SERVICE)
            .await
            .err()
            .expect("invalid endpoint");
        assert_eq!(error, OtlpError::Endpoint);
        assert!(error.to_string().contains("--otlp-endpoint"));
        assert!(!error.to_string().contains("secret"));
    }
}

#[tokio::test]
async fn shutdown_exports_protobuf_with_the_service_identity() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/custom/traces"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let endpoint = format!("{}/custom/traces", server.uri());
    let export = OtlpExport::start(Some(&endpoint), SERVICE)
        .await
        .expect("start")
        .expect("enabled");

    // Act
    let mut span = export.provider.tracer("export-test").start("session.turn");
    span.end();
    let warnings = export.shutdown().await;

    // Assert
    assert_eq!(warnings, 0);
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let payload =
        ExportTraceServiceRequest::decode(requests[0].body.as_slice()).expect("OTLP protobuf");
    assert_eq!(
        payload.resource_spans[0].scope_spans[0].spans[0].name,
        "session.turn"
    );
    let resource = payload.resource_spans[0]
        .resource
        .as_ref()
        .expect("resource");
    let attribute = |key: &str| {
        resource
            .attributes
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| attribute.value.as_ref())
            .and_then(|value| value.value.clone())
    };
    assert_eq!(
        attribute("service.name"),
        Some(any_value::Value::StringValue("otlp-test".to_string()))
    );
    assert_eq!(
        attribute("service.version"),
        Some(any_value::Value::StringValue("1.2.3".to_string()))
    );
    assert!(attribute("service.instance.id").is_some());
    assert_eq!(attribute("user.id"), None);
}

#[tokio::test]
async fn repeated_install_keeps_counting_failed_exports() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let endpoint = format!("{}/v1/traces", server.uri());
    let export = OtlpExport::start(Some(&endpoint), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    export.install();
    export.install();

    // Act
    let mut span = export
        .provider
        .tracer("failure-test")
        .start("completed-work");
    span.end();
    let warnings = export.shutdown().await;

    // Assert
    assert!(warnings > 0);
    assert!(
        !server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn rejected_exports_are_counted_once_per_export_sharing_the_subscriber() {
    // The first installed export binds the process-wide diagnostic subscriber;
    // the second shares the process but not the subscriber's counter. Each
    // must report its one rejected batch exactly once.
    for rejected in ["traces", "logs"] {
        // Arrange
        let server = MockServer::start().await;
        for signal in ["traces", "logs"] {
            // A non-retryable rejection keeps one request per signal.
            let status = if signal == rejected { 400 } else { 200 };
            Mock::given(method("POST"))
                .and(path(format!("/v1/{signal}")))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
        }
        let endpoint = format!("{}/v1/traces", server.uri());
        let export = OtlpExport::start_with_events(Some(&endpoint), SERVICE)
            .await
            .expect("start")
            .expect("enabled");
        let event_log = export.event_log().expect("event log");
        export.install();

        // Act
        event_log.emit("agentty_turn_start", &[]);
        let mut span = export.provider.tracer("failure-test").start("session.turn");
        span.end();
        let warnings = export.shutdown().await;

        // Assert
        assert_eq!(warnings, 1, "rejected {rejected} export");
    }
}

#[tokio::test]
async fn repeated_shutdown_reports_a_bounded_diagnostic() {
    // Arrange
    let export = OtlpExport::start(Some("http://localhost:4318/v1/traces"), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    export.provider.shutdown().expect("first shutdown");

    // Act
    let shutdown_warnings = export.shutdown().await;

    // Assert
    assert_eq!(shutdown_warnings, 1);
}

#[tokio::test]
async fn unicode_endpoint_paths_are_normalized_before_exporter_configuration() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let endpoint = format!("{}/\u{00e4}", server.uri());
    let export = OtlpExport::start(Some(&endpoint), SERVICE)
        .await
        .expect("start")
        .expect("enabled");

    // Act
    let mut span = export.provider.tracer("unicode-test").start("configured");
    span.end();
    let warnings = export.shutdown().await;

    // Assert
    assert_eq!(warnings, 0);
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/%C3%A4");
}

#[tokio::test]
async fn event_log_exports_records_beside_traces_with_one_resource_identity() {
    // Arrange
    let server = MockServer::start().await;
    for signal in ["traces", "logs"] {
        Mock::given(method("POST"))
            .and(path(format!("/otlp/v1/{signal}")))
            .and(query_param("tenant", "local"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    let endpoint = format!("{}/otlp/v1/traces?tenant=local", server.uri());
    let service = Service {
        user_id: Some("installation-id".to_string()),
        ..SERVICE
    };
    let export = OtlpExport::start_with_events(Some(&endpoint), service)
        .await
        .expect("start")
        .expect("enabled");
    let event_log = export.event_log().expect("event log");

    // Act
    event_log.emit(
        "agentty_turn_start",
        &[("agent", "codex"), ("model", "gpt-5.5")],
    );
    let mut span = export.provider.tracer("event-test").start("session.turn");
    span.end();
    let warnings = export.shutdown().await;

    // Assert
    assert_eq!(warnings, 0);
    let requests = server.received_requests().await.expect("requests");
    let body = |signal: &str| {
        requests
            .iter()
            .find(|request| request.url.path().ends_with(signal))
            .expect("signal request")
            .body
            .as_slice()
    };
    let logs = ExportLogsServiceRequest::decode(body("/v1/logs")).expect("OTLP logs");
    let traces = ExportTraceServiceRequest::decode(body("/v1/traces")).expect("OTLP traces");
    let record = &logs.resource_logs[0].scope_logs[0].log_records[0];
    let string = |value: &str| Some(any_value::Value::StringValue(value.to_string()));
    let attribute = |attributes: &[KeyValue], key: &str| {
        attributes
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| attribute.value.as_ref())
            .and_then(|value| value.value.clone())
    };
    assert_eq!(record.event_name, "agentty_turn_start");
    assert_eq!(
        record.body.as_ref().and_then(|body| body.value.clone()),
        string("agentty_turn_start")
    );
    assert_eq!(record.severity_number, SeverityNumber::Info as i32);
    assert_eq!(record.severity_text, "INFO");
    assert!(record.time_unix_nano > 0);
    assert_eq!(attribute(&record.attributes, "agent"), string("codex"));
    assert_eq!(attribute(&record.attributes, "model"), string("gpt-5.5"));
    assert_eq!(record.attributes.len(), 2);
    let log_resource = &logs.resource_logs[0]
        .resource
        .as_ref()
        .expect("log resource")
        .attributes;
    let trace_resource = &traces.resource_spans[0]
        .resource
        .as_ref()
        .expect("trace resource")
        .attributes;
    assert_eq!(attribute(log_resource, "service.name"), string("otlp-test"));
    assert!(attribute(log_resource, "service.instance.id").is_some());
    assert_eq!(
        attribute(log_resource, "service.instance.id"),
        attribute(trace_resource, "service.instance.id")
    );
    assert_eq!(
        attribute(log_resource, "user.id"),
        string("installation-id")
    );
    assert_eq!(
        attribute(trace_resource, "user.id"),
        string("installation-id")
    );
}

#[tokio::test]
async fn installed_meter_exports_histograms_beside_traces_with_one_resource_identity() {
    // Arrange
    let server = MockServer::start().await;
    for signal in ["traces", "metrics"] {
        Mock::given(method("POST"))
            .and(path(format!("/otlp/v1/{signal}")))
            .and(query_param("tenant", "local"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    let endpoint = format!("{}/otlp/v1/traces?tenant=local", server.uri());
    let export = OtlpExport::start(Some(&endpoint), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    export.install();

    // Act
    global::meter("metric-test")
        .f64_histogram("gen_ai.client.operation.duration")
        .with_unit("s")
        .build()
        .record(0.25, &[]);
    let mut span = export.provider.tracer("metric-test").start("session.turn");
    span.end();
    let warnings = export.shutdown().await;

    // Assert
    assert_eq!(warnings, 0);
    let requests = server.received_requests().await.expect("requests");
    let body = |signal: &str| {
        requests
            .iter()
            .find(|request| request.url.path().ends_with(signal))
            .expect("signal request")
            .body
            .as_slice()
    };
    let metrics = ExportMetricsServiceRequest::decode(body("/v1/metrics")).expect("OTLP metrics");
    let traces = ExportTraceServiceRequest::decode(body("/v1/traces")).expect("OTLP traces");
    let exported = &metrics.resource_metrics[0].scope_metrics[0].metrics[0];
    assert_eq!(exported.name, "gen_ai.client.operation.duration");
    assert_eq!(exported.unit, "s");
    assert!(matches!(
        &exported.data,
        Some(metric::Data::Histogram(histogram))
            if histogram.data_points[0].count == 1
    ));
    let attribute = |attributes: &[KeyValue], key: &str| {
        attributes
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| attribute.value.as_ref())
            .and_then(|value| value.value.clone())
    };
    let metric_resource = &metrics.resource_metrics[0]
        .resource
        .as_ref()
        .expect("metric resource")
        .attributes;
    let trace_resource = &traces.resource_spans[0]
        .resource
        .as_ref()
        .expect("trace resource")
        .attributes;
    assert_eq!(
        attribute(metric_resource, "service.name"),
        Some(any_value::Value::StringValue("otlp-test".to_string()))
    );
    assert!(attribute(metric_resource, "service.instance.id").is_some());
    assert_eq!(
        attribute(metric_resource, "service.instance.id"),
        attribute(trace_resource, "service.instance.id")
    );
}

#[tokio::test]
async fn sibling_signals_require_a_standard_traces_path_and_events_require_opt_in() {
    // Arrange
    let standard = "http://localhost:4318/v1/traces";
    let custom = "http://localhost:4318/custom/traces";

    // Act
    let traces_only = OtlpExport::start(Some(standard), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    let custom_path = OtlpExport::start_with_events(Some(custom), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    let disabled = OtlpExport::start_with_events(None, SERVICE)
        .await
        .expect("disabled startup");

    // Assert
    assert!(traces_only.event_log().is_none());
    assert!(traces_only.metrics.is_some());
    assert!(custom_path.event_log().is_none());
    assert!(custom_path.metrics.is_none());
    assert!(disabled.is_none());
    assert_eq!(traces_only.shutdown().await, 0);
    assert_eq!(custom_path.shutdown().await, 0);
}

#[tokio::test]
async fn custom_traces_path_installs_no_meter_provider() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let endpoint = format!("{}/custom/traces", server.uri());
    let export = OtlpExport::start(Some(&endpoint), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    export.install();

    // Act
    global::meter("metric-test")
        .u64_histogram("gen_ai.client.token.usage")
        .build()
        .record(3, &[]);
    let warnings = export.shutdown().await;

    // Assert
    assert_eq!(warnings, 0);
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[test]
fn signal_endpoint_replaces_only_the_traces_signal_path() {
    // Arrange
    let endpoints = [
        ("http://host/v1/traces", "logs", Some("http://host/v1/logs")),
        (
            "http://host/v1/traces",
            "metrics",
            Some("http://host/v1/metrics"),
        ),
        (
            "https://host/otlp/v1/traces?tenant=a",
            "logs",
            Some("https://host/otlp/v1/logs?tenant=a"),
        ),
        (
            "https://host/i/v1/traces?tenant=a",
            "metrics",
            Some("https://host/i/v1/metrics?tenant=a"),
        ),
        ("http://host/v1/traces/", "metrics", None),
        ("http://host/traces", "logs", None),
    ];

    // Act / Assert
    for (traces, signal, expected) in endpoints {
        let traces = Url::parse(traces).expect("traces URL");
        assert_eq!(
            OtlpExport::signal_endpoint(&traces, signal)
                .as_ref()
                .map(Url::as_str),
            expected
        );
    }
}

#[test]
fn sibling_exporter_configuration_failure_does_not_echo_the_endpoint() {
    // Arrange
    let invalid = Some("http://host/private invalid endpoint");

    // Act
    let errors = [(invalid, None), (None, invalid)].map(|(events, metrics)| {
        OtlpExport::build("http://host/v1/traces", events, metrics, SERVICE)
            .err()
            .expect("invalid sibling exporter configuration")
    });

    // Assert
    for error in errors {
        assert_eq!(error, OtlpError::Configuration);
        assert!(!error.to_string().contains("private"));
    }
}

#[tokio::test]
async fn event_and_metric_shutdown_failures_are_counted() {
    // Arrange
    let export = OtlpExport::start_with_events(Some("http://localhost:4318/v1/traces"), SERVICE)
        .await
        .expect("start")
        .expect("enabled");
    export
        .events
        .as_ref()
        .expect("event provider")
        .shutdown()
        .expect("first event shutdown");
    export
        .metrics
        .as_ref()
        .expect("meter provider")
        .shutdown()
        .expect("first metric shutdown");

    // Act
    let warnings = export.shutdown().await;

    // Assert
    assert_eq!(warnings, 2);
}

#[test]
fn diagnostics_count_sdk_warnings_and_errors_except_export_results() {
    // Arrange
    let count = Arc::new(AtomicU64::new(0));
    let subscriber = Registry::default().with(Diagnostics(Arc::clone(&count)));

    // Act
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(target: "opentelemetry_sdk", "private diagnostic");
        tracing::error!(target: "opentelemetry_otlp", "private diagnostic");
        tracing::info!(target: "opentelemetry_sdk", "information");
        tracing::warn!(target: "ag_telemetry", "application warning");
        tracing::warn!(name: "Export.Failed.NonRetryable", target: "opentelemetry_otlp", "counted");
        tracing::error!(name: "BatchSpanProcessor.ExportError", target: "opentelemetry_sdk", "counted");
    });

    // Assert
    assert_eq!(count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn cli_endpoint_and_protocol_override_environment_and_use_trace_headers() {
    // Arrange
    const CHILD: &str = "AG_TELEMETRY_OTLP_CONFIGURATION_TEST_CHILD";
    const ENDPOINT: &str = "AG_TELEMETRY_OTLP_CONFIGURATION_TEST_ENDPOINT";
    if std::env::var_os(CHILD).is_some() {
        let endpoint = std::env::var(ENDPOINT).expect("endpoint");
        let export = OtlpExport::start(Some(&endpoint), SERVICE)
            .await
            .expect("start")
            .expect("enabled");
        let mut span = export
            .provider
            .tracer("configuration-test")
            .start("configured");
        span.end();
        assert_eq!(export.shutdown().await, 0);
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/custom/traces"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let endpoint = format!("{}/custom/traces", server.uri());

    // Act
    let output = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "otlp::tests::cli_endpoint_and_protocol_override_environment_and_use_trace_headers",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(ENDPOINT, endpoint)
        .env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "invalid endpoint")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .env(
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            "authorization=Bearer%20test-token",
        )
        .env("OTEL_EXPORTER_OTLP_HEADERS", "x-ignored=value")
        .output()
        .await
        .expect("child test");

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["authorization"], "Bearer test-token");
    assert!(!requests[0].headers.contains_key("x-ignored"));
}

/// Records which exporter methods the wrapper forwarded, answering each with
/// a value that differs from the trait default.
#[derive(Debug, Default)]
struct RecordingExporter {
    calls: Mutex<Vec<&'static str>>,
}

impl RecordingExporter {
    fn record(&self, call: &'static str) -> OTelSdkResult {
        self.calls.lock().expect("calls").push(call);

        Err(OTelSdkError::InternalFailure(call.to_string()))
    }
}

impl SdkSpanExporter for RecordingExporter {
    fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        future::ready(self.record("span export"))
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.record("span shutdown_with_timeout")
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.record("span shutdown")
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.record("span force_flush")
    }

    fn set_resource(&mut self, _resource: &Resource) {
        let _ = self.record("span set_resource");
    }
}

impl SdkLogExporter for RecordingExporter {
    fn export(&self, _batch: LogBatch<'_>) -> impl Future<Output = OTelSdkResult> + Send {
        future::ready(self.record("log export"))
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.record("log shutdown_with_timeout")
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.record("log shutdown")
    }

    fn event_enabled(&self, _level: Severity, _target: &str, _name: Option<&str>) -> bool {
        let _ = self.record("log event_enabled");

        false
    }

    fn set_resource(&mut self, _resource: &Resource) {
        let _ = self.record("log set_resource");
    }
}

impl PushMetricExporter for RecordingExporter {
    fn export(&self, _metrics: &ResourceMetrics) -> impl Future<Output = OTelSdkResult> + Send {
        future::ready(self.record("metric export"))
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.record("metric force_flush")
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.record("metric shutdown_with_timeout")
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.record("metric shutdown")
    }

    fn temporality(&self) -> Temporality {
        let _ = self.record("metric temporality");

        Temporality::Delta
    }
}

#[tokio::test]
async fn counted_exporter_forwards_every_method_and_counts_only_rejected_exports() {
    // Arrange
    let failures = Arc::new(AtomicU64::new(0));
    let mut exporter = CountedExporter {
        exporter: RecordingExporter::default(),
        failures: Arc::clone(&failures),
    };
    let resource = Resource::builder_empty().build();

    // Act
    let span_results = [
        SdkSpanExporter::export(&exporter, Vec::new()).await,
        SdkSpanExporter::shutdown_with_timeout(&exporter, Duration::from_secs(1)),
        SdkSpanExporter::shutdown(&exporter),
        SdkSpanExporter::force_flush(&exporter),
    ];
    SdkSpanExporter::set_resource(&mut exporter, &resource);
    let log_results = [
        SdkLogExporter::export(&exporter, LogBatch::new(&[])).await,
        SdkLogExporter::shutdown_with_timeout(&exporter, Duration::from_secs(1)),
        SdkLogExporter::shutdown(&exporter),
    ];
    let event_enabled = SdkLogExporter::event_enabled(&exporter, Severity::Info, "target", None);
    SdkLogExporter::set_resource(&mut exporter, &resource);
    let metric_results = [
        PushMetricExporter::export(&exporter, &ResourceMetrics::default()).await,
        PushMetricExporter::force_flush(&exporter),
        PushMetricExporter::shutdown_with_timeout(&exporter, Duration::from_secs(1)),
        PushMetricExporter::shutdown(&exporter),
    ];
    let temporality = PushMetricExporter::temporality(&exporter);

    // Assert
    assert!(
        span_results
            .iter()
            .chain(&log_results)
            .chain(&metric_results)
            .all(Result::is_err)
    );
    assert!(!event_enabled);
    assert_eq!(temporality, Temporality::Delta);
    assert_eq!(failures.load(Ordering::Relaxed), 3);
    assert_eq!(
        *exporter.exporter.calls.lock().expect("calls"),
        [
            "span export",
            "span shutdown_with_timeout",
            "span shutdown",
            "span force_flush",
            "span set_resource",
            "log export",
            "log shutdown_with_timeout",
            "log shutdown",
            "log event_enabled",
            "log set_resource",
            "metric export",
            "metric force_flush",
            "metric shutdown_with_timeout",
            "metric shutdown",
            "metric temporality",
        ]
    );
}
