use ag_telemetry::otlp::OtlpError;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message as _;
use tokio::io::BufReader;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{parse_cli, provider_response};
use crate::{ChatMode, Cli, CliError, execute_with_telemetry};

async fn model_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(provider_response("hello"))
        .mount(&server)
        .await;

    server
}

fn traced_cli(model_uri: &str, endpoint: &str, database: &std::path::Path) -> Cli {
    parse_cli([
        "ag-harness",
        "--otlp-endpoint",
        endpoint,
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        model_uri,
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("traced chat arguments should parse")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn traced_chat_exports_the_turn_span() {
    // Arrange
    let model = model_server().await;
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&collector)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let endpoint = format!("{}/v1/traces", collector.uri());
    let cli = traced_cli(&model.uri(), &endpoint, &storage.path().join("harness.db"));
    let mut output = Vec::new();
    let mut warnings = Vec::new();

    // Act
    let result = execute_with_telemetry(
        cli,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
        &mut warnings,
    )
    .await;

    // Assert
    assert!(result.is_ok());
    assert_eq!(warnings, [] as [u8; 0]);
    let span_names = collector
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .map(|request| {
            ExportTraceServiceRequest::decode(request.body.as_slice()).expect("OTLP protobuf")
        })
        .flat_map(|payload| payload.resource_spans)
        .flat_map(|resource| resource.scope_spans)
        .flat_map(|scope| scope.spans)
        .map(|span| span.name)
        .collect::<Vec<_>>();
    assert!(span_names.contains(&"invoke_agent".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_export_warns_without_failing_the_chat() {
    // Arrange
    let model = model_server().await;
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&collector)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let endpoint = format!("{}/v1/traces", collector.uri());
    let cli = traced_cli(&model.uri(), &endpoint, &storage.path().join("harness.db"));
    let mut output = Vec::new();
    let mut warnings = Vec::new();

    // Act
    let result = execute_with_telemetry(
        cli,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
        &mut warnings,
    )
    .await;

    // Assert
    assert!(result.is_ok());
    assert!(
        String::from_utf8(output)
            .expect("chat output should be UTF-8")
            .contains("assistant> hello\n")
    );
    assert!(
        String::from_utf8(warnings)
            .expect("warning output should be UTF-8")
            .contains("some traces may be missing.")
    );
}

#[tokio::test]
async fn invalid_endpoint_fails_before_the_chat_starts() {
    // Arrange
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let cli = traced_cli(
        "http://127.0.0.1:9",
        "http://user:private@host/traces",
        &storage.path().join("harness.db"),
    );
    let mut output = Vec::new();
    let mut warnings = Vec::new();

    // Act
    let result = execute_with_telemetry(
        cli,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
        &mut warnings,
    )
    .await;

    // Assert
    let error = result.expect_err("invalid endpoint should fail");
    assert!(matches!(error, CliError::Telemetry(OtlpError::Endpoint)));
    assert!(!error.to_string().contains("private"));
    assert_eq!(output, [] as [u8; 0]);
    assert_eq!(warnings, [] as [u8; 0]);
}
