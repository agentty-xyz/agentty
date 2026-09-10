use crate::chat_completion::{
    ChatCompletionBackend, ChatCompletionResponse, ChatCompletionToolCall, GeneratedResponse,
};
use crate::{model, schema_contract, tool};

#[test]
fn decodes_complete_metadata_from_first_choice() {
    // Arrange
    let response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
        "choices": [
            {
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            },
            {
                "finish_reason": "length",
                "message": {"content": "ignored"}
            }
        ],
        "id": "response-1",
        "model": "provider-model",
        "system_fingerprint": "fingerprint-1",
        "usage": {
            "completion_tokens": 21,
            "completion_tokens_details": {"reasoning_tokens": 3},
            "prompt_cache_hit_tokens": 5,
            "prompt_cache_miss_tokens": 8,
            "prompt_tokens": 13,
            "prompt_tokens_details": {"cached_tokens": 99},
            "total_tokens": 34,
            "unknown_usage_field": 55
        },
        "unknown_response_field": true
    }))
    .expect("complete response metadata should decode");

    // Act
    let (choice, metadata) = response
        .into_completion()
        .expect("first completion choice should exist");
    let usage = metadata.usage().expect("usage should be retained");

    // Assert
    assert_eq!(choice.message.content.as_deref(), Some(r#"{"name":"Ada"}"#));
    assert_eq!(metadata.finish_reason(), "stop");
    assert_eq!(metadata.response_id(), Some("response-1"));
    assert_eq!(metadata.response_model(), Some("provider-model"));
    assert_eq!(metadata.system_fingerprint(), Some("fingerprint-1"));
    assert_eq!(usage.cache_hit_tokens(), Some(5));
    assert_eq!(usage.cache_miss_tokens(), Some(8));
    assert_eq!(usage.input_tokens(), Some(13));
    assert_eq!(usage.output_tokens(), Some(21));
    assert_eq!(usage.reasoning_tokens(), Some(3));
    assert_eq!(usage.total_tokens(), Some(34));
}

#[test]
fn decodes_partial_usage_without_estimating_missing_counts() {
    // Arrange
    let response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {"content": null}
        }],
        "id": 42,
        "model": ["unexpected"],
        "system_fingerprint": {"unexpected": true},
        "usage": {
            "completion_tokens": "unknown",
            "completion_tokens_details": {"reasoning_tokens": -1},
            "prompt_tokens_details": {"cached_tokens": 7}
        }
    }))
    .expect("partial usage should decode");

    // Act
    let (_, metadata) = response
        .into_completion()
        .expect("completion choice should exist");
    let usage = metadata.usage().expect("partial usage should be retained");

    // Assert
    assert_eq!(usage.cache_hit_tokens(), Some(7));
    assert_eq!(usage.cache_miss_tokens(), None);
    assert_eq!(usage.input_tokens(), None);
    assert_eq!(usage.output_tokens(), None);
    assert_eq!(usage.reasoning_tokens(), None);
    assert_eq!(usage.total_tokens(), None);
    assert_eq!(metadata.response_id(), None);
    assert_eq!(metadata.response_model(), None);
    assert_eq!(metadata.system_fingerprint(), None);
}

#[test]
fn preserves_missing_metadata_and_empty_choices() {
    // Arrange
    let response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": "{}"}
        }]
    }))
    .expect("minimal response should decode");
    let empty_response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
        "choices": []
    }))
    .expect("empty choices should decode");

    // Act
    let (_, metadata) = response
        .into_completion()
        .expect("minimal response should retain its choice");

    // Assert
    assert_eq!(metadata.response_id(), None);
    assert_eq!(metadata.response_model(), None);
    assert_eq!(metadata.system_fingerprint(), None);
    assert_eq!(metadata.usage(), None);
    assert!(empty_response.into_completion().is_none());
}

#[test]
fn decodes_advertised_write_tool_call() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("update", schema).with_tool(tool::ToolDefinition::write());
    let calls = vec![ChatCompletionToolCall {
        function: serde_json::json!({
            "name": "write",
            "arguments": r#"{"path":"src/lib.rs","patch":"--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"}"#
        }),
        id: "call_write".to_string(),
        kind: "function".to_string(),
    }];

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        Some("I will update the requested file."),
        None,
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::ToolCall {
            ref call,
            ref metadata,
        } if metadata.finish_reason() == "tool_calls"
            && call.name() == "write"
                && call.write_arguments().is_some_and(|arguments| {
                    arguments.path() == "src/lib.rs"
                        && arguments.patch().starts_with("--- a/src/lib.rs")
                })
    ));
}

