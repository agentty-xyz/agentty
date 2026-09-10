use super::support::{max_as_xhigh, native_schema_backend};
use crate::chat_completion::{
    ChatCompletionBackend, ChatCompletionProviderPolicy, ReasoningFormat, StructuredOutputMode,
    default_client, endpoint,
};
use crate::{model, schema_contract, tool};

#[test]
fn builds_endpoint_from_base_url() {
    // Arrange and Act
    let endpoint = endpoint("https://example.com/v1///");

    // Assert
    assert_eq!(endpoint, "https://example.com/v1/chat/completions");
}

#[test]
fn maps_reasoning_effort_to_each_provider_wire_format() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let backend = |reasoning_format| {
        ChatCompletionBackend::with_client(
            "test-key".to_string(),
            "https://example.com/v1".to_string(),
            "model".to_string(),
            ChatCompletionProviderPolicy {
                display_name: "Provider",
                reasoning_format,
                response_format_with_tools: true,
                structured_output: StructuredOutputMode::JsonSchema,
                telemetry_name: "provider",
                unsupported_schema_reason: "object schema required",
            },
            default_client(),
        )
    };
    let request = |reasoning_effort| {
        model::ModelRequest::new("hello", schema.clone())
            .with_model_reasoning_effort(reasoning_effort)
    };

    // Act
    let effort_backend = backend(ReasoningFormat::Effort(max_as_xhigh));
    let low_request = request(model::ReasoningEffort::Low);
    let maximum_request = request(model::ReasoningEffort::Max);
    let medium_request = request(model::ReasoningEffort::Medium);
    let kimi_backend = backend(crate::provider::kimi_policy("kimi-k2.6").reasoning_format);
    let kimi_k2_7_backend =
        backend(crate::provider::kimi_policy("kimi-k2.7-code").reasoning_format);
    let kimi_k3_backend = backend(crate::provider::kimi_policy("kimi-k3").reasoning_format);
    let high_request = request(model::ReasoningEffort::High);
    let xhigh_request = request(model::ReasoningEffort::XHigh);
    let qwen_backend = backend(crate::provider::qwen_policy("qwen-plus").reasoning_format);
    let qwen_3_8_backend = backend(crate::provider::qwen_policy("qwen3.8-max").reasoning_format);

    // Assert
    assert_eq!(effort_backend.reasoning_effort(&low_request), Some("low"));
    assert_eq!(
        effort_backend.reasoning_effort(&maximum_request),
        Some("xhigh")
    );
    assert_eq!(effort_backend.enable_thinking(&low_request), None);
    assert!(effort_backend.thinking(&low_request).is_none());
    assert_eq!(
        kimi_backend
            .thinking(&low_request)
            .map(|thinking| thinking.kind),
        Some("disabled")
    );
    assert_eq!(
        kimi_backend
            .thinking(&high_request)
            .map(|thinking| thinking.kind),
        Some("enabled")
    );
    assert_eq!(
        kimi_k2_7_backend
            .thinking(&low_request)
            .map(|thinking| thinking.kind),
        Some("enabled")
    );
    assert_eq!(kimi_k2_7_backend.reasoning_effort(&low_request), None);
    assert!(kimi_k3_backend.thinking(&low_request).is_none());
    assert_eq!(kimi_k3_backend.reasoning_effort(&low_request), Some("low"));
    assert_eq!(
        kimi_k3_backend.reasoning_effort(&medium_request),
        Some("high")
    );
    assert_eq!(
        kimi_k3_backend.reasoning_effort(&high_request),
        Some("high")
    );
    assert_eq!(
        kimi_k3_backend.reasoning_effort(&xhigh_request),
        Some("max")
    );
    assert_eq!(
        kimi_k3_backend.reasoning_effort(&maximum_request),
        Some("max")
    );
    assert_eq!(qwen_backend.enable_thinking(&low_request), Some(false));
    assert_eq!(qwen_backend.enable_thinking(&high_request), Some(true));
    assert_eq!(qwen_backend.reasoning_effort(&high_request), None);
    assert_eq!(qwen_3_8_backend.enable_thinking(&low_request), None);
    assert_eq!(qwen_3_8_backend.reasoning_effort(&low_request), Some("low"));
    assert_eq!(
        qwen_3_8_backend.reasoning_effort(&medium_request),
        Some("medium")
    );
    assert_eq!(
        qwen_3_8_backend.reasoning_effort(&high_request),
        Some("xhigh")
    );
    assert_eq!(
        qwen_3_8_backend.reasoning_effort(&maximum_request),
        Some("xhigh")
    );
}

