//! Conformance of [`WireProtocol::ChatCompletions`] provider specifications
//! against an in-process server and recorded fixtures: request shape,
//! byte-identical preset requests, response parsing, structured-output
//! modes, error mapping, and specification validation.

#[path = "support/conformance.rs"]
mod conformance;

use std::error::Error;

use ag_router::ReasoningEffort::{High, Low, Max, Medium};
use ag_router::{
    Auth, Completion, ErrorClass, ModelError, ProviderSpec, Router, StructuredOutputMode,
    WireProtocol, preset,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COMPLETION: &str = include_str!("fixtures/conformance/chat_completions/completion.json");
const RATE_LIMITED: &str = include_str!("fixtures/conformance/chat_completions/rate-limited.json");

/// Starts a server answering every Chat Completions request with `status`
/// and `body`.
async fn server(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    server
}

/// Returns a custom specification for `server` under the `custom` prefix.
fn custom(server: &MockServer) -> ProviderSpec {
    ProviderSpec::new(
        "custom",
        WireProtocol::ChatCompletions,
        format!("{}/v1", server.uri()),
    )
}

/// Executes the golden request through `spec` and returns what the server
/// received.
async fn execute_golden(
    server: &MockServer,
    spec: ProviderSpec,
    model: &str,
    with_tools: bool,
    options: ag_router::GenerationOptions,
) -> Result<wiremock::Request, Box<dyn Error>> {
    let request = conformance::golden_request(model, with_tools, options)?;
    let _ = Router::new([spec]).execute(request).await;

    conformance::only_request(server).await
}

#[tokio::test]
async fn presets_send_byte_identical_requests() -> Result<(), Box<dyn Error>> {
    // Arrange
    let cases = [
        (
            "kimi/kimi-k2.6",
            true,
            conformance::options(Some(High), true, Some(512)),
            include_str!("fixtures/conformance/requests/kimi-k2.6.json"),
        ),
        (
            "kimi/kimi-k2.7-code",
            true,
            conformance::options(Some(Low), true, None),
            include_str!("fixtures/conformance/requests/kimi-k2.7-code.json"),
        ),
        (
            "kimi/kimi-k3",
            false,
            conformance::options(Some(Medium), true, None),
            include_str!("fixtures/conformance/requests/kimi-k3.json"),
        ),
        (
            "muse/muse-spark-1.3",
            true,
            conformance::options(Some(Max), true, Some(512)),
            include_str!("fixtures/conformance/requests/muse-spark-1.3.json"),
        ),
        (
            "qwen/qwen3.8-max",
            true,
            conformance::options(Some(High), true, Some(512)),
            include_str!("fixtures/conformance/requests/qwen3.8-max.json"),
        ),
        (
            "qwen/qwen-plus",
            false,
            conformance::options(Some(Low), false, None),
            include_str!("fixtures/conformance/requests/qwen-plus.json"),
        ),
        (
            "openai-compatible/llama3.3",
            true,
            conformance::options(Some(High), true, Some(512)),
            include_str!("fixtures/conformance/requests/openai-compatible-tools.json"),
        ),
        (
            "openai-compatible/llama3.3",
            false,
            conformance::options(None, false, None),
            include_str!("fixtures/conformance/requests/openai-compatible-no-tools.json"),
        ),
    ];

    for (model, with_tools, options, golden) in cases {
        let server = server(400, "{}").await;
        let base_url = format!("{}/v1", server.uri());
        let (prefix, name) = model.split_once('/').ok_or("model id")?;
        let spec = match prefix {
            "kimi" => preset::kimi(base_url, "test-key", name),
            "muse" => preset::muse(base_url, "test-key", name),
            "qwen" => preset::qwen(base_url, "test-key", name),
            _ => preset::openai_compatible(base_url, Some("test-key".to_string())),
        };

        // Act
        let received = execute_golden(&server, spec, model, with_tools, options).await?;

        // Assert
        conformance::assert_golden(&received, golden)?;
    }

    Ok(())
}

#[tokio::test]
async fn sends_header_auth_extra_headers_and_query_parameters() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, COMPLETION).await;
    let spec = custom(&server)
        .with_auth(Auth::Header {
            name: "api-key".to_string(),
            value: "azure-key".to_string(),
        })
        .with_header("HTTP-Referer", "https://agentty.dev")
        .with_query("api-version", "2026-01-01")
        .with_max_tokens_field(None);

    // Act
    let received = execute_golden(
        &server,
        spec,
        "custom/llama3.3",
        false,
        conformance::options(None, false, None),
    )
    .await?;

    // Assert
    assert_eq!(received.url.query(), Some("api-version=2026-01-01"));
    assert_eq!(
        received.headers.get("api-key").ok_or("api-key")?,
        "azure-key"
    );
    assert_eq!(
        received.headers.get("http-referer").ok_or("referer")?,
        "https://agentty.dev"
    );
    assert!(received.headers.get("authorization").is_none());
    let body: Value = serde_json::from_slice(&received.body)?;
    assert!(body.get("max_tokens").is_none() && body.get("max_completion_tokens").is_none());

    Ok(())
}

