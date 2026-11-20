//! Conformance of [`WireProtocol::AnthropicMessages`] provider
//! specifications against an in-process server and recorded fixtures:
//! request shape, byte-identical preset requests, response parsing, signed
//! thinking replay, structured-output modes, images, stop reasons, error
//! mapping, streamed responses and their deltas, prompt-cache breakpoints,
//! and specification validation.

#[path = "support/conformance.rs"]
mod conformance;

use std::error::Error;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_router::ReasoningEffort::{High, Max, XHigh};
use ag_router::{
    Completion, DeltaObserver, ErrorClass, ImageContent, ImageMediaType, InputBlock, ModelError,
    ModelMessage, ModelRequest, ProviderSpec, ReasoningMapping, ReasoningView, ResponseDelta,
    RetryKind, RetryNotice, RetryObserver, RetryPolicy, Router, StructuredOutputMode,
    ThinkingOptions, ToolDefinition, TurnInput, WireProtocol, preset, reasoning_view,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FORCED_TOOL: &str = include_str!("fixtures/conformance/anthropic_messages/forced-tool.json");
const MESSAGE: &str = include_str!("fixtures/conformance/anthropic_messages/message.json");
const OVERLOADED: &str = include_str!("fixtures/conformance/anthropic_messages/overloaded.json");
const PROMPT_TOO_LONG: &str =
    include_str!("fixtures/conformance/anthropic_messages/prompt-too-long.json");
const REFUSAL: &str = include_str!("fixtures/conformance/anthropic_messages/refusal.json");
const TOOL_USE: &str = include_str!("fixtures/conformance/anthropic_messages/tool-use.json");

/// Starts a server answering every Messages request with `status` and
/// `body`.
async fn server(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    server
}

/// Returns a custom specification for `server` under the `custom` prefix.
fn custom(server: &MockServer) -> ProviderSpec {
    ProviderSpec::new(
        "custom",
        WireProtocol::AnthropicMessages,
        format!("{}/v1", server.uri()),
    )
}

/// Returns the Anthropic preset of `model` for `server`.
fn anthropic(server: &MockServer, model: &str) -> ProviderSpec {
    preset::anthropic(format!("{}/v1", server.uri()), "test-key", model)
}

/// Returns the JSON body `server` received in its one request.
async fn received_body(server: &MockServer) -> Result<Value, Box<dyn Error>> {
    let request = conformance::only_request(server).await?;

    Ok(serde_json::from_slice(&request.body)?)
}

/// Returns a structured request for `model` with `messages`, no tools, and
/// the conformance schema.
fn structured(model: &str, messages: Vec<ModelMessage>) -> Result<ModelRequest, Box<dyn Error>> {
    Ok(ModelRequest::chat(
        model,
        messages,
        Vec::new(),
        conformance::format()?,
    ))
}

#[tokio::test]
async fn presets_send_byte_identical_requests() -> Result<(), Box<dyn Error>> {
    // Arrange
    let cases = [
        (
            "anthropic/claude-opus-5-5",
            conformance::options(Some(Max), true, Some(512)),
            "anthropic-claude-opus-5-5",
        ),
        (
            "anthropic/claude-sonnet-4-6",
            conformance::options(Some(XHigh), false, None),
            "anthropic-claude-sonnet-4-6",
        ),
        (
            "anthropic/claude-haiku-4-5",
            conformance::options(Some(High), false, None),
            "anthropic-claude-haiku-4-5",
        ),
    ];

    for (model, options, fixture) in cases {
        let server = server(400, "{}").await;
        let name = model.split_once('/').ok_or("model id")?.1;
        let request = conformance::golden_request(model, true, options)?;

        // Act
        let _ = Router::new([anthropic(&server, name)])
            .execute(request)
            .await;

        // Assert
        let received = conformance::only_request(&server).await?;
        conformance::assert_golden(&received, fixture)?;
    }

    Ok(())
}

#[tokio::test]
async fn parses_a_recorded_message_with_usage_and_signed_thinking() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, MESSAGE).await;
    let request = conformance::golden_request(
        "anthropic/claude-opus-5-5",
        true,
        conformance::options(None, false, None),
    )?;

    // Act
    let completion = Router::new([anthropic(&server, "claude-opus-5-5")])
        .execute(request)
        .await?;

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
    assert_eq!(metadata.finish_reason(), "stop");
    assert_eq!(metadata.response_id(), Some("msg_conformance_1"));
    assert_eq!(metadata.response_model(), Some("claude-opus-5-5"));
    let usage = metadata.usage().ok_or("usage")?;
    assert_eq!(usage.input_tokens(), Some(1120));
    assert_eq!(usage.cache_hit_tokens(), Some(1000));
    assert_eq!(usage.cache_miss_tokens(), Some(120));
    assert_eq!(usage.cache_write_tokens(), Some(20));
    assert_eq!(usage.output_tokens(), Some(50));
    assert_eq!(usage.reasoning_tokens(), None);
    assert_eq!(usage.total_tokens(), Some(1170));
    let reasoning_content = reasoning_content.ok_or("reasoning")?;
    assert_eq!(
        serde_json::from_str::<Value>(&reasoning_content)?,
        json!([
            {"type": "thinking", "thinking": "The file names Ada.", "signature": "sig-conformance-1"},
            {"type": "redacted_thinking", "data": "redacted-conformance-1"},
            {"type": "text"}
        ])
    );
    assert_eq!(
        reasoning_view(&reasoning_content),
        ReasoningView::Summary("The file names Ada.".to_string())
    );

    Ok(())
}

