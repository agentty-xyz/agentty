//! Conformance of [`WireProtocol::ChatCompletions`] provider specifications
//! against an in-process server and recorded fixtures: request shape,
//! byte-identical preset requests, response parsing, structured-output
//! modes, error mapping, and specification validation.

#[path = "support/conformance.rs"]
mod conformance;

use std::error::Error;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ag_router::ReasoningEffort::{High, Low, Max, Medium};
use ag_router::{
    Auth, Completion, DeltaObserver, ErrorClass, InstructionPlacement, MaxTokensField, ModelError,
    ProviderSpec, ResponseDelta, Router, StructuredOutputMode, WireProtocol, preset,
};
use async_trait::async_trait;
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
            "kimi-k2.6",
        ),
        (
            "kimi/kimi-k2.7-code",
            true,
            conformance::options(Some(Low), true, None),
            "kimi-k2.7-code",
        ),
        (
            "kimi/kimi-k3",
            false,
            conformance::options(Some(Medium), true, None),
            "kimi-k3",
        ),
        (
            "muse/muse-spark-1.3",
            true,
            conformance::options(Some(Max), true, Some(512)),
            "muse-spark-1.3",
        ),
        (
            "qwen/qwen3.8-max",
            true,
            conformance::options(Some(High), true, Some(512)),
            "qwen3.8-max",
        ),
        (
            "qwen/qwen-plus",
            false,
            conformance::options(Some(Low), false, None),
            "qwen-plus",
        ),
        (
            "openai-compatible/llama3.3",
            true,
            conformance::options(Some(High), true, Some(512)),
            "openai-compatible-tools",
        ),
        (
            "openai-compatible/llama3.3",
            false,
            conformance::options(None, false, None),
            "openai-compatible-no-tools",
        ),
    ];

    for (model, with_tools, options, fixture) in cases {
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
        conformance::assert_golden(&received, fixture)?;
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
async fn compat_quirks_apply_only_when_opted_in() -> Result<(), Box<dyn Error>> {
    // Arrange
    let instructions = "[application instructions for this turn]\nBe brief.";
    let separate = json!([
        {"content": "Who wrote it?", "role": "user"},
        {"content": instructions, "role": "user"}
    ]);
    let joined = json!([{
        "content": format!("Who wrote it?\n\n{instructions}"),
        "role": "user"
    }]);
    // Each quirk flips alone, so neither opt-in implies the other.
    let cases = [
        (None, None, "max_completion_tokens", &separate),
        (
            Some(InstructionPlacement::JoinPreviousUser),
            None,
            "max_completion_tokens",
            &joined,
        ),
        (
            None,
            Some(MaxTokensField::MaxTokens),
            "max_tokens",
            &separate,
        ),
        (
            Some(InstructionPlacement::JoinPreviousUser),
            Some(MaxTokensField::MaxTokens),
            "max_tokens",
            &joined,
        ),
    ];

    for (placement, field, bound_field, messages) in cases {
        let server = server(200, COMPLETION).await;
        let mut spec = custom(&server);
        if let Some(placement) = placement {
            spec = spec.with_instruction_placement(placement);
        }
        if let Some(field) = field {
            spec = spec.with_max_tokens_field(Some(field));
        }
        let opted_in = (placement, field);
        let mut request = ag_router::ModelRequest::text_chat(
            "custom/llama3.3",
            vec![
                ag_router::ModelMessage::User("Who wrote it?".to_string()),
                ag_router::ModelMessage::Instructions("Be brief.".to_string()),
            ],
            Vec::new(),
        );
        request.options.max_output_tokens = std::num::NonZeroU32::new(64);

        // Act
        let _ = Router::new([spec]).execute(request).await;
        let received = conformance::only_request(&server).await?;

        // Assert
        let body: Value = serde_json::from_slice(&received.body)?;
        assert_eq!(&body["messages"], messages, "opted in: {opted_in:?}");
        assert_eq!(body[bound_field], 64, "opted in: {opted_in:?}");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({"include_usage": true}));
        assert_eq!(body.as_object().ok_or("object")?.len(), 5);
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

/// Starts a server answering every Chat Completions request with `chunks`
/// as a server-sent event stream ended by `[DONE]`.
async fn stream_server(chunks: &[Value]) -> MockServer {
    let mut body = events(chunks);
    body.push_str("data: [DONE]\n\n");

    event_stream_server(body).await
}

/// Serializes `chunks` as server-sent events.
fn events(chunks: &[Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        let _ = write!(body, "data: {chunk}\n\n");
    }

    body
}

/// Starts a server answering every Chat Completions request with the
/// event stream `body`.
async fn event_stream_server(body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    server
}

/// Records every streamed delta in order.
#[derive(Default)]
struct RecordedDeltas(Mutex<Vec<ResponseDelta>>);

#[async_trait]
impl DeltaObserver for RecordedDeltas {
    async fn delta(&self, delta: ResponseDelta) {
        if let Ok(mut deltas) = self.0.lock() {
            deltas.push(delta);
        }
    }
}

#[tokio::test]
async fn streams_deltas_into_the_completion_an_unstreamed_response_returns()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let unstreamed = server(
        200,
        &json!({
            "id": "chatcmpl-7",
            "model": "llama3.3",
            "system_fingerprint": "fp_7",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": "Reading both.",
                    "reasoning_content": "Need a and b.",
                    "tool_calls": [
                        {"id": "call_a", "type": "function",
                         "function": {"name": "read", "arguments": "{\"path\":\"a.txt\"}"}},
                        {"id": "call_b", "type": "function",
                         "function": {"name": "read", "arguments": "{\"path\":\"b.txt\"}"}}
                    ]
                }
            }],
            "usage": {"prompt_tokens": 120, "completion_tokens": 30, "total_tokens": 150}
        })
        .to_string(),
    )
    .await;
    let call = |index: u64, id: Option<&str>, name: Option<&str>, arguments: &str| {
        let mut call = json!({"index": index, "function": {"arguments": arguments}});
        if let Some(id) = id {
            call["id"] = json!(id);
            call["type"] = json!("function");
        }
        if let Some(name) = name {
            call["function"]["name"] = json!(name);
        }
        json!({"id": "chatcmpl-7", "model": "llama3.3", "system_fingerprint": "fp_7",
               "choices": [{"index": 0, "delta": {"tool_calls": [call]}, "finish_reason": null}]})
    };
    let delta = |delta: Value| json!({"id": "chatcmpl-7", "choices": [{"index": 0, "delta": delta, "finish_reason": null}]});
    let streamed = stream_server(&[
        delta(json!({"role": "assistant", "content": ""})),
        delta(json!({"reasoning_content": "Need a "})),
        delta(json!({"reasoning_content": "and b."})),
        delta(json!({"content": "Reading "})),
        delta(json!({"content": "both."})),
        call(0, Some("call_a"), Some("read"), ""),
        call(0, None, None, "{\"path\":"),
        call(0, None, None, "\"a.txt\"}"),
        call(1, Some("call_b"), Some("read"), "{\"path\":\"b.txt\"}"),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        json!({"choices": [],
               "usage": {"prompt_tokens": 120, "completion_tokens": 30, "total_tokens": 150}}),
    ])
    .await;
    let spec = |server: &MockServer| custom(server).with_reasoning_text_replay(true);
    let request = || {
        conformance::golden_request(
            "custom/llama3.3",
            true,
            conformance::options(None, false, None),
        )
    };
    let observer = Arc::new(RecordedDeltas::default());

    // Act
    let expected = Router::new([spec(&unstreamed)]).execute(request()?).await?;
    let completion = Router::new([spec(&streamed)])
        .execute_streamed(request()?, observer.clone(), None)
        .await?;
    let unobserved = Router::new([spec(&streamed)]).execute(request()?).await?;

    // Assert
    assert_eq!(completion, expected);
    assert_eq!(unobserved, expected);
    let deltas = observer.0.lock().map_err(|_| "poisoned")?.clone();
    let arguments = |index: usize, arguments: &str| ResponseDelta::ToolArguments {
        arguments: arguments.to_string(),
        index,
    };
    assert_eq!(
        deltas,
        [
            ResponseDelta::Reasoning("Need a ".to_string()),
            ResponseDelta::Reasoning("and b.".to_string()),
            ResponseDelta::Text("Reading ".to_string()),
            ResponseDelta::Text("both.".to_string()),
            arguments(0, "{\"path\":"),
            arguments(0, "\"a.txt\"}"),
            arguments(1, "{\"path\":\"b.txt\"}"),
        ]
    );
    let received = conformance::only_request(&unstreamed).await?;
    let body: Value = serde_json::from_slice(&received.body)?;
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));

    Ok(())
}

