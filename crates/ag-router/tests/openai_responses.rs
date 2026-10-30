//! External-consumer checks for `OpenAI` models routed through the
//! Responses API: stateless streamed requests, parallel calls, reasoning
//! replay, usage, and failure classification.

use std::fmt::Write as _;

use ag_router::{
    Completion, ErrorClass, ImageContent, ImageMediaType, InputBlock, JsonSchemaFormat, ModelError,
    ModelMessage, ModelRequest, OutputSchema, Provider, ProviderConfig, ReasoningEffort, Router,
    ToolCall, ToolDefinition, TurnInput,
};
use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Message the Responses API gives an input over the window, as recorded in
/// Codex CLI's own stream fixtures.
const CONTEXT_OVERFLOW_MESSAGE: &str =
    "Your input exceeds the context window of this model. Please adjust your input and try again.";

fn schema() -> Result<OutputSchema, ag_router::OutputSchemaError> {
    OutputSchema::new(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
        "additionalProperties": false
    }))
}

fn read_tool() -> ToolDefinition {
    ToolDefinition::new(
        "read",
        "Read one file.",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
    )
}

fn request(
    model: &str,
    messages: Vec<ModelMessage>,
) -> Result<ModelRequest, Box<dyn std::error::Error>> {
    Ok(ModelRequest::chat(
        model,
        messages,
        vec![read_tool()],
        JsonSchemaFormat::new("person", schema()?)?,
    ))
}

fn user_request(model: &str) -> Result<ModelRequest, Box<dyn std::error::Error>> {
    request(
        model,
        vec![ModelMessage::User("Extract the name".to_string())],
    )
}

fn router(server: &MockServer) -> Result<Router, ModelError> {
    Router::new([ProviderConfig {
        provider: Provider::OpenAi,
        api_key: Some("sk-test".to_string()),
        base_url: format!("{}/v1/", server.uri()),
    }])
}

/// Serializes `events` as a server-sent event stream.
fn stream(events: &[Value]) -> String {
    events.iter().fold(String::new(), |mut stream, event| {
        let _ = write!(
            stream,
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap_or("")
        );

        stream
    })
}

fn item_done(item: Value) -> Value {
    let mut event = json!({"type": "response.output_item.done"});
    event["item"] = item;

    event
}

fn completed(usage: Value) -> Value {
    let mut event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_1",
            "model": "gpt-6.1-sol-2026-09-01",
            "output": [],
            "status": "completed"
        }
    });
    event["response"]["usage"] = usage;

    event
}

fn reasoning_item(id: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": id,
        "summary": [],
        "encrypted_content": format!("gAAAA-{id}")
    })
}

fn message_item(text: &str) -> Value {
    json!({
        "type": "message",
        "id": "msg_1",
        "role": "assistant",
        "status": "completed",
        "content": [{"type": "output_text", "text": text, "annotations": []}]
    })
}

fn function_call_item(call_id: &str, path: &str) -> Value {
    json!({
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": "read",
        "arguments": json!({"path": path}).to_string(),
        "status": "completed"
    })
}

async fn mount_stream(server: &MockServer, events: &[Value]) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(bearer_token("sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(stream(events), "text/event-stream"))
        .mount(server)
        .await;
}

/// Returns the JSON body of the `index`th request `server` received.
async fn received_body(
    server: &MockServer,
    index: usize,
) -> Result<Value, Box<dyn std::error::Error>> {
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    let request = requests.get(index).ok_or("request missing")?;

    Ok(serde_json::from_slice(&request.body)?)
}

/// Routes `request` to a server streaming `events` and returns the failure.
async fn streamed_failure(
    events: &[Value],
    request: ModelRequest,
) -> Result<ModelError, Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    mount_stream(&server, events).await;

    match router(&server)?.execute(request).await {
        Ok(_) => Err("the response must fail".into()),
        Err(error) => Ok(error),
    }
}