#[tokio::test]
async fn replays_signed_thinking_verbatim_in_its_original_order() -> Result<(), Box<dyn Error>> {
    // Arrange
    let calling = server(200, TOOL_USE).await;
    let replaying = server(400, "{}").await;
    let request = |messages| structured("anthropic/claude-opus-5-5", messages);
    let first = Router::new([anthropic(&calling, "claude-opus-5-5")])
        .execute(request(vec![ModelMessage::User(
            "Find the name".to_string(),
        )])?)
        .await?;
    let Completion::ToolCalls { calls, metadata } = first else {
        return Err("expected tool calls".into());
    };
    let results = calls.iter().map(|call| ModelMessage::ToolResult {
        call_id: call.id().to_string(),
        content: r#"{"text":"Ada"}"#.to_string(),
        name: call.name().to_string(),
    });
    let mut history = vec![
        ModelMessage::User("Find the name".to_string()),
        ModelMessage::AssistantToolCalls(calls.clone()),
    ];
    history.extend(results);

    // Act
    let _ = Router::new([anthropic(&replaying, "claude-opus-5-5")])
        .execute(request(history)?)
        .await;

    // Assert
    assert_eq!(metadata.finish_reason(), "tool_calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].assistant_text(), Some("Reading the files."));
    assert!(calls[1].reasoning_content().is_none());
    let recorded: Value = serde_json::from_str(TOOL_USE)?;
    let body = received_body(&replaying).await?;
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "Find the name"}]},
            {"role": "assistant", "content": recorded["content"]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"text\":\"Ada\"}"},
                {"type": "tool_result", "tool_use_id": "toolu_2", "content": "{\"text\":\"Ada\"}",
                    "cache_control": {"type": "ephemeral"}}
            ]}
        ])
    );

    Ok(())
}

#[tokio::test]
async fn replays_a_final_answer_after_its_thinking_and_omits_other_reasoning()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(400, "{}").await;
    let thinking = json!([
        {"type": "thinking", "thinking": "", "signature": "sig-1"},
        {"type": "text"}
    ])
    .to_string();
    let request = structured(
        "anthropic/claude-opus-5-5",
        vec![
            ModelMessage::User("Find the name".to_string()),
            ModelMessage::AssistantReasoning {
                content: r#"{"name":"Ada"}"#.to_string(),
                reasoning_content: thinking.clone(),
            },
            ModelMessage::User("And again".to_string()),
            ModelMessage::AssistantReasoning {
                content: r#"{"name":"Ada"}"#.to_string(),
                reasoning_content: "plain reasoning another provider wrote".to_string(),
            },
            ModelMessage::Assistant(" ".to_string()),
            ModelMessage::User("Once more".to_string()),
        ],
    )?;

    // Act
    let _ = Router::new([anthropic(&server, "claude-opus-5-5")])
        .execute(request)
        .await;

    // Assert
    let body = received_body(&server).await?;
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"type": "thinking", "thinking": "", "signature": "sig-1"},
            {"type": "text", "text": "{\"name\":\"Ada\"}"}
        ])
    );
    assert_eq!(
        body["messages"][3]["content"],
        json!([{"type": "text", "text": "{\"name\":\"Ada\"}"}])
    );
    assert_eq!(body["messages"].as_array().map(Vec::len), Some(5));
    assert_eq!(reasoning_view(&thinking), ReasoningView::Opaque);

    Ok(())
}

