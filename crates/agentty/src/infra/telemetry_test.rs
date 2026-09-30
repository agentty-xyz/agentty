use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opentelemetry::trace::{Span as _, Tracer as _, TracerProvider as _};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::infra::telemetry::{Diagnostics, Telemetry};
use crate::test_support::telemetry::TRACER_PROVIDER_LOCK;

#[tokio::test]
async fn initialization_worker_failure_returns_a_bounded_error() {
    // Arrange / Act
    let result = Telemetry::initialize(|| {
        std::panic::resume_unwind(Box::new("private initialization detail"))
    })
    .await;

    // Assert
    assert_eq!(
        result.err().expect("worker failure"),
        "OTLP exporter initialization failed"
    );
}

#[test]
fn exporter_configuration_failure_does_not_echo_the_endpoint() {
    // Arrange / Act
    let result = Telemetry::build("http://host/private invalid endpoint");

    // Assert
    assert_eq!(
        result.err().expect("invalid exporter configuration"),
        "Could not configure OTLP HTTP/protobuf export"
    );
}

#[tokio::test]
async fn absent_endpoint_never_constructs_an_exporter() {
    // Arrange / Act
    let telemetry = Telemetry::start(None).await.expect("disabled startup");

    // Assert
    assert!(telemetry.is_none());
}

#[tokio::test]
async fn invalid_endpoint_errors_do_not_echo_credentials() {
    // Arrange
    let endpoints = [
        "",
        "localhost:4318",
        "file:///private",
        "ftp://host/traces",
        "http://user:secret@host/traces",
        "https://host/traces#secret",
    ];

    // Act / Assert
    for endpoint in endpoints {
        let result = Telemetry::start(Some(endpoint)).await;
        let error = result.err().expect("invalid endpoint");
        assert!(error.contains("--otlp-endpoint"));
        assert!(!error.contains("secret"));
    }
}

#[tokio::test]
async fn shutdown_exports_protobuf_to_the_exact_endpoint() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/custom/traces"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let endpoint = format!("{}/custom/traces", server.uri());
    let telemetry = Telemetry::start(Some(&endpoint))
        .await
        .expect("start")
        .expect("enabled");

    // Act
    let tracer = telemetry.provider.tracer("export-test");
    let mut span = tracer.start("session.turn");
    span.end();
    let warnings = telemetry.shutdown().await;

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
    assert!(
        resource
            .attributes
            .iter()
            .any(|attribute| attribute.key == "service.name")
    );
    assert!(
        resource
            .attributes
            .iter()
            .any(|attribute| attribute.key == "service.version")
    );
    assert!(
        resource
            .attributes
            .iter()
            .any(|attribute| attribute.key == "service.instance.id")
    );
}

#[tokio::test]
async fn failed_export_is_reported_without_changing_application_work() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let endpoint = format!("{}/v1/traces", server.uri());
    let telemetry = Telemetry::start(Some(&endpoint))
        .await
        .expect("start")
        .expect("enabled");
    telemetry.install().expect("install");
    assert!(telemetry.install().is_err());

    // Act
    let mut span = telemetry
        .provider
        .tracer("failure-test")
        .start("completed-work");
    span.end();
    let diagnostics = telemetry.shutdown().await;

    // Assert
    assert!(diagnostics > 0);
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
    let telemetry = Telemetry::start(Some("http://localhost:4318/v1/traces"))
        .await
        .expect("start")
        .expect("enabled");
    telemetry.provider.shutdown().expect("first shutdown");

    // Act
    let shutdown_warnings = telemetry.shutdown().await;

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
    let telemetry = Telemetry::start(Some(&endpoint))
        .await
        .expect("start")
        .expect("enabled");

    // Act
    let mut span = telemetry
        .provider
        .tracer("unicode-test")
        .start("configured");
    span.end();
    let warnings = telemetry.shutdown().await;

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
        tracing::warn!(target: "agentty", "application warning");
    });

    // Assert
    assert_eq!(count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn cli_endpoint_and_protocol_override_environment_and_use_trace_headers() {
    // Arrange
    const CHILD: &str = "AGENTTY_OTLP_CONFIGURATION_TEST_CHILD";
    const ENDPOINT: &str = "AGENTTY_OTLP_CONFIGURATION_TEST_ENDPOINT";
    if std::env::var_os(CHILD).is_some() {
        let endpoint = std::env::var(ENDPOINT).expect("endpoint");
        let telemetry = Telemetry::start(Some(&endpoint))
            .await
            .expect("start")
            .expect("enabled");
        let mut span = telemetry
            .provider
            .tracer("configuration-test")
            .start("configured");
        span.end();
        assert_eq!(telemetry.shutdown().await, 0);
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
        .args(["--exact", "infra::telemetry::tests::cli_endpoint_and_protocol_override_environment_and_use_trace_headers", "--nocapture"])
        .env(CHILD, "1").env(ENDPOINT, endpoint)
        .env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "invalid endpoint")
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
        .env("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "authorization=Bearer%20test-token")
        .env("OTEL_EXPORTER_OTLP_HEADERS", "x-ignored=value")
        .output().await.expect("child test");

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
