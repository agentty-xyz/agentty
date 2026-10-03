use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opentelemetry::trace::{Span as _, Tracer as _, TracerProvider as _};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value;
use prost::Message as _;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Diagnostics, OtlpError, OtlpExport, Service};

const SERVICE: Service = Service {
    name: "otlp-test",
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
    let error = OtlpExport::build("http://host/private invalid endpoint", SERVICE)
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
    for endpoint in endpoints {
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

#[test]
fn diagnostics_count_only_sdk_warnings_and_errors() {
    // Arrange
    let count = Arc::new(AtomicU64::new(0));
    let subscriber = Registry::default().with(Diagnostics(Arc::clone(&count)));

    // Act
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(target: "opentelemetry_sdk", "private diagnostic");
        tracing::error!(target: "opentelemetry_otlp", "private diagnostic");
        tracing::info!(target: "opentelemetry_sdk", "information");
        tracing::warn!(target: "ag_telemetry", "application warning");
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