#[tokio::test]
async fn sends_images_as_base64_blocks() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, MESSAGE).await;
    let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1];
    let input = TurnInput::new(vec![
        InputBlock::Text("Who is this?".to_string()),
        InputBlock::Image(ImageContent::new(ImageMediaType::Png, png)?),
    ])?;
    let request = structured("custom/claude", vec![ModelMessage::UserInput(input)])?;

    // Act
    Router::new([custom(&server).with_cache_breakpoints(false)])
        .execute(request)
        .await?;

    // Assert
    let body = received_body(&server).await?;
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            {"type": "text", "text": "Who is this?"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgoB"
            }}
        ])
    );

    Ok(())
}

#[tokio::test]
async fn rejects_images_for_a_specification_without_image_input() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, MESSAGE).await;
    let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let input = TurnInput::new(vec![InputBlock::Image(ImageContent::new(
        ImageMediaType::Png,
        png,
    )?)])?;
    let request = structured("custom/claude", vec![ModelMessage::UserInput(input)])?;

    // Act
    let error = Router::new([custom(&server).with_image_input(false)])
        .execute(request)
        .await
        .err();

    // Assert
    assert!(matches!(
        error,
        Some(ModelError::UnsupportedImageInput { .. })
    ));
    conformance::assert_no_request(&server).await
}

#[tokio::test]
async fn forced_tool_output_reads_the_tool_input_as_the_answer() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, FORCED_TOOL).await;
    let spec = custom(&server)
        .with_reasoning(ReasoningMapping::None)
        .with_structured_output(StructuredOutputMode::ForcedTool);
    let request = structured(
        "custom/claude-haiku-4-5",
        vec![ModelMessage::User("Find the name".to_string())],
    )?;

    // Act
    let completion = Router::new([spec]).execute(request).await?;

    // Assert
    let Completion::Output {
        value,
        reasoning_content,
        ..
    } = completion
    else {
        return Err("expected structured output".into());
    };
    assert_eq!(value, json!({"name": "Ada"}));
    assert_eq!(reasoning_content, None);
    let body = received_body(&server).await?;
    assert_eq!(
        body["tool_choice"],
        json!({"type": "tool", "name": "structured_output"})
    );
    assert_eq!(body["tools"][0]["name"], "structured_output");
    assert_eq!(
        body["tools"][0]["input_schema"],
        *conformance::format()?.schema().value()
    );
    assert!(body.get("output_config").is_none());

    Ok(())
}

#[tokio::test]
async fn forced_tool_output_requires_a_call_beside_tools_and_lets_thinking_models_choose()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let with_tools = server(200, MESSAGE).await;
    let thinking = server(200, MESSAGE).await;
    let forced = custom(&with_tools)
        .with_reasoning(ReasoningMapping::None)
        .with_structured_output(StructuredOutputMode::ForcedTool);
    // Its effort names make it a model that may think by default.
    let free = custom(&thinking).with_structured_output(StructuredOutputMode::ForcedTool);
    let request = || {
        conformance::golden_request(
            "custom/claude",
            true,
            conformance::options(None, false, None),
        )
    };

    // Act
    Router::new([forced]).execute(request()?).await?;
    let answer = Router::new([free]).execute(request()?).await?;

    // Assert
    let tool_body = received_body(&with_tools).await?;
    assert_eq!(tool_body["tool_choice"], json!({"type": "any"}));
    assert_eq!(tool_body["tools"][1]["name"], "structured_output");
    let thinking_body = received_body(&thinking).await?;
    assert_eq!(thinking_body["tool_choice"], json!({"type": "auto"}));
    assert!(thinking_body.get("thinking").is_none());
    assert!(matches!(answer, Completion::Output { value, .. } if value == json!({"name": "Ada"})));

    Ok(())
}

#[tokio::test]
async fn forced_tool_output_rejects_a_tool_using_its_reserved_name() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, FORCED_TOOL).await;
    let spec = custom(&server).with_structured_output(StructuredOutputMode::ForcedTool);
    let request = ModelRequest::chat(
        "custom/claude-opus-5",
        vec![ModelMessage::User("Find the name".to_string())],
        vec![ToolDefinition::new(
            "structured_output",
            "Collides.",
            json!({"type": "object"}),
        )],
        conformance::format()?,
    );

    // Act
    let error = Router::new([spec]).execute(request).await.err();

    // Assert
    assert!(matches!(
        error,
        Some(ModelError::UnsupportedOutputSchema { reason })
            if reason.contains("reserves the tool name `structured_output`")
    ));
    conformance::assert_no_request(&server).await
}