#[test]
fn rejects_oversized_content_with_tool_calls() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
    let content = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
    let calls = vec![ChatCompletionToolCall {
        function: serde_json::json!({
            "name": "read",
            "arguments": r#"{"path":"Cargo.toml"}"#
        }),
        id: "call_read".to_string(),
        kind: "function".to_string(),
    }];

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        Some(&content),
        None,
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::Failed {
            error: model::ModelError::ResponseContentTooLarge,
            ..
        }
    ));
}

#[test]
fn retains_metadata_for_invalid_advertised_write_arguments() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("update", schema).with_tool(tool::ToolDefinition::write());
    let calls = vec![ChatCompletionToolCall {
        function: serde_json::json!({
            "name": "write",
            "arguments": r#"{"path":"src/lib.rs","patch":""}"#
        }),
        id: "call_write".to_string(),
        kind: "function".to_string(),
    }];

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        None,
        None,
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::Failed {
            error: model::ModelError::InvalidToolArguments { .. },
            metadata,
        } if metadata.finish_reason() == "tool_calls"
    ));
}

#[test]
fn decodes_schema_valid_read_rejection_for_tool_feedback() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
    let calls = vec![ChatCompletionToolCall {
        function: serde_json::json!({
            "name": "read",
            "arguments": r#"{"action":"search"}"#
        }),
        id: "call_search".to_string(),
        kind: "function".to_string(),
    }];

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        None,
        None,
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::ToolCall { ref call, .. }
            if call.read_arguments().is_some_and(|arguments| {
                arguments.validation_error()
                    == Some("search requires a query and accepts only an optional path and limit")
            })
    ));
}

#[test]
fn rejects_nul_search_query_as_invalid_tool_arguments() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
    let calls = vec![ChatCompletionToolCall {
        function: serde_json::json!({
            "name": "read",
            "arguments": r#"{"action":"search","query":"needle\u0000suffix"}"#
        }),
        id: "call_search".to_string(),
        kind: "function".to_string(),
    }];

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        None,
        None,
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::Failed {
            error: model::ModelError::InvalidToolArguments { .. },
            ..
        }
    ));
}

#[test]
fn decodes_multiple_advertised_tool_calls() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
    let calls = ["Cargo.toml", "README.md"]
        .into_iter()
        .enumerate()
        .map(|(index, path)| ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "read",
                "arguments": serde_json::json!({"path": path}).to_string()
            }),
            id: format!("call_{index}"),
            kind: "function".to_string(),
        })
        .collect();

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        None,
        Some("reasoning"),
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::ToolCalls { calls, metadata }
            if metadata.finish_reason() == "tool_calls"
                && calls.len() == 2
                && calls[0].read_arguments().is_some_and(|arguments| {
                    arguments.path() == "Cargo.toml"
                })
                && calls[1].read_arguments().is_some_and(|arguments| {
                    arguments.path() == "README.md"
                })
    ));
}

#[test]
fn rejects_duplicate_tool_call_ids() {
    // Arrange
    let schema = schema_contract::OutputSchema::new(serde_json::json!({
        "type": "object"
    }))
    .expect("schema should be valid");
    let request =
        model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
    let calls = ["Cargo.toml", "README.md"]
        .into_iter()
        .map(|path| ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "read",
                "arguments": serde_json::json!({"path": path}).to_string()
            }),
            id: "duplicate_call".to_string(),
            kind: "function".to_string(),
        })
        .collect();

    // Act
    let response = ChatCompletionBackend::decode_tool_call(
        &request,
        None,
        None,
        calls,
        model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
    );

    // Assert
    assert!(matches!(
        response,
        GeneratedResponse::Failed {
            error: model::ModelError::DuplicateToolCallId { id },
            metadata,
        } if id == "duplicate_call" && metadata.finish_reason() == "tool_calls"
    ));
}