#[tokio::test]
async fn streams_a_stateless_request_and_validates_the_output()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    let reasoning = reasoning_item("rs_1");
    mount_stream(
        &server,
        &[
            json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}}),
            json!({"type": "response.output_text.delta", "delta": "{\"na"}),
            item_done(reasoning.clone()),
            item_done(message_item(r#"{"name":"Ada"}"#)),
            completed(json!({
                "input_tokens": 1200,
                "input_tokens_details": {"cached_tokens": 1024},
                "output_tokens": 90,
                "output_tokens_details": {"reasoning_tokens": 64},
                "total_tokens": 1290
            })),
        ],
    )
    .await;
    let mut request = request(
        "openai/gpt-6.1-sol",
        vec![
            ModelMessage::System("You are terse.".to_string()),
            ModelMessage::User("Extract the name".to_string()),
        ],
    )?;
    request.options.prompt_cache_key = Some("ag-session-1".to_string());
    request.options.reasoning_effort = Some(ReasoningEffort::Max);

    // Act
    let completion = router(&server)?.execute(request).await?;

    // Assert
    let Completion::Output {
        value,
        metadata,
        reasoning_content,
    } = completion
    else {
        return Err("terminal output expected".into());
    };
    assert_eq!(value, json!({"name": "Ada"}));
    assert_eq!(metadata.finish_reason(), "stop");
    assert_eq!(metadata.response_id(), Some("resp_1"));
    assert_eq!(metadata.response_model(), Some("gpt-6.1-sol-2026-09-01"));
    let usage = metadata.usage().ok_or("usage expected")?;
    assert_eq!(usage.input_tokens(), Some(1200));
    assert_eq!(usage.cache_hit_tokens(), Some(1024));
    assert_eq!(usage.cache_miss_tokens(), None);
    assert_eq!(usage.output_tokens(), Some(90));
    assert_eq!(usage.reasoning_tokens(), Some(64));
    assert_eq!(usage.total_tokens(), Some(1290));
    let replay: Value = serde_json::from_str(&reasoning_content.ok_or("reasoning expected")?)?;
    assert_eq!(replay, json!([reasoning]));
    assert_eq!(
        received_body(&server, 0).await?,
        json!({
            "include": ["reasoning.encrypted_content"],
            "input": [
                {"type": "message", "role": "developer", "content": "You are terse."},
                {"type": "message", "role": "user", "content": "Extract the name"}
            ],
            "model": "gpt-6.1-sol",
            "parallel_tool_calls": true,
            "prompt_cache_key": "ag-session-1",
            "reasoning": {"effort": "max"},
            "store": false,
            "stream": true,
            "text": {"format": {
                "type": "json_schema",
                "name": "person",
                "schema": schema()?.value(),
                "strict": false
            }},
            "tools": [{
                "type": "function",
                "name": "read",
                "description": "Read one file.",
                "parameters": read_tool().parameters(),
                "strict": false
            }]
        })
    );

    Ok(())
}

#[tokio::test]
async fn decodes_parallel_calls_and_replays_them_after_their_reasoning()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    let reasoning = reasoning_item("rs_calls");
    mount_stream(
        &server,
        &[
            item_done(reasoning.clone()),
            item_done(message_item("Reading both manifests.")),
            item_done(function_call_item("call_a", "Cargo.toml")),
            item_done(function_call_item("call_b", "crates/ag-router/Cargo.toml")),
            completed(json!({"input_tokens": 10, "output_tokens": 5, "total_tokens": 15})),
        ],
    )
    .await;
    let router = router(&server)?;

    // Act
    let completion = router.execute(user_request("openai/gpt-6.1-sol")?).await?;
    let Completion::ToolCalls { calls, metadata } = completion else {
        return Err("tool calls expected".into());
    };
    let mut history = vec![
        ModelMessage::User("Extract the name".to_string()),
        ModelMessage::AssistantToolCalls(calls.clone()),
    ];
    for call in &calls {
        history.push(ModelMessage::ToolResult {
            call_id: call.id().to_string(),
            content: format!("contents of {}", call.arguments_json()),
            name: call.name().to_string(),
        });
    }
    let _ = router
        .execute(request("openai/gpt-6.1-sol", history)?)
        .await;

    // Assert
    assert_eq!(metadata.finish_reason(), "tool_calls");
    assert_eq!(
        calls.iter().map(ToolCall::id).collect::<Vec<_>>(),
        ["call_a", "call_b"]
    );
    assert_eq!(calls[0].assistant_text(), Some("Reading both manifests."));
    assert!(calls[0].reasoning_content().is_some());
    assert_eq!(calls[1].assistant_text(), None);
    assert_eq!(calls[1].reasoning_content(), None);
    assert_eq!(
        calls[1].arguments(),
        Some(&json!({"path": "crates/ag-router/Cargo.toml"}))
    );
    assert_eq!(
        received_body(&server, 1).await?["input"],
        json!([
            {"type": "message", "role": "user", "content": "Extract the name"},
            reasoning,
            {"type": "message", "role": "assistant", "content": "Reading both manifests."},
            {
                "type": "function_call",
                "call_id": "call_a",
                "name": "read",
                "arguments": r#"{"path":"Cargo.toml"}"#
            },
            {
                "type": "function_call",
                "call_id": "call_b",
                "name": "read",
                "arguments": r#"{"path":"crates/ag-router/Cargo.toml"}"#
            },
            {
                "type": "function_call_output",
                "call_id": "call_a",
                "output": r#"contents of {"path":"Cargo.toml"}"#
            },
            {
                "type": "function_call_output",
                "call_id": "call_b",
                "output": r#"contents of {"path":"crates/ag-router/Cargo.toml"}"#
            }
        ])
    );

    Ok(())
}