#[tokio::test]
async fn stream_usage_can_be_turned_off_and_read_from_the_choice() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = stream_server(&[
        json!({"choices": [{"index": 0, "delta": {"content": "{\"name\":\"Ada\"}"}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop",
                            "usage": {"prompt_tokens": 9, "completion_tokens": 4}}]}),
    ])
    .await;
    let request = conformance::golden_request(
        "custom/llama3.3",
        false,
        conformance::options(None, false, None),
    )?;

    // Act
    let completion = Router::new([custom(&server).with_stream_usage(false)])
        .execute(request)
        .await?;

    // Assert
    let Completion::Output {
        value, metadata, ..
    } = completion
    else {
        return Err("expected structured output".into());
    };
    assert_eq!(value, json!({"name": "Ada"}));
    assert_eq!(metadata.usage().ok_or("usage")?.input_tokens(), Some(9));
    let body: Value = serde_json::from_slice(&conformance::only_request(&server).await?.body)?;
    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());

    Ok(())
}

#[tokio::test]
async fn reads_a_stream_that_closes_without_done_and_keeps_top_level_usage()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let mut body = events(&[json!({
        "choices": [],
        "usage": {"prompt_tokens": 9, "completion_tokens": 4}
    })]);
    // The last chunk closes the stream without its blank line.
    let last = json!({"choices": [{"index": 0, "delta": {"content": "{\"name\":\"Ada\"}"},
                                   "finish_reason": "stop", "usage": {"prompt_tokens": 1}}]});
    let _ = write!(body, "data: {last}");
    let server = event_stream_server(body).await;
    let request = conformance::golden_request(
        "custom/llama3.3",
        false,
        conformance::options(None, false, None),
    )?;
    let observer = Arc::new(RecordedDeltas::default());

    // Act
    let completion = Router::new([custom(&server)])
        .execute_streamed(request, observer.clone(), None)
        .await?;

    // Assert
    let Completion::Output { metadata, .. } = completion else {
        return Err("expected structured output".into());
    };
    assert_eq!(metadata.usage().ok_or("usage")?.input_tokens(), Some(9));
    let deltas = observer.0.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(
        deltas,
        [ResponseDelta::Text("{\"name\":\"Ada\"}".to_string())]
    );

    Ok(())
}

