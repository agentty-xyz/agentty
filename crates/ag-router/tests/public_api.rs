//! External-consumer checks for the provider-neutral router API.

use std::sync::Arc;

use ag_router::{
    Completion, ErrorClass, ImageContent, ImageMediaType, InputBlock, JsonSchemaFormat, ModelError,
    ModelMessage, ModelRequest, OutputSchema, Provider, ProviderConfig, Router, ToolCall,
    ToolDefinition, TurnInput,
};
use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn schema() -> Result<OutputSchema, ag_router::OutputSchemaError> {
    OutputSchema::new(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
        "additionalProperties": false
    }))
}

fn request(model: &str) -> Result<ModelRequest, Box<dyn std::error::Error>> {
    Ok(ModelRequest::chat(
        model,
        vec![ModelMessage::User("Extract the name".to_string())],
        vec![],
        JsonSchemaFormat::new("person", schema()?)?,
    ))
}

fn router(provider: Provider, server: &MockServer) -> Result<Router, ModelError> {
    Router::new([ProviderConfig {
        provider,
        api_key: "test-key".to_string(),
        base_url: format!("{}/v1/", server.uri()),
    }])
}

fn completion(content: &str) -> Value {
    json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": content}
        }],
        "id": "response-1",
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
    })
}

#[test]
fn shared_image_input_keeps_caller_allocation() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let bytes: Arc<[u8]> = vec![0xFF, 0xD8, 0xFF, 0xE0].into();

    // Act
    let image = ImageContent::from_shared(ImageMediaType::Jpeg, Arc::clone(&bytes))?;
    let cloned = image.clone();

    // Assert
    assert!(std::ptr::eq(image.bytes().as_ptr(), bytes.as_ptr()));
    assert!(std::ptr::eq(cloned.bytes().as_ptr(), bytes.as_ptr()));

    Ok(())
}

#[tokio::test]
async fn routes_muse_with_named_json_schema_and_validated_result()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_partial_json(json!({
            "model": "muse-spark-1.3",
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "person", "schema": schema()?.value()}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion(r#"{"name":"Ada"}"#)))
        .expect(1)
        .mount(&server)
        .await;

    // Act
    let response = router(Provider::Muse, &server)?
        .execute(request("muse/muse-spark-1.3")?)
        .await
        .expect("valid response must complete");

    // Assert
    assert!(
        matches!(response, Completion::Output { value, metadata, .. }
        if value == json!({"name":"Ada"})
        && metadata.response_id() == Some("response-1")
        && metadata.usage().and_then(|usage| usage.total_tokens()) == Some(7))
    );

    Ok(())
}

#[tokio::test]
async fn preserves_slash_qualified_model_in_provider_request()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"model": "team/custom-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion(r#"{"name":"Ada"}"#)))
        .expect(1)
        .mount(&server)
        .await;

    // Act
    let response = router(Provider::Muse, &server)?
        .execute(request("muse/team/custom-model")?)
        .await?;

    // Assert
    assert!(matches!(response, Completion::Output { value, .. } if value == json!({"name":"Ada"})));

    Ok(())
}

#[tokio::test]
async fn routes_qwen_and_validates_local_json_schema() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "qwen-plus",
            "response_format": {"type": "json_object"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion(r#"{"age":42}"#)))
        .expect(1)
        .mount(&server)
        .await;

    // Act
    let error = router(Provider::Qwen, &server)?
        .execute(request("qwen/qwen-plus")?)
        .await
        .expect_err("schema violation must fail locally");

    // Assert
    assert!(matches!(
        error.root_cause(),
        ModelError::SchemaViolation { .. }
    ));
    assert_eq!(
        error
            .metadata()
            .map(ag_router::CompletionMetadata::finish_reason),
        Some("stop")
    );

    Ok(())
}

#[tokio::test]
async fn returns_generic_tool_calls_for_caller_execution() -> Result<(), Box<dyn std::error::Error>>
{
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"id\":7}"}
                }]}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = request("muse/muse-spark-1.3")?;
    if let ag_router::Task::Chat(chat) = &mut request.task {
        chat.tools.push(ToolDefinition::new(
            "lookup",
            "Look up a record",
            json!({"type":"object","properties":{"id":{"type":"integer"}}}),
        ));
    }

    // Act
    let result = router(Provider::Muse, &server)?
        .execute(request)
        .await
        .expect("advertised tool call must decode");

    // Assert
    assert!(matches!(result, Completion::ToolCalls { calls, .. }
        if calls.len() == 1
        && calls[0].name() == "lookup"
        && calls[0].arguments() == Some(&json!({"id":7}))));

    Ok(())
}

