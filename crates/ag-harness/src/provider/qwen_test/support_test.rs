use async_trait::async_trait;
use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::chat_completion::{
    ChatCompletion, ChatCompletionClient, ChatCompletionError, ChatCompletionRequest,
    STRUCTURED_OUTPUT_INSTRUCTION,
};
use crate::provider::qwen::{QWEN_PLUS, QwenConfig};
use crate::schema_contract::OutputSchema;
use crate::{model, tool};

pub(super) struct StubClient;

#[async_trait]
impl ChatCompletionClient for StubClient {
    async fn complete(
        &self,
        request: ChatCompletionRequest<'_>,
    ) -> Result<Option<ChatCompletion>, ChatCompletionError> {
        assert_eq!(request.api_key(), "stub-key");
        assert_eq!(
            request.endpoint(),
            "https://stub.example/v1/chat/completions"
        );
        assert_eq!(request.payload()["model"], "qwen-stub");
        assert_eq!(request.payload()["response_format"]["type"], "json_object");

        Ok(Some(ChatCompletion::new(
            "stop".to_string(),
            Some(r#"{"name":"Ada"}"#.to_string()),
        )))
    }
}

pub(super) fn person_schema_value() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

pub(super) fn person_schema() -> OutputSchema {
    OutputSchema::new(person_schema_value()).expect("schema should be valid")
}

pub(super) fn request(prompt: &str) -> model::ModelRequest {
    model::ModelRequest::new(prompt, person_schema())
}

pub(super) fn read_request(prompt: &str) -> model::ModelRequest {
    request(prompt).with_tool(tool::ToolDefinition::read())
}

pub(super) fn read_tool_wire() -> serde_json::Value {
    let definition = tool::ToolDefinition::read();

    json!({
        "type": "function",
        "function": {
            "description": definition.description(),
            "name": definition.name(),
            "parameters": definition.parameters()
        }
    })
}

pub(super) fn escaped_value_schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "value": { "type": "string" }
        },
        "required": ["value"],
        "additionalProperties": false
    }))
    .expect("schema should be valid")
}

pub(super) fn qwen(server: &MockServer) -> model::ModelClient {
    qwen_model(server, QWEN_PLUS)
}

pub(super) fn qwen_model(server: &MockServer, model: &str) -> model::ModelClient {
    model::ModelClient::qwen(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: format!("{}/", server.uri()),
        model: model.to_string(),
    })
    .expect("fixture configuration should be valid")
}

pub(super) async fn mount_structured_response(
    server: &MockServer,
    prompt: &str,
    schema: &serde_json::Value,
    content: &str,
) {
    let schema_instruction = format!("{STRUCTURED_OUTPUT_INSTRUCTION}{schema}");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "messages": [
                {"content": schema_instruction, "role": "system"},
                {"content": prompt, "role": "user"}
            ],
            "model": "qwen-plus",
            "response_format": {"type": "json_object"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": content, "tool_calls": null}
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

pub(super) async fn mount_tool_response(
    server: &MockServer,
    prompt: &str,
    message: serde_json::Value,
) {
    mount_read_response(server, prompt, "tool_calls", message).await;
}

pub(super) async fn mount_read_response(
    server: &MockServer,
    prompt: &str,
    finish_reason: &str,
    message: serde_json::Value,
) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "messages": [
                {
                    "content": format!(
                        "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                        person_schema_value()
                    ),
                    "role": "system"
                },
                {"content": prompt, "role": "user"}
            ],
            "model": "qwen-plus",
            "tools": [read_tool_wire()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": finish_reason,
                "message": message
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

pub(super) async fn mount_qwen_3_8_tool_response(
    server: &MockServer,
    prompt: &str,
    reasoning_content: &str,
) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "messages": [
                {
                    "content": format!(
                        "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                        person_schema_value()
                    ),
                    "role": "system"
                },
                {"content": prompt, "role": "user"}
            ],
            "model": "qwen3.8-max",
            "tools": [read_tool_wire()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "reasoning_content": reasoning_content,
                    "tool_calls": [{
                        "id": "call_qwen_read",
                        "type": "function",
                        "function": {
                            "name": "read",
                            "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                        }
                    }]
                }
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

pub(super) async fn mount_qwen_3_8_continuation(
    server: &MockServer,
    prompt: &str,
    result: &str,
    reasoning_content: &str,
) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "messages": [
                {
                    "content": format!(
                        "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                        person_schema_value()
                    ),
                    "role": "system"
                },
                {"content": prompt, "role": "user"},
                {
                    "content": null,
                    "reasoning_content": reasoning_content,
                    "role": "assistant",
                    "tool_calls": [{
                        "function": {
                            "arguments": r#"{"limit":12,"offset":1,"path":"Cargo.toml"}"#,
                            "name": "read"
                        },
                        "id": "call_qwen_read",
                        "type": "function"
                    }]
                },
                {
                    "content": result,
                    "role": "tool",
                    "tool_call_id": "call_qwen_read"
                }
            ],
            "model": "qwen3.8-max",
            "tools": [read_tool_wire()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Cargo"}"#}
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}