#[tokio::test]
async fn maps_stop_reasons_to_failures() -> Result<(), Box<dyn Error>> {
    // Arrange
    let stopped = |stop_reason: &str| {
        json!({
            "id": "msg_stopped",
            "content": [{"type": "text", "text": "{\"na"}],
            "stop_reason": stop_reason,
            "usage": {"input_tokens": 10, "output_tokens": 4}
        })
        .to_string()
    };
    let refusal = server(200, REFUSAL).await;
    let truncated = server(200, &stopped("max_tokens")).await;
    let overflowed = server(200, &stopped("model_context_window_exceeded")).await;
    let execute = |server: MockServer| async move {
        let request = structured(
            "custom/claude",
            vec![ModelMessage::User("Find the name".to_string())],
        )?;

        Router::new([custom(&server).with_display_name("Claude Gateway")])
            .execute(request)
            .await
            .err()
            .ok_or_else(|| Box::<dyn Error>::from("the request should fail"))
    };

    // Act
    let refused = execute(refusal).await?;
    let incomplete = execute(truncated).await?;
    let overflow = execute(overflowed).await?;

    // Assert
    assert!(matches!(
        refused.root_cause(),
        ModelError::IncompleteResponse { reason }
            if reason == "refusal: cyber: The request was declined."
    ));
    assert_eq!(
        refused
            .metadata()
            .map(ag_router::CompletionMetadata::finish_reason),
        Some("refusal")
    );
    assert!(matches!(
        incomplete.root_cause(),
        ModelError::IncompleteResponse { reason } if reason == "max_tokens"
    ));
    assert_eq!(overflow.error_class(), ErrorClass::ContextOverflow);
    assert!(overflow.to_string().contains("Claude Gateway"));
    assert!(
        overflow
            .metadata()
            .and_then(|metadata| metadata.usage())
            .is_some()
    );

    Ok(())
}

#[tokio::test]
async fn maps_overloaded_server_overflow_and_billing_rejections() -> Result<(), Box<dyn Error>> {
    // Arrange
    let overloaded = server(529, OVERLOADED).await;
    let failed = server(
        500,
        r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#,
    )
    .await;
    let too_long = server(400, PROMPT_TOO_LONG).await;
    let billing = server(
        402,
        r#"{"type":"error","error":{"type":"billing_error","message":"Your credit balance is too low."}}"#,
    )
    .await;
    let execute = |server: MockServer| async move {
        let request = structured(
            "anthropic/claude-opus-5-5",
            vec![ModelMessage::User("Find the name".to_string())],
        )?;

        Router::new([anthropic(&server, "claude-opus-5-5")])
            .execute(request)
            .await
            .err()
            .ok_or_else(|| Box::<dyn Error>::from("the request should fail"))
    };

    // Act
    let overloaded = execute(overloaded).await?;
    let failed = execute(failed).await?;
    let too_long = execute(too_long).await?;
    let billing = execute(billing).await?;

    // Assert
    assert_eq!(overloaded.error_class(), ErrorClass::Provider);
    assert_eq!(overloaded.http_status(), Some(529));
    assert!(overloaded.to_string().contains("overloaded_error"));
    assert!(!conformance::error_chain(&overloaded).contains("test-key"));
    assert_eq!(failed.error_class(), ErrorClass::Provider);
    assert_eq!(failed.http_status(), Some(500));
    assert_eq!(too_long.error_class(), ErrorClass::ContextOverflow);
    assert!(billing.is_quota_exceeded());

    Ok(())
}

#[tokio::test]
async fn rejects_an_option_the_protocol_cannot_express_before_any_request()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let server = server(200, MESSAGE).await;
    let spec =
        custom(&server).with_reasoning(ReasoningMapping::Thinking(ThinkingOptions::default()));
    let request = structured(
        "custom/claude",
        vec![ModelMessage::User("Find the name".to_string())],
    )?;

    // Act
    let error = Router::new([spec]).execute(request).await.err();

    // Assert
    assert!(matches!(
        error,
        Some(ModelError::InvalidProviderConfig { reason })
            if reason.starts_with("the AnthropicMessages protocol cannot express")
    ));
    conformance::assert_no_request(&server).await
}

/// Records the retries a router reports.
#[derive(Default)]
struct RecordedRetries(std::sync::Mutex<Vec<RetryNotice>>);

#[async_trait::async_trait]
impl RetryObserver for RecordedRetries {
    async fn retrying(&self, notice: RetryNotice) {
        if let Ok(mut notices) = self.0.lock() {
            notices.push(notice);
        }
    }
}