#[tokio::test]
async fn returns_malformed_tool_calls_and_replays_them_as_json_objects()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    let raw = r#"{"path src/": "a.rs""#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": raw}
                }]}
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let lookup = ToolDefinition::new("lookup", "Look up a record", json!({"type":"object"}));
    let mut request = request("muse/muse-spark-1.3")?;
    if let ag_router::Task::Chat(chat) = &mut request.task {
        chat.tools.push(lookup);
    }
    let router = router(Provider::Muse, &server)?;

    // Act
    let result = router.execute(request.clone()).await?;
    let Completion::ToolCalls { calls, .. } = result else {
        return Err("expected tool calls".into());
    };
    if let ag_router::Task::Chat(chat) = &mut request.task {
        chat.messages
            .push(ModelMessage::AssistantToolCalls(calls.clone()));
        chat.messages.push(ModelMessage::ToolResult {
            call_id: "call-1".to_string(),
            content: "malformed".to_string(),
            name: "lookup".to_string(),
        });
    }
    router.execute(request).await?;
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    let body: Value = serde_json::from_slice(&requests[1].body)?;
    let replayed = body["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["tool_calls"].is_array())
        })
        .ok_or("replayed tool call missing")?;

    // Assert
    assert_eq!(calls[0].arguments(), None);
    assert_eq!(calls[0].arguments_json(), raw);
    assert!(calls[0].malformed_reason().is_some());
    let wire = replayed["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .ok_or("arguments must be a string")?;
    assert_eq!(
        serde_json::from_str::<Value>(wire)?,
        json!({"malformed_arguments": raw})
    );
    assert!(replayed["content"].is_null());

    Ok(())
}