#[tokio::test]
async fn replays_stored_reasoning_and_omits_cleared_or_foreign_reasoning()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    mount_stream(
        &server,
        &[
            item_done(message_item(r#"{"name":"Ada"}"#)),
            completed(json!(null)),
        ],
    )
    .await;
    let stored = reasoning_item("rs_output");
    let image = ImageContent::new(
        ImageMediaType::Png,
        vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
    )?;
    let cleared_call = ToolCall::from_json(
        "call_cleared".to_string(),
        "read",
        r#"{"path":"README.md"}"#,
        Some("[reasoning cleared to fit the context window]".to_string()),
    )?;
    let history = vec![
        ModelMessage::User("first".to_string()),
        ModelMessage::AssistantReasoning {
            content: r#"{"name":"Bo"}"#.to_string(),
            reasoning_content: json!([stored]).to_string(),
        },
        ModelMessage::AssistantReasoning {
            content: r#"{"name":"Cy"}"#.to_string(),
            reasoning_content: "Kimi reasoning text".to_string(),
        },
        ModelMessage::AssistantToolCall(cleared_call),
        ModelMessage::ToolResult {
            call_id: "call_cleared".to_string(),
            content: "# README".to_string(),
            name: "read".to_string(),
        },
        ModelMessage::Assistant(r#"{"name":"Di"}"#.to_string()),
        ModelMessage::UserInput(TurnInput::new(vec![
            InputBlock::Text("Who is pictured?".to_string()),
            InputBlock::Image(image),
        ])?),
    ];

    // Act
    let completion = router(&server)?
        .execute(request("openai/gpt-5.3-codex", history)?)
        .await?;

    // Assert
    let Completion::Output {
        metadata,
        reasoning_content,
        ..
    } = completion
    else {
        return Err("terminal output expected".into());
    };
    assert_eq!(reasoning_content, None);
    assert_eq!(metadata.usage(), None);
    let input = &received_body(&server, 0).await?["input"];
    assert_eq!(
        input,
        &json!([
            {"type": "message", "role": "user", "content": "first"},
            stored,
            {"type": "message", "role": "assistant", "content": r#"{"name":"Bo"}"#},
            {"type": "message", "role": "assistant", "content": r#"{"name":"Cy"}"#},
            {
                "type": "function_call",
                "call_id": "call_cleared",
                "name": "read",
                "arguments": r#"{"path":"README.md"}"#
            },
            {"type": "function_call_output", "call_id": "call_cleared", "output": "# README"},
            {"type": "message", "role": "assistant", "content": r#"{"name":"Di"}"#},
            {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "Who is pictured?"},
                    {"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="}
                ]
            }
        ])
    );

    Ok(())
}

#[tokio::test]
async fn sends_xhigh_for_max_effort_to_models_without_max() -> Result<(), Box<dyn std::error::Error>>
{
    // Arrange
    let server = MockServer::start().await;
    mount_stream(
        &server,
        &[
            item_done(message_item(r#"{"name":"Ada"}"#)),
            completed(json!(null)),
        ],
    )
    .await;
    let mut request = user_request("openai/gpt-5.3-codex")?;
    request.options.reasoning_effort = Some(ReasoningEffort::Max);

    // Act
    router(&server)?.execute(request).await?;

    // Assert
    assert_eq!(
        received_body(&server, 0).await?["reasoning"],
        json!({"effort": "xhigh"})
    );

    Ok(())
}

#[tokio::test]
async fn decodes_the_terminal_output_when_no_item_events_arrive()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    mount_stream(
        &server,
        &[json!({
            "type": "response.completed",
            "response": {
                "id": "resp_2",
                "status": "completed",
                "output": [
                    {"type": "item_reference", "id": "msg_0"},
                    {"type": "message", "role": "assistant", "content": [{"type": "output_image"}]},
                    function_call_item("call_only", "Cargo.toml")
                ]
            }
        })],
    )
    .await;

    // Act
    let completion = router(&server)?
        .execute(user_request("openai/gpt-6-luna")?)
        .await?;

    // Assert
    let Completion::ToolCalls { calls, .. } = completion else {
        return Err("tool call expected".into());
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id(), "call_only");
    assert_eq!(calls[0].reasoning_content(), None);
    assert_eq!(calls[0].assistant_text(), None);

    Ok(())
}

#[tokio::test]
async fn classifies_context_overflow_before_and_after_the_stream_starts()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(400).set_body_raw(
                json!({"error": {
                    "message": CONTEXT_OVERFLOW_MESSAGE,
                    "type": "invalid_request_error",
                    "param": "input",
                    "code": "context_length_exceeded"
                }})
                .to_string(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;
    let streamed_message_only = [json!({
        "type": "response.failed",
        "response": {
            "id": "resp_overflow",
            "status": "failed",
            "error": {"code": "server_error", "message": CONTEXT_OVERFLOW_MESSAGE}
        }
    })];
    let streamed_code_only = [json!({
        "type": "response.failed",
        "response": {"status": "failed", "error": {"code": "context_length_exceeded"}}
    })];

    // Act
    let rejected = router(&server)?
        .execute(user_request("openai/gpt-6.1-sol")?)
        .await
        .err()
        .ok_or("an HTTP 400 must fail")?;
    let streamed =
        streamed_failure(&streamed_message_only, user_request("openai/gpt-6.1-sol")?).await?;
    let coded = streamed_failure(&streamed_code_only, user_request("openai/gpt-6.1-sol")?).await?;

    // Assert
    assert!(rejected.is_context_overflow());
    assert_eq!(rejected.http_status(), Some(400));
    assert!(streamed.is_context_overflow());
    assert_eq!(streamed.http_status(), None);
    assert_eq!(
        streamed.metadata().map(|metadata| metadata.response_id()),
        Some(Some("resp_overflow"))
    );
    assert!(
        streamed
            .to_string()
            .contains("OpenAI response failed with `server_error`")
    );
    assert!(coded.is_context_overflow());

    Ok(())
}

#[tokio::test]
async fn classifies_other_stream_failures_as_provider_errors()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let rate_limited = [json!({
        "type": "response.failed",
        "response": {
            "status": "failed",
            "error": {"code": "rate_limit_exceeded", "message": "Rate limit reached for gpt-6.1-sol"}
        }
    })];
    let error_event = [json!({"type": "error", "code": "server_error", "message": "try again"})];
    let nested_error_event = [json!({"type": "error", "error": {"message": "boom"}})];

    // Act
    let rate_limited = streamed_failure(&rate_limited, user_request("openai/gpt-6.1-sol")?).await?;
    let error_event = streamed_failure(&error_event, user_request("openai/gpt-6.1-sol")?).await?;
    let nested = streamed_failure(&nested_error_event, user_request("openai/gpt-6.1-sol")?).await?;

    // Assert
    assert_eq!(rate_limited.error_class(), ErrorClass::Provider);
    assert!(!rate_limited.is_context_overflow());
    assert_eq!(error_event.error_class(), ErrorClass::Provider);
    assert!(
        error_event
            .to_string()
            .contains("`server_error`: try again")
    );
    assert!(nested.to_string().contains("`unknown`: boom"));

    Ok(())
}

#[tokio::test]
async fn reports_incomplete_refused_and_empty_responses() -> Result<(), Box<dyn std::error::Error>>
{
    // Arrange
    let incomplete = [json!({
        "type": "response.incomplete",
        "response": {
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "usage": {"input_tokens": 5, "output_tokens": 128_000}
        }
    })];
    let unknown_status = [json!({"type": "response.completed", "response": {"status": "queued"}})];
    let refused = [
        item_done(json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "refusal", "refusal": "I can't help with that."}]
        })),
        completed(json!(null)),
    ];
    let empty = [completed(json!(null))];

    // Act
    let incomplete = streamed_failure(&incomplete, user_request("openai/gpt-6.1-sol")?).await?;
    let unknown_status =
        streamed_failure(&unknown_status, user_request("openai/gpt-6.1-sol")?).await?;
    let refused = streamed_failure(&refused, user_request("openai/gpt-6.1-sol")?).await?;
    let empty = streamed_failure(&empty, user_request("openai/gpt-6.1-sol")?).await?;

    // Assert
    assert!(matches!(
        incomplete.root_cause(),
        ModelError::IncompleteResponse { reason } if reason == "max_output_tokens"
    ));
    assert_eq!(
        incomplete
            .metadata()
            .and_then(|metadata| metadata.usage())
            .and_then(|usage| usage.output_tokens()),
        Some(128_000)
    );
    assert!(matches!(
        unknown_status.root_cause(),
        ModelError::IncompleteResponse { reason } if reason == "queued"
    ));
    assert!(matches!(
        refused.root_cause(),
        ModelError::IncompleteResponse { reason }
            if reason == "refusal: I can't help with that."
    ));
    assert!(matches!(empty.root_cause(), ModelError::InvalidResponse));

    Ok(())
}

