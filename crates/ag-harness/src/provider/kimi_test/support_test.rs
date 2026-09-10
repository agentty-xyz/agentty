use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::chat_completion::STRUCTURED_OUTPUT_INSTRUCTION;
use crate::provider::kimi::KimiConfig;
use crate::schema_contract::OutputSchema;
use crate::{model, tool};

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