#[tokio::test]
async fn keeps_assistant_text_beside_tool_calls_and_replays_it_as_content()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let notes = "Notes: src/a.rs defines alpha_one. Next I read src/b.rs.";
    let cases = [
        (Provider::Kimi, "kimi/kimi-k2.6", Some("session-1")),
        (Provider::Muse, "muse/muse-spark-1.3", Some("session-1")),
        (Provider::Qwen, "qwen/qwen3.8-max", None),
    ];
    for (provider, model, sent_cache_key) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {"content": notes, "tool_calls": [
                        {"id": "call-1", "type": "function",
                         "function": {"name": "lookup", "arguments": "{\"id\":1}"}},
                        {"id": "call-2", "type": "function",
                         "function": {"name": "lookup", "arguments": "{\"id\":2}"}}
                    ]}
                }]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let mut request = request(model)?;
        if let ag_router::Task::Chat(chat) = &mut request.task {
            chat.tools.push(ToolDefinition::new(
                "lookup",
                "Look up a record",
                json!({"type":"object"}),
            ));
        }
        request.options.prompt_cache_key = Some("session-1".to_string());
        let router = router(provider, &server)?;

        // Act
        let Completion::ToolCalls { calls, .. } = router.execute(request.clone()).await? else {
            return Err("expected tool calls".into());
        };
        if let ag_router::Task::Chat(chat) = &mut request.task {
            chat.messages
                .push(ModelMessage::AssistantToolCalls(calls.clone()));
            for call in &calls {
                chat.messages.push(ModelMessage::ToolResult {
                    call_id: call.id().to_string(),
                    content: "cleared".to_string(),
                    name: "lookup".to_string(),
                });
            }
        }
        router.execute(request).await?;
        let requests = server
            .received_requests()
            .await
            .ok_or("request recording disabled")?;
        let body: Value = serde_json::from_slice(&requests[1].body)?;
        let replayed = body["messages"]
            .as_array()
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["tool_calls"].is_array())
            })
            .ok_or("replayed tool call missing")?;

        // Assert
        assert_eq!(calls[0].assistant_text(), Some(notes), "{model}");
        assert_eq!(calls[1].assistant_text(), None, "{model}");
        assert_eq!(replayed["content"], notes, "{model}");
        assert_eq!(replayed["tool_calls"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            body.get("prompt_cache_key").and_then(Value::as_str),
            sent_cache_key,
            "{model}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn rejects_unknown_provider_before_network() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    let router = router(Provider::Muse, &server)?;

    // Act
    let error = router
        .execute(request("qwen/qwen-plus")?)
        .await
        .expect_err("unconfigured provider must fail");

    // Assert
    assert!(matches!(error, ModelError::UnknownProvider));
    assert!(
        server
            .received_requests()
            .await
            .expect("requests recorded")
            .is_empty()
    );

    Ok(())
}

#[tokio::test]
async fn replays_image_and_batched_function_history_for_kimi()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion(r#"{"name":"Ada"}"#)))
        .expect(2)
        .mount(&server)
        .await;
    let image = ImageContent::new(
        ImageMediaType::Jpeg,
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x22, 0x22],
    )?;
    let input = TurnInput::new(vec![
        InputBlock::Text("Read this image".to_string()),
        InputBlock::Image(image),
    ])?;
    let call = ToolCall::from_json(
        "call-1".to_string(),
        "lookup",
        "{}",
        Some("provider thought".to_string()),
    )?;
    let mut request = ModelRequest::chat(
        "kimi/kimi-k2.6",
        vec![
            ModelMessage::UserInput(input),
            ModelMessage::AssistantToolCalls(vec![call]),
            ModelMessage::ToolResult {
                call_id: "call-1".to_string(),
                content: "done".to_string(),
                name: "lookup".to_string(),
            },
        ],
        vec![ToolDefinition::new(
            "lookup",
            "Look up a record",
            json!({"type":"object"}),
        )],
        JsonSchemaFormat::new("person", schema()?)?,
    );
    request.options.reasoning_effort = Some(ag_router::ReasoningEffort::High);

    // Act
    let router = router(Provider::Kimi, &server)?;
    let response = router.execute(request.clone()).await?;
    request.options.reasoning_effort = Some(ag_router::ReasoningEffort::Low);
    let low_response = router.execute(request).await?;
    let requests = server
        .received_requests()
        .await
        .ok_or("request recording disabled")?;
    let body: Value = serde_json::from_slice(&requests[0].body)?;

    // Assert
    assert!(matches!(response, Completion::Output { value, .. } if value == json!({"name":"Ada"})));
    assert!(matches!(low_response, Completion::Output { .. }));
    assert_eq!(body["model"], "kimi-k2.6");
    assert_eq!(
        body["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/jpeg;base64,/9j/4CIi"
    );
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(body["messages"][2]["reasoning_content"], "provider thought");
    assert_eq!(body["messages"][3]["tool_call_id"], "call-1");
    assert_eq!(body["thinking"]["type"], "enabled");
    let low_body: Value = serde_json::from_slice(&requests[1].body)?;
    assert_eq!(low_body["thinking"]["type"], "disabled");

    Ok(())
}

#[tokio::test]
async fn sends_prompt_cache_key_only_to_providers_that_route_by_it()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let cases = [
        (
            "muse/muse-spark-1.3",
            Provider::Muse,
            Some("session-key"),
            true,
        ),
        ("muse/muse-spark-1.3", Provider::Muse, None, false),
        ("kimi/kimi-k3", Provider::Kimi, Some("session-key"), true),
        (
            "qwen/qwen3.8-max",
            Provider::Qwen,
            Some("session-key"),
            false,
        ),
    ];

    for (model, provider, key, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(completion(r#"{"name":"Ada"}"#)))
            .expect(1)
            .mount(&server)
            .await;
        let mut request = request(model)?;
        request.options.prompt_cache_key = key.map(str::to_string);

        // Act
        router(provider, &server)?.execute(request).await?;
        let requests = server
            .received_requests()
            .await
            .ok_or("request recording disabled")?;
        let body: Value = serde_json::from_slice(&requests[0].body)?;

        // Assert
        assert_eq!(
            body.get("prompt_cache_key"),
            expected.then(|| json!("session-key")).as_ref(),
            "{model} with key {key:?}"
        );
    }

    Ok(())
}

/// Returns the router error for one request answered with `status` and
/// `body`.
async fn failure(
    provider: Provider,
    model: &str,
    status: u16,
    body: &str,
) -> Result<ModelError, Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(status).set_body_raw(body.to_string(), "application/json"),
        )
        .mount(&server)
        .await;

    match router(provider, &server)?.execute(request(model)?).await {
        Ok(_) => Err("unsuccessful response must fail".into()),
        Err(error) => Ok(error),
    }
}