#[tokio::test]
async fn classifies_failures_reported_inside_the_stream() -> Result<(), Box<dyn Error>> {
    // Arrange
    let overflow = stream_server(&[
        json!({"choices": [{"index": 0, "delta": {"content": "partial"}}]}),
        json!({"error": {"code": "context_length_exceeded", "message": "too long"}}),
    ])
    .await;
    let unfinished =
        stream_server(&[json!({"choices": [{"index": 0, "delta": {"content": "par"}}]})]).await;
    let request = || {
        conformance::golden_request(
            "custom/llama3.3",
            false,
            conformance::options(None, false, None),
        )
    };

    // Act
    let overflow_error = Router::new([custom(&overflow)])
        .execute(request()?)
        .await
        .err()
        .ok_or("the error chunk should fail the request")?;
    let unfinished_error = Router::new([custom(&unfinished)])
        .execute(request()?)
        .await
        .err()
        .ok_or("a stream without a finished choice should fail")?;

    // Assert
    assert_eq!(overflow_error.error_class(), ErrorClass::ContextOverflow);
    assert!(
        conformance::error_chain(&overflow_error)
            .contains("custom response failed with `context_length_exceeded`: too long")
    );
    assert_eq!(unfinished_error.error_class(), ErrorClass::Transport);

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
