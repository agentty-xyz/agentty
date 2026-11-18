//! Conformance of [`WireProtocol::Responses`] provider specifications
//! against an in-process server and recorded event-stream fixtures: request
//! shape, byte-identical preset requests, stream parsing, structured output,
//! error mapping, and specification validation.

#[path = "support/conformance.rs"]
mod conformance;

use std::error::Error;
use std::fmt::Write as _;
use std::sync::Arc;

use ag_router::ReasoningEffort::{High, Max};
use ag_router::{
    Auth, Completion, CredentialSource, ErrorClass, ModelError, ProviderSpec, ReasoningMapping,
    Router, StructuredOutputMode, WireProtocol, preset,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COMPLETED_EVENTS: &str = include_str!("fixtures/conformance/responses/completed-events.json");
const FAILED_EVENTS: &str = include_str!("fixtures/conformance/responses/failed-events.json");

/// Credential returning the fixed token the golden `ChatGPT` request
/// recorded.
struct PlanToken;

#[async_trait]
impl CredentialSource for PlanToken {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("plan-token".to_string())
    }
}

/// Serializes the recorded `events` as a server-sent event stream.
fn stream(events: &str) -> Result<String, Box<dyn Error>> {
    let events: Vec<Value> = serde_json::from_str(events)?;

    Ok(events.iter().fold(String::new(), |mut stream, event| {
        let _ = write!(
            stream,
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap_or_default()
        );

        stream
    }))
}

/// Starts a server answering every Responses request with `status` and
/// `body`.
async fn server(status: u16, body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    server
}

/// Returns a custom specification for `server` under the `custom` prefix.
fn custom(server: &MockServer) -> ProviderSpec {
    ProviderSpec::new(
        "custom",
        WireProtocol::Responses,
        format!("{}/v1", server.uri()),
    )
}

/// Executes the golden request through `spec` and returns what the server
/// received.
async fn execute_golden(
    server: &MockServer,
    spec: ProviderSpec,
    model: &str,
    options: ag_router::GenerationOptions,
) -> Result<wiremock::Request, Box<dyn Error>> {
    let request = conformance::golden_request(model, true, options)?;
    let _ = Router::new([spec]).execute(request).await;

    conformance::only_request(server).await
}

#[tokio::test]
async fn presets_send_byte_identical_requests() -> Result<(), Box<dyn Error>> {
    // Arrange
    let cases = [
        (
            "openai/gpt-6-luna",
            conformance::options(Some(Max), true, Some(512)),
            include_str!("fixtures/conformance/requests/openai-gpt-6-luna.json"),
        ),
        (
            "openai/gpt-5.3-codex",
            conformance::options(Some(Max), false, None),
            include_str!("fixtures/conformance/requests/openai-gpt-5.3-codex.json"),
        ),
        (
            "chatgpt/gpt-6-sol",
            conformance::options(Some(High), true, None),
            include_str!("fixtures/conformance/requests/chatgpt-gpt-6-sol.json"),
        ),
    ];

    for (model, options, golden) in cases {
        let server = server(400, "{}".to_string()).await;
        let base_url = format!("{}/v1", server.uri());
        let (prefix, name) = model.split_once('/').ok_or("model id")?;
        let spec = if prefix == "chatgpt" {
            preset::chatgpt(base_url, Arc::new(PlanToken), name)
        } else {
            preset::openai(base_url, "test-key", name)
        };

        // Act
        let received = execute_golden(&server, spec, model, options).await?;

        // Assert
        conformance::assert_golden(&received, golden)?;
    }

    Ok(())
}

#[tokio::test]
async fn sends_header_auth_extra_headers_and_query_parameters() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, stream(COMPLETED_EVENTS)?).await;
    let spec = custom(&server)
        .with_auth(Auth::Header {
            name: "api-key".to_string(),
            value: "azure-key".to_string(),
        })
        .with_header("x-ms-client-request-id", "conformance")
        .with_query("api-version", "preview");

    // Act
    let received = execute_golden(
        &server,
        spec,
        "custom/gpt-6-luna",
        conformance::options(None, false, None),
    )
    .await?;

    // Assert
    assert_eq!(received.url.query(), Some("api-version=preview"));
    assert_eq!(
        received.headers.get("api-key").ok_or("api-key")?,
        "azure-key"
    );
    assert_eq!(
        received
            .headers
            .get("x-ms-client-request-id")
            .ok_or("request id")?,
        "conformance"
    );
    assert!(received.headers.get("authorization").is_none());

    Ok(())
}

