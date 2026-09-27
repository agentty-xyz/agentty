//! External-consumer checks for the provider-neutral router API.

use std::sync::Arc;

use ag_router::{
    Completion, ImageContent, ImageMediaType, InputBlock, JsonSchemaFormat, ModelError,
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
        && calls[0].arguments() == &json!({"id":7})));

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
