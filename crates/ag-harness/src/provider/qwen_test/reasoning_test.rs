use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{
    mount_qwen_3_8_continuation, mount_qwen_3_8_tool_response, person_schema_value, qwen,
    qwen_model, read_request, read_tool_wire,
};
use crate::chat_completion::STRUCTURED_OUTPUT_INSTRUCTION;
use crate::tool;

#[tokio::test]
async fn sends_tool_result_history_for_continuation() {
    // Arrange
    let server = MockServer::start().await;
    let prompt = "inspect the manifest";
    let result = r#"{"content":"[workspace]","end_line":1,"next_offset":null,"path":"Cargo.toml","start_line":1,"truncated":false}"#;
    let arguments = serde_json::from_value(json!({
        "path": "Cargo.toml",
        "offset": 1,
        "limit": 12
    }))
    .expect("read arguments should be valid");
    let call = tool::ToolCall::read("call_qwen_read".to_string(), arguments, None);
    let mut model_request = read_request(prompt);
    model_request.record_tool_result(call, result.to_string());
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
            "model": "qwen-plus",
            "tools": [read_tool_wire()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": r#"{"name":"Cargo"}"#,
                    "tool_calls": null
                }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let model = qwen(&server);

    // Act
    let response = model
        .complete(model_request)
        .await
        .expect("continued Qwen request should succeed");

    // Assert
    assert_eq!(response.output(), Some(&json!({ "name": "Cargo" })));
}

#[tokio::test]
async fn preserves_qwen_3_8_reasoning_for_tool_continuation() {
    // Arrange
    let server = MockServer::start().await;
    let prompt = "inspect the manifest";
    let result = r#"{"content":"[workspace]","end_line":1,"next_offset":null,"path":"Cargo.toml","start_line":1,"truncated":false}"#;
    let reasoning_content = "I should inspect the manifest before answering.";
    mount_qwen_3_8_tool_response(&server, prompt, reasoning_content).await;
    mount_qwen_3_8_continuation(&server, prompt, result, reasoning_content).await;
    let model = qwen_model(&server, "qwen3.8-max");

    // Act
    let tool_response = model
        .complete(read_request(prompt))
        .await
        .expect("initial Qwen3.8 tool request should succeed");
    let call = tool_response
        .call()
        .expect("initial response should contain a tool call")
        .clone();
    let mut model_request = read_request(prompt);
    model_request.record_tool_result(call, result.to_string());
    let response = model
        .complete(model_request)
        .await
        .expect("continued Qwen3.8 request should succeed");

    // Assert
    assert_eq!(response.output(), Some(&json!({ "name": "Cargo" })));
}