#[tokio::test]
async fn retries_an_overloaded_response_under_the_spec_policy_and_reports_it()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_raw(OVERLOADED, "application/json"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MESSAGE, "application/json"))
        .mount(&server)
        .await;
    let policy = RetryPolicy::default()
        .with_retried(RetryKind::Overloaded, true)
        .with_base_delay(Duration::from_millis(1));
    let spec = anthropic(&server, "claude-opus-5-5").with_retry(policy);
    let request = conformance::golden_request(
        "anthropic/claude-opus-5-5",
        true,
        conformance::options(None, false, None),
    )?;
    let observer = Arc::new(RecordedRetries::default());

    // Act
    let completion = Router::new([spec])
        .execute_observed(request, observer.clone())
        .await?;

    // Assert
    assert!(matches!(completion, Completion::Output { .. }));
    let notices = observer
        .0
        .lock()
        .map_err(|_| "the notices lock is poisoned")?
        .clone();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].attempt, 1);
    assert_eq!(notices[0].kind, RetryKind::Overloaded);
    assert_eq!(notices[0].status, Some(529));

    Ok(())
}

/// Returns the server-sent events of `events`, each named by its type.
fn sse(events: &[Value]) -> String {
    events.iter().fold(String::new(), |mut body, event| {
        let kind = event["type"].as_str().unwrap_or_default();
        let _ = write!(body, "event: {kind}\ndata: {event}\n\n");

        body
    })
}

/// Returns the event stream the Messages API sends for `recorded`, an
/// unstreamed response: its envelope in `message_start` with a partial
/// usage, each block started empty and streamed in five-character deltas
/// (a thinking block's signature in one), redacted thinking started whole,
/// then the stop reason and cumulative usage in `message_delta`, with a
/// `ping` among them.
fn event_stream(recorded: &str) -> Result<String, Box<dyn Error>> {
    let message: Value = serde_json::from_str(recorded)?;
    let mut start = message.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["stop_details"] = Value::Null;
    start["usage"]["output_tokens"] = json!(1);
    let mut events = vec![
        json!({"type": "message_start", "message": start}),
        json!({"type": "ping"}),
    ];
    let pieces = |text: &str| -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut pieces: Vec<String> = chars
            .chunks(5)
            .map(|chunk| chunk.iter().collect())
            .collect();
        if pieces.is_empty() {
            pieces.push(String::new());
        }

        pieces
    };
    let content = message["content"].as_array().ok_or("content")?;
    for (index, block) in content.iter().enumerate() {
        let delta =
            |delta: Value| json!({"type": "content_block_delta", "index": index, "delta": delta});
        let (started, deltas) = match block["type"].as_str() {
            Some("text") => (
                json!({"type": "text", "text": ""}),
                pieces(block["text"].as_str().unwrap_or_default())
                    .into_iter()
                    .map(|text| delta(json!({"type": "text_delta", "text": text})))
                    .collect(),
            ),
            Some("thinking") => {
                let mut deltas: Vec<Value> = pieces(block["thinking"].as_str().unwrap_or_default())
                    .into_iter()
                    .map(|thinking| delta(json!({"type": "thinking_delta", "thinking": thinking})))
                    .collect();
                deltas.push(delta(json!({
                    "type": "signature_delta",
                    "signature": block["signature"],
                })));

                (
                    json!({"type": "thinking", "thinking": "", "signature": ""}),
                    deltas,
                )
            }
            Some("tool_use") => (
                json!({"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}),
                pieces(&block["input"].to_string())
                    .into_iter()
                    .map(|json| delta(json!({"type": "input_json_delta", "partial_json": json})))
                    .collect(),
            ),
            _ => (block.clone(), Vec::new()),
        };
        events
            .push(json!({"type": "content_block_start", "index": index, "content_block": started}));
        events.extend(deltas);
        events.push(json!({"type": "content_block_stop", "index": index}));
    }
    events.push(json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": message["stop_reason"],
            "stop_sequence": null,
            "stop_details": message.get("stop_details").cloned().unwrap_or_default(),
        },
        "usage": {"output_tokens": message["usage"]["output_tokens"]},
    }));
    events.push(json!({"type": "message_stop"}));

    Ok(sse(&events))
}

/// Starts a server answering every Messages request with the event stream
/// `body`.
async fn event_stream_server(body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    server
}

/// Records every streamed delta in order.
#[derive(Default)]
struct RecordedDeltas(Mutex<Vec<ResponseDelta>>);

