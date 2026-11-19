//! Conformance of [`WireProtocol::AnthropicMessages`] provider
//! specifications against an in-process server and recorded fixtures:
//! request shape, byte-identical preset requests, response parsing, signed
//! thinking replay, structured-output modes, images, stop reasons, error
//! mapping, and specification validation.

#[path = "support/conformance.rs"]
mod conformance;

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use ag_router::ReasoningEffort::{High, Max, XHigh};
use ag_router::{
    Completion, ErrorClass, ImageContent, ImageMediaType, InputBlock, ModelError, ModelMessage,
    ModelRequest, ProviderSpec, ReasoningMapping, ReasoningView, RetryKind, RetryNotice,
    RetryObserver, RetryPolicy, Router, StructuredOutputMode, ThinkingOptions, ToolDefinition,
    TurnInput, WireProtocol, preset, reasoning_view,
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
                {"type": "tool_result", "tool_use_id": "toolu_2", "content": "{\"text\":\"Ada\"}"}
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
    Router::new([custom(&server)]).execute(request).await?;

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