#[tokio::test]
async fn parses_a_recorded_stream_with_usage_and_reasoning() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, stream(COMPLETED_EVENTS)?).await;
    let router = Router::new([custom(&server).with_auth(Auth::Bearer("key".to_string()))]);
    let request = conformance::golden_request(
        "custom/gpt-6-luna",
        true,
        conformance::options(None, false, None),
    )?;

    // Act
    let completion = router.execute(request).await?;

    // Assert
    let Completion::Output {
        value,
        metadata,
        reasoning_content,
    } = completion
    else {
        return Err("expected structured output".into());
    };
    assert_eq!(value, json!({"name": "Ada"}));
    assert_eq!(metadata.response_id(), Some("resp_conformance_1"));
    let usage = metadata.usage().ok_or("usage")?;
    assert_eq!(usage.input_tokens(), Some(1200));
    assert_eq!(usage.reasoning_tokens(), Some(64));
    assert!(
        reasoning_content
            .as_deref()
            .is_some_and(|reasoning| reasoning.contains("gAAAA-conformance"))
    );

    Ok(())
}

#[tokio::test]
async fn structured_output_is_native_and_policy_fields_are_optional() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let server = server(200, stream(COMPLETED_EVENTS)?).await;
    let spec = custom(&server)
        .with_prompt_cache_key(false)
        .with_reasoning(ReasoningMapping::None);

    // Act
    let received = execute_golden(
        &server,
        spec,
        "custom/gpt-6-luna",
        conformance::options(Some(High), true, None),
    )
    .await?;

    // Assert
    let body: Value = serde_json::from_slice(&received.body)?;
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["text"]["format"]["name"], "person");
    assert!(body.get("reasoning").is_none());
    assert!(body.get("prompt_cache_key").is_none());

    Ok(())
}

#[tokio::test]
async fn maps_streamed_and_http_failures() -> Result<(), Box<dyn Error>> {
    // Arrange
    let failed = server(200, stream(FAILED_EVENTS)?).await;
    let unauthorized = server(401, r#"{"error":{"message":"bad key"}}"#.to_string()).await;
    let spec = |server: &MockServer| {
        custom(server)
            .with_display_name("Custom Gateway")
            .with_overflow_marker("Prompt is too long")
            .with_query("key", "query-secret")
    };
    let request = || {
        conformance::golden_request(
            "custom/gpt-6-luna",
            true,
            conformance::options(None, false, None),
        )
    };

    // Act
    let streamed = Router::new([spec(&failed)])
        .execute(request()?)
        .await
        .err()
        .ok_or("the stream should fail")?;
    let rejected = Router::new([spec(&unauthorized)])
        .execute(request()?)
        .await
        .err()
        .ok_or("the request should fail")?;

    // Assert
    assert_eq!(streamed.error_class(), ErrorClass::ContextOverflow);
    assert!(streamed.to_string().contains("Custom Gateway"));
    assert_eq!(rejected.error_class(), ErrorClass::Provider);
    assert_eq!(rejected.http_status(), Some(401));
    assert!(!conformance::error_chain(&rejected).contains("query-secret"));

    Ok(())
}

#[tokio::test]
async fn rejects_invalid_specifications_and_bounds_before_any_request() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let server = server(200, stream(COMPLETED_EVENTS)?).await;
    let invalid = custom(&server).with_structured_output(StructuredOutputMode::SchemaPrompt);
    let request = |max_output_tokens| {
        conformance::golden_request(
            "custom/gpt-6-luna",
            true,
            conformance::options(None, false, max_output_tokens),
        )
    };

    // Act
    let error = Router::new([invalid]).execute(request(None)?).await.err();
    let unbounded = Router::new([custom(&server).with_max_tokens_field(None)])
        .execute(request(Some(512))?)
        .await
        .err();
    let too_small = Router::new([custom(&server)])
        .execute(request(Some(8))?)
        .await
        .err();

    // Assert
    assert!(matches!(
        error,
        Some(ModelError::InvalidProviderConfig { .. })
    ));
    assert!(matches!(
        unbounded,
        Some(ModelError::UnsupportedGeneration { reason })
            if reason == "custom does not accept an output-token bound"
    ));
    assert!(matches!(
        too_small,
        Some(ModelError::UnsupportedGeneration { reason })
            if reason == "custom requires an output-token bound of at least 16"
    ));
    conformance::assert_no_request(&server).await
}