impl RecordedDeltas {
    fn take(&self) -> Result<Vec<ResponseDelta>, Box<dyn Error>> {
        Ok(std::mem::take(
            &mut *self.0.lock().map_err(|_| "the deltas lock is poisoned")?,
        ))
    }
}

#[async_trait::async_trait]
impl DeltaObserver for RecordedDeltas {
    async fn delta(&self, delta: ResponseDelta) {
        if let Ok(mut deltas) = self.0.lock() {
            deltas.push(delta);
        }
    }
}

/// Builds the specification of a case for its server.
type SpecFor = fn(&MockServer) -> ProviderSpec;

/// Returns a completion, or a failure as its display, class, and status, so
/// a streamed and an unstreamed result compare.
type Outcome = Result<Completion, (String, ErrorClass, Option<u16>)>;

fn outcome(result: Result<Completion, ModelError>) -> Outcome {
    result.map_err(|error| (error.to_string(), error.error_class(), error.http_status()))
}

#[tokio::test]
async fn streams_each_recorded_response_into_the_unstreamed_completion()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let forced = |server: &MockServer| {
        custom(server)
            .with_reasoning(ReasoningMapping::None)
            .with_structured_output(StructuredOutputMode::ForcedTool)
    };
    let preset = |server: &MockServer| anthropic(server, "claude-opus-5-5");
    let arguments = |index: usize, arguments: &str| ResponseDelta::ToolArguments {
        arguments: arguments.to_string(),
        index,
    };
    let text = |text: &str| ResponseDelta::Text(text.to_string());
    let reasoning = |text: &str| ResponseDelta::Reasoning(text.to_string());
    let cases: [(&str, SpecFor, Vec<ResponseDelta>); 4] = [
        (
            MESSAGE,
            preset,
            vec![
                reasoning("The f"),
                reasoning("ile n"),
                reasoning("ames "),
                reasoning("Ada."),
                text("{\"nam"),
                text("e\":\"A"),
                text("da\"}"),
            ],
        ),
        (
            TOOL_USE,
            preset,
            vec![
                reasoning("Read "),
                reasoning("both "),
                reasoning("files"),
                reasoning("."),
                text("Readi"),
                text("ng th"),
                text("e fil"),
                text("es."),
                arguments(0, "{\"pat"),
                arguments(0, "h\":\"a"),
                arguments(0, ".txt\""),
                arguments(0, "}"),
                arguments(1, "{\"pat"),
                arguments(1, "h\":\"b"),
                arguments(1, ".txt\""),
                arguments(1, "}"),
            ],
        ),
        (
            FORCED_TOOL,
            forced,
            vec![text("{\"nam"), text("e\":\"A"), text("da\"}")],
        ),
        (REFUSAL, preset, Vec::new()),
    ];

    for (recorded, spec, expected_deltas) in cases {
        let unstreamed = server(200, recorded).await;
        let streamed = event_stream_server(event_stream(recorded)?).await;
        let request = || {
            conformance::golden_request(
                "custom/claude-opus-5-5",
                false,
                conformance::options(None, false, None),
            )
        };
        let build = |spec: ProviderSpec| {
            let id = spec.id().to_string();
            let mut request = request()?;
            request.model = format!("{id}/claude-opus-5-5");

            Ok::<_, Box<dyn Error>>((Router::new([spec]), request))
        };
        let deltas = Arc::new(RecordedDeltas::default());

        // Act
        let (router, request) = build(spec(&unstreamed))?;
        let expected = outcome(router.execute(request).await);
        let (router, request) = build(spec(&streamed))?;
        let completion = outcome(router.execute_streamed(request, deltas.clone(), None).await);

        // Assert
        assert_eq!(completion, expected);
        assert_eq!(deltas.take()?, expected_deltas);
    }

    Ok(())
}

#[tokio::test]
async fn reports_cache_reads_and_writes_from_the_streamed_usage() -> Result<(), Box<dyn Error>> {
    // Arrange
    let server = event_stream_server(event_stream(MESSAGE)?).await;
    let request = structured(
        "anthropic/claude-opus-5-5",
        vec![ModelMessage::User("Find the name".to_string())],
    )?;

    // Act
    let completion = Router::new([anthropic(&server, "claude-opus-5-5")])
        .execute(request)
        .await?;

    // Assert
    let Completion::Output { metadata, .. } = completion else {
        return Err("expected structured output".into());
    };
    let usage = *metadata.usage().ok_or("usage")?;
    assert_eq!(usage.cache_hit_tokens(), Some(1000));
    assert_eq!(usage.cache_write_tokens(), Some(20));
    assert_eq!(usage.input_tokens(), Some(1120));
    assert_eq!(usage.output_tokens(), Some(50));

    Ok(())
}

