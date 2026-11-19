//! Shared Kimi test fixtures: schemas, requests, mock-server clients, and
//! mounted structured and tool-call responses.

use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, body_json, method, path};
#[cfg(feature = "sqlite")]
use wiremock::{Match, Request};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::provider::kimi::KimiConfig;
use crate::provider::test_support::STRUCTURED_OUTPUT_INSTRUCTION;
use crate::schema_contract::OutputSchema;
use crate::{model, tool};

/// Matches a durable-session request whose body equals the wrapped value
/// plus the session's `prompt_cache_key` and the streaming fields of every
/// Chat Completions request.
#[cfg(feature = "sqlite")]
pub(super) struct SessionBody(pub(super) Value);

#[cfg(feature = "sqlite")]
impl Match for SessionBody {
    fn matches(&self, request: &Request) -> bool {
        let Ok(mut body) = request.body_json::<Value>() else {
            return false;
        };
        let Some(fields) = body.as_object_mut() else {
            return false;
        };
        let key = fields.remove("prompt_cache_key");
        let streamed = fields.remove("stream") == Some(json!(true))
            && fields.remove("stream_options") == Some(json!({"include_usage": true}));

        streamed
            && key
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|key| key.starts_with("ag-session-"))
            && body == self.0
    }
}

pub(super) fn person_schema_value() -> Value {
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

pub(super) fn read_tool_wire() -> Value {
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

pub(super) fn kimi(server: &MockServer) -> model::ModelClient {
    kimi_model(server, "kimi-k2.6")
}

pub(super) fn kimi_model(server: &MockServer, model: &str) -> model::ModelClient {
    model::ModelClient::kimi(KimiConfig {
        api_key: "test-key".to_string(),
        base_url: format!("{}/", server.uri()),
        model: model.to_string(),
    })
    .expect("fixture configuration should be valid")
}

pub(super) async fn mount_structured_response(
    server: &MockServer,
    prompt: &str,
    schema: &Value,
    content: &str,
) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "stream": true,
            "stream_options": {"include_usage": true},
            "messages": [
                {
                    "content": format!("{STRUCTURED_OUTPUT_INSTRUCTION}{schema}"),
                    "role": "system"
                },
                {"content": prompt, "role": "user"}
            ],
            "model": "kimi-k2.6",
            "response_format": {"type": "json_object"},
            "thinking": {"keep": "all", "type": "enabled"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": content}
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}

pub(super) async fn mount_tool_response(server: &MockServer, prompt: &str, message: Value) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "stream": true,
            "stream_options": {"include_usage": true},
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
            "model": "kimi-k2.6",
            "thinking": {"keep": "all", "type": "enabled"},
            "tools": [read_tool_wire()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": message
            }]
        })))
        .expect(1)
        .mount(server)
        .await;
}