#[tokio::test]
async fn classifies_live_context_overflow_bodies_per_provider()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let cases = [
        (
            Provider::Kimi,
            "kimi/kimi-k2.6",
            include_str!("fixtures/context_overflow/kimi-k2.6.json"),
        ),
        (
            Provider::Muse,
            "muse/muse-spark-1.3",
            include_str!("fixtures/context_overflow/muse-spark-1.3.json"),
        ),
        (
            Provider::Qwen,
            "qwen/qwen3.8-max",
            include_str!("fixtures/context_overflow/qwen3.8-max.json"),
        ),
        (
            Provider::Qwen,
            "qwen/qwen-plus",
            include_str!("fixtures/context_overflow/qwen-plus.json"),
        ),
    ];

    for (provider, model, body) in cases {
        // Act
        let error = failure(provider, model, 400, body).await?;

        // Assert
        assert_eq!(error.error_class(), ErrorClass::ContextOverflow, "{model}");
        assert!(error.is_context_overflow(), "{model}");
        assert_eq!(error.http_status(), Some(400), "{model}");
    }

    Ok(())
}

#[tokio::test]
async fn classifies_payload_too_large_and_shared_markers_as_context_overflow()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let openai_overflow = r#"{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 8192 tokens."}}"#;

    // Act
    let too_large = failure(Provider::Muse, "muse/muse-spark-1.3", 413, "<html>").await?;
    let shared_marker = failure(Provider::Qwen, "qwen/qwen-plus", 422, openai_overflow).await?;

    // Assert
    assert!(too_large.is_context_overflow());
    assert_eq!(too_large.http_status(), Some(413));
    assert!(shared_marker.is_context_overflow());

    Ok(())
}

#[tokio::test]
async fn keeps_other_provider_failures_out_of_context_overflow()
-> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let kimi_overflow = include_str!("fixtures/context_overflow/kimi-k2.6.json");
    let muse_validation = r#"{"error":{"code":null,"message":"unknown parameter `frobnicate`","param":"frobnicate","type":"invalid_request_error"}}"#;

    // Act
    let failures = [
        failure(Provider::Muse, "muse/muse-spark-1.3", 400, muse_validation).await?,
        failure(Provider::Muse, "muse/muse-spark-1.3", 400, kimi_overflow).await?,
        failure(Provider::Kimi, "kimi/kimi-k2.6", 500, kimi_overflow).await?,
    ];

    // Assert
    for error in failures {
        assert_eq!(error.error_class(), ErrorClass::Provider, "{error}");
        assert!(!error.is_context_overflow(), "{error}");
    }

    Ok(())
}