#[tokio::test]
async fn fails_a_stream_error_event_as_its_unstreamed_rejection() -> Result<(), Box<dyn Error>> {
    // Arrange
    let billing = r#"{"type":"error","error":{"type":"billing_error","message":"Your credit balance is too low."}}"#;
    let unknown = r#"{"type":"error","error":{"type":"novel_error","message":"Try later."}}"#;
    let started = json!({"type": "message_start", "message": {
        "id": "msg_failed", "model": "claude-opus-5-5", "content": [],
        "usage": {"input_tokens": 10, "output_tokens": 1}
    }});
    // Each failure follows streamed content, so even a retried status is
    // not retried.
    let partial = json!({"type": "content_block_start", "index": 0,
        "content_block": {"type": "text", "text": ""}});
    let delta = json!({"type": "content_block_delta", "index": 0,
        "delta": {"type": "text_delta", "text": "{\"na"}});
    let stream = |error: &str| -> Result<String, Box<dyn Error>> {
        Ok(sse(&[
            started.clone(),
            partial.clone(),
            delta.clone(),
            serde_json::from_str(error)?,
        ]))
    };
    let execute = |server: MockServer, retries: Arc<RecordedRetries>| async move {
        let request = structured(
            "anthropic/claude-opus-5-5",
            vec![ModelMessage::User("Find the name".to_string())],
        )?;
        let policy = RetryPolicy::default()
            .with_retried(RetryKind::Overloaded, true)
            .with_base_delay(Duration::from_millis(1));

        Router::new([anthropic(&server, "claude-opus-5-5").with_retry(policy)])
            .execute_observed(request, retries)
            .await
            .err()
            .ok_or_else(|| Box::<dyn Error>::from("the request should fail"))
    };
    let retries = Arc::new(RecordedRetries::default());

    // Act
    let overloaded = execute(
        event_stream_server(stream(OVERLOADED)?).await,
        retries.clone(),
    )
    .await?;
    let too_long = execute(
        event_stream_server(stream(PROMPT_TOO_LONG)?).await,
        retries.clone(),
    )
    .await?;
    let billing = execute(event_stream_server(stream(billing)?).await, retries.clone()).await?;
    let unknown = execute(event_stream_server(stream(unknown)?).await, retries.clone()).await?;

    // Assert
    assert_eq!(overloaded.error_class(), ErrorClass::Provider);
    assert_eq!(overloaded.http_status(), Some(529));
    assert!(overloaded.to_string().contains("overloaded_error"));
    assert!(!overloaded.is_quota_exceeded());
    assert_eq!(too_long.error_class(), ErrorClass::ContextOverflow);
    assert_eq!(too_long.http_status(), Some(400));
    assert!(billing.is_quota_exceeded());
    assert_eq!(billing.http_status(), Some(402));
    assert_eq!(unknown.error_class(), ErrorClass::Provider);
    assert_eq!(unknown.http_status(), None);
    assert!(unknown.to_string().contains("novel_error"));
    assert!(retries.0.lock().map_err(|_| "poisoned")?.is_empty());

    Ok(())
}

#[tokio::test]
async fn retries_a_stream_that_fails_before_any_content_under_the_spec_policy()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let overloaded = MockServer::start().await;
    let failed_early = sse(&[
        json!({"type": "message_start", "message": {"id": "msg_failed", "content": []}}),
        json!({"type": "ping"}),
        serde_json::from_str(OVERLOADED)?,
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(failed_early, "text/event-stream"))
        .up_to_n_times(1)
        .mount(&overloaded)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(event_stream(MESSAGE)?, "text/event-stream"),
        )
        .mount(&overloaded)
        .await;
    let invalid = event_stream_server("data: {\"type\":\n\n".to_string()).await;
    let too_long = event_stream_server(sse(&[
        json!({"type": "message_start", "message": {"id": "msg_failed", "content": []}}),
        serde_json::from_str(PROMPT_TOO_LONG)?,
    ]))
    .await;
    let policy = RetryPolicy::default()
        .with_retried(RetryKind::Overloaded, true)
        .with_base_delay(Duration::from_millis(1));
    let request = || {
        structured(
            "anthropic/claude-opus-5-5",
            vec![ModelMessage::User("Find the name".to_string())],
        )
    };
    let retries = Arc::new(RecordedRetries::default());

    // Act
    let completion = Router::new([anthropic(&overloaded, "claude-opus-5-5").with_retry(policy)])
        .execute_observed(request()?, retries.clone())
        .await?;
    let malformed = Router::new([anthropic(&invalid, "claude-opus-5-5").with_retry(policy)])
        .execute_observed(request()?, retries.clone())
        .await
        .err()
        .ok_or("a malformed stream should fail")?;
    let rejected = Router::new([anthropic(&too_long, "claude-opus-5-5").with_retry(policy)])
        .execute_observed(request()?, retries.clone())
        .await
        .err()
        .ok_or("a rejected request should fail")?;

    // Assert
    assert!(matches!(completion, Completion::Output { .. }));
    let notices = retries
        .0
        .lock()
        .map_err(|_| "the notices lock is poisoned")?
        .clone();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].attempt, 1);
    assert_eq!(notices[0].kind, RetryKind::Overloaded);
    assert_eq!(notices[0].status, Some(529));
    assert_eq!(malformed.error_class(), ErrorClass::InvalidProviderResponse);
    // A status the policy does not retry fails at once, even before content.
    assert_eq!(rejected.error_class(), ErrorClass::ContextOverflow);

    Ok(())
}