#[tokio::test]
async fn rejects_unusable_function_calls() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let mut unadvertised = function_call_item("call_x", "Cargo.toml");
    unadvertised["name"] = json!("delete");
    let mut missing_call_id = function_call_item("call_y", "Cargo.toml");
    missing_call_id
        .as_object_mut()
        .ok_or("object expected")?
        .remove("call_id");
    let cases = [
        (
            vec![item_done(unadvertised)],
            ErrorClass::InvalidToolCall,
            "unsupported tool: delete",
        ),
        (
            vec![item_done(missing_call_id)],
            ErrorClass::InvalidToolCall,
            "invalid tool arguments",
        ),
        (
            vec![
                item_done(function_call_item("call_z", "a")),
                item_done(function_call_item("call_z", "b")),
            ],
            ErrorClass::InvalidToolCall,
            "duplicate tool call identifier: call_z",
        ),
        (
            vec![
                item_done(json!({"type": "web_search_call", "id": "ws_1", "status": "completed"})),
                item_done(function_call_item("call_w", "a")),
            ],
            ErrorClass::InvalidToolCall,
            "unsupported tool type: web_search_call",
        ),
    ];

    for (mut events, class, message) in cases {
        events.push(completed(json!(null)));

        // Act
        let error = streamed_failure(&events, user_request("openai/gpt-6.1-sol")?).await?;

        // Assert
        assert_eq!(error.error_class(), class, "{message}");
        assert!(error.to_string().contains(message), "{error}");
    }

    Ok(())
}

#[tokio::test]
async fn reports_a_stream_closed_before_completion_as_a_transport_failure()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let events = [item_done(message_item(r#"{"name":"Ada"}"#))];

    // Act
    let error = streamed_failure(&events, user_request("openai/gpt-6.1-sol")?).await?;

    // Assert
    assert_eq!(error.error_class(), ErrorClass::Transport);
    assert!(
        error
            .to_string()
            .contains("the response stream closed before the response completed")
    );

    Ok(())
}