#[test]
fn preserves_k2_6_reasoning_only_while_thinking_is_enabled() {
    // Arrange
    let backend = ChatCompletionBackend::with_client(
        "test-key".to_string(),
        "https://example.com/v1".to_string(),
        "kimi-k2.6".to_string(),
        crate::provider::kimi_policy("kimi-k2.6"),
        default_client(),
    );
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let default_request = model::ModelRequest::new("hello", schema.clone());
    let low_request = model::ModelRequest::new("hello", schema.clone())
        .with_model_reasoning_effort(model::ReasoningEffort::Low);
    let high_request = model::ModelRequest::new("hello", schema)
        .with_model_reasoning_effort(model::ReasoningEffort::High);

    // Act
    let default_thinking = backend
        .thinking(&default_request)
        .expect("K2.6 defaults should preserve thinking");
    let low_thinking = backend
        .thinking(&low_request)
        .expect("explicit low effort should disable K2.6 thinking");
    let high_thinking = backend
        .thinking(&high_request)
        .expect("explicit high effort should preserve K2.6 thinking");

    // Assert
    assert_eq!(default_thinking.kind, "enabled");
    assert_eq!(default_thinking.keep, Some("all"));
    assert_eq!(low_thinking.kind, "disabled");
    assert_eq!(low_thinking.keep, None);
    assert_eq!(high_thinking.kind, "enabled");
    assert_eq!(high_thinking.keep, Some("all"));
}

#[test]
fn serializes_tool_history_for_native_json_schema_provider() {
    // Arrange
    let backend = native_schema_backend();
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let arguments = serde_json::from_value(serde_json::json!({
        "path": "Cargo.toml"
    }))
    .expect("read arguments should be valid");
    let mut request = model::ModelRequest::new("inspect the manifest", schema);
    request.record_tool_result(
        tool::ToolCall::read("call_read".to_string(), arguments, None),
        "result".to_string(),
    );

    // Act
    let messages = serde_json::to_value(
        backend
            .messages(&request)
            .expect("tool history should serialize"),
    )
    .expect("messages should encode as JSON");

    // Assert
    assert_eq!(
        messages,
        serde_json::json!([
            {"content": "inspect the manifest", "role": "user"},
            {
                "content": null,
                "role": "assistant",
                "tool_calls": [{
                    "function": {
                        "arguments": r#"{"path":"Cargo.toml"}"#,
                        "name": "read"
                    },
                    "id": "call_read",
                    "type": "function"
                }]
            },
            {
                "content": "result",
                "role": "tool",
                "tool_call_id": "call_read"
            }
        ])
    );
}

#[test]
fn serializes_batched_tool_history_for_native_json_schema_provider() {
    // Arrange
    let backend = native_schema_backend();
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let calls = [
        ("call_manifest", "Cargo.toml"),
        ("call_readme", "README.md"),
    ]
    .into_iter()
    .map(|(id, path)| {
        let arguments = serde_json::from_value(serde_json::json!({ "path": path }))
            .expect("read arguments should be valid");

        tool::ToolCall::read(id.to_string(), arguments, None)
    })
    .collect();
    let mut request = model::ModelRequest::new("inspect both files", schema);
    request.record_tool_results(
        calls,
        vec!["manifest result".to_string(), "readme result".to_string()],
    );

    // Act
    let messages = serde_json::to_value(
        backend
            .messages(&request)
            .expect("batched tool history should serialize"),
    )
    .expect("messages should encode as JSON");

    // Assert
    assert_eq!(
        messages,
        serde_json::json!([
            {"content": "inspect both files", "role": "user"},
            {
                "content": null,
                "role": "assistant",
                "tool_calls": [
                    {
                        "function": {
                            "arguments": r#"{"path":"Cargo.toml"}"#,
                            "name": "read"
                        },
                        "id": "call_manifest",
                        "type": "function"
                    },
                    {
                        "function": {
                            "arguments": r#"{"path":"README.md"}"#,
                            "name": "read"
                        },
                        "id": "call_readme",
                        "type": "function"
                    }
                ]
            },
            {
                "content": "manifest result",
                "role": "tool",
                "tool_call_id": "call_manifest"
            },
            {
                "content": "readme result",
                "role": "tool",
                "tool_call_id": "call_readme"
            }
        ])
    );
}

#[test]
fn serializes_conversation_history() {
    // Arrange
    let backend = native_schema_backend();
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let mut request = model::ModelRequest::new("first question", schema.clone());
    request.record_output_with_reasoning(&serde_json::json!({"message": "first answer"}), None);
    let mut messages = request.into_messages();
    messages.insert(
        0,
        model::ModelMessage::System("read-only instructions".to_string()),
    );
    let request = model::ModelRequest::with_history(messages, "second question", schema);

    // Act
    let messages = serde_json::to_value(
        backend
            .messages(&request)
            .expect("conversation history should serialize"),
    )
    .expect("messages should encode as JSON");

    // Assert
    assert_eq!(
        messages,
        serde_json::json!([
            {"content": "read-only instructions", "role": "system"},
            {"content": "first question", "role": "user"},
            {"content": r#"{"message":"first answer"}"#, "role": "assistant"},
            {"content": "second question", "role": "user"}
        ])
    );
}