#[tokio::test]
async fn fails_a_stream_that_closes_before_message_stop() -> Result<(), Box<dyn Error>> {
    // Arrange
    let recorded = event_stream(MESSAGE)?;
    let cut = recorded
        .rsplit_once("event: message_stop")
        .ok_or("message_stop")?
        .0
        .to_string();
    let server = event_stream_server(cut).await;
    let request = structured(
        "anthropic/claude-opus-5-5",
        vec![ModelMessage::User("Find the name".to_string())],
    )?;

    // Act
    let error = Router::new([anthropic(&server, "claude-opus-5-5")])
        .execute(request)
        .await
        .err()
        .ok_or("the request should fail")?;

    // Assert
    assert_eq!(error.error_class(), ErrorClass::Transport);
    assert!(conformance::error_chain(&error).contains("closed before the response completed"));

    Ok(())
}

#[tokio::test]
async fn marks_cache_breakpoints_only_while_the_policy_asks_for_them() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let marked = server(400, "{}").await;
    let unmarked = server(400, "{}").await;
    let tool = ToolDefinition::new(
        "read",
        "Read one file.",
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    );
    let request = || -> Result<ModelRequest, Box<dyn Error>> {
        Ok(ModelRequest::chat(
            "custom/claude",
            vec![
                ModelMessage::System("You extract names.".to_string()),
                ModelMessage::System("Answer in English.".to_string()),
                ModelMessage::User("Find the name".to_string()),
                ModelMessage::User("in a.txt".to_string()),
            ],
            vec![tool.clone()],
            conformance::format()?,
        ))
    };
    // Forced-tool output appends `structured_output` after the request's
    // tools, so it is the last tool.
    let forced = custom(&marked)
        .with_reasoning(ReasoningMapping::None)
        .with_structured_output(StructuredOutputMode::ForcedTool);
    let chat = ProviderSpec::new(
        "chat",
        WireProtocol::ChatCompletions,
        "https://example.test/v1",
    )
    .with_cache_breakpoints(true);

    // Act
    let _ = Router::new([forced]).execute(request()?).await;
    let _ = Router::new([custom(&unmarked).with_cache_breakpoints(false)])
        .execute(request()?)
        .await;
    let invalid = chat.validate().err();

    // Assert
    let marked = received_body(&marked).await?;
    let breakpoint = json!({"type": "ephemeral"});
    assert_eq!(marked["stream"], json!(true));
    assert_eq!(marked["tools"][0].get("cache_control"), None);
    assert_eq!(marked["tools"][1]["name"], json!("structured_output"));
    assert_eq!(marked["tools"][1]["cache_control"], breakpoint);
    assert_eq!(marked["system"][0].get("cache_control"), None);
    assert_eq!(marked["system"][1]["cache_control"], breakpoint);
    assert_eq!(
        marked["messages"][0]["content"][0].get("cache_control"),
        None
    );
    assert_eq!(
        marked["messages"][0]["content"][1]["cache_control"],
        breakpoint
    );
    assert_eq!(marked.to_string().matches("cache_control").count(), 3);
    let unmarked = received_body(&unmarked).await?;
    assert_eq!(unmarked["stream"], json!(true));
    assert!(!unmarked.to_string().contains("cache_control"));
    assert!(matches!(
        invalid,
        Some(ModelError::InvalidProviderConfig { reason })
            if reason.contains("`cache_control` breakpoints")
    ));

    Ok(())
}