#[tokio::test]
async fn parses_a_recorded_completion_with_usage() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, COMPLETION).await;
    let router = Router::new([custom(&server).with_auth(Auth::Bearer("key".to_string()))]);
    let request = conformance::golden_request(
        "custom/llama3.3",
        true,
        conformance::options(None, false, None),
    )?;

    // Act
    let completion = router.execute(request).await?;

    // Assert
    let Completion::Output {
        value, metadata, ..
    } = completion
    else {
        return Err("expected structured output".into());
    };
    assert_eq!(value, json!({"name": "Ada"}));
    assert_eq!(metadata.response_id(), Some("chatcmpl-conformance-1"));
    assert_eq!(metadata.system_fingerprint(), Some("fp_conformance"));
    let usage = metadata.usage().ok_or("usage")?;
    assert_eq!(usage.input_tokens(), Some(120));
    assert_eq!(usage.output_tokens(), Some(8));

    Ok(())
}

#[tokio::test]
async fn structured_output_modes_select_the_response_format() -> Result<(), Box<dyn Error>> {
    // Arrange
    let modes = [
        (
            StructuredOutputMode::JsonSchema,
            json!("json_schema"),
            false,
        ),
        (StructuredOutputMode::JsonObject, json!("json_object"), true),
        (
            StructuredOutputMode::SchemaPrompt,
            json!("json_schema"),
            true,
        ),
    ];

    for (mode, format_type, states_schema) in modes {
        let server = server(200, COMPLETION).await;
        let spec = custom(&server).with_structured_output(mode);

        // Act
        let received = execute_golden(
            &server,
            spec,
            "custom/llama3.3",
            false,
            conformance::options(None, false, None),
        )
        .await?;

        // Assert
        let body: Value = serde_json::from_slice(&received.body)?;
        assert_eq!(body["response_format"]["type"], format_type);
        assert_eq!(
            body["messages"][0]["content"]
                .as_str()
                .is_some_and(|content| content.contains("JSON Schema:")),
            states_schema
        );
    }

    Ok(())
}

#[tokio::test]
async fn maps_provider_errors_with_custom_overflow_markers() -> Result<(), Box<dyn Error>> {
    // Arrange
    let overflow = server(
        400,
        r#"{"error":{"message":"Prompt is TOO LONG for this model"}}"#,
    )
    .await;
    let quota = server(429, RATE_LIMITED).await;
    let unauthorized = server(401, r#"{"error":{"message":"bad key"}}"#).await;
    let spec = |server: &MockServer| {
        custom(server)
            .with_display_name("Custom Gateway")
            .with_overflow_marker("Prompt is too long")
            .with_query("key", "query-secret")
    };
    let request = || {
        conformance::golden_request(
            "custom/llama3.3",
            false,
            conformance::options(None, false, None),
        )
    };

    // Act
    let overflow_error = Router::new([spec(&overflow)])
        .execute(request()?)
        .await
        .err()
        .ok_or("overflow should fail")?;
    let quota_error = Router::new([spec(&quota)])
        .execute(request()?)
        .await
        .err()
        .ok_or("quota should fail")?;
    let unauthorized_error = Router::new([spec(&unauthorized)])
        .execute(request()?)
        .await
        .err()
        .ok_or("unauthorized should fail")?;

    // Assert
    assert_eq!(overflow_error.error_class(), ErrorClass::ContextOverflow);
    assert!(quota_error.is_quota_exceeded());
    assert_eq!(quota_error.http_status(), Some(429));
    assert_eq!(unauthorized_error.error_class(), ErrorClass::Provider);
    assert!(
        unauthorized_error
            .to_string()
            .contains("Custom Gateway returned HTTP 401")
    );
    assert!(!conformance::error_chain(&unauthorized_error).contains("query-secret"));

    Ok(())
}

#[tokio::test]
async fn rejects_an_invalid_specification_before_any_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, COMPLETION).await;
    let spec = custom(&server).with_auth(Auth::Bearer(" ".to_string()));
    let request = conformance::golden_request(
        "custom/llama3.3",
        false,
        conformance::options(None, false, None),
    )?;

    // Act
    let error = Router::new([spec]).execute(request).await;

    // Assert
    assert!(matches!(
        error,
        Err(ModelError::InvalidProviderConfig { .. })
    ));
    conformance::assert_no_request(&server).await
}
