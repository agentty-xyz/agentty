use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{
    kimi, kimi_model, mount_tool_response, person_schema, person_schema_value, read_request,
    read_tool_wire,
};
use crate::chat_completion::STRUCTURED_OUTPUT_INSTRUCTION;
use crate::harness::Harness;
use crate::{model, schema_contract};

#[tokio::test]
async fn preserves_reasoning_and_named_tool_result_for_continuation() {
    // Arrange
    let server = MockServer::start().await;
    let prompt = "inspect the manifest";
    let result = r#"{"content":"[workspace]","end_line":1,"next_offset":null,"path":"Cargo.toml","start_line":1,"truncated":false}"#;
    let reasoning_content = "I should inspect the manifest before answering.";
    mount_tool_response(
        &server,
        prompt,
        json!({
            "content": null,
            "reasoning_content": reasoning_content,
            "tool_calls": [{
                "id": "call_kimi_read",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                }
            }]
        }),
    )
    .await;
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
                        "id": "call_kimi_read",
                        "type": "function"
                    }]
                },
                {
                    "content": result,
                    "name": "read",
                    "role": "tool",
                    "tool_call_id": "call_kimi_read"
                }
            ],
            "model": "kimi-k2.6",
            "thinking": {"keep": "all", "type": "enabled"},
            "tools": [read_tool_wire()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Cargo"}"#}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let tool_response = model
        .complete(read_request(prompt))
        .await
        .expect("initial Kimi tool request should succeed");
    let call = tool_response
        .call()
        .expect("initial response should contain a tool call")
        .clone();
    let mut model_request = read_request(prompt);
    model_request.record_tool_result(call, result.to_string());
    let response = model
        .complete(model_request)
        .await
        .expect("continued Kimi request should succeed");

    // Assert
    assert_eq!(response.output(), Some(&json!({ "name": "Cargo" })));
}

#[tokio::test]
async fn preserves_terminal_k3_reasoning_for_next_session_turn() {
    // Arrange
    let server = MockServer::start().await;
    let first_prompt = "identify the person";
    let second_prompt = "repeat the person";
    let reasoning_content = "The requested name is Ada.";
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
                {"content": first_prompt, "role": "user"}
            ],
            "model": "kimi-k3",
            "response_format": {"type": "json_object"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": r#"{"name":"Ada"}"#,
                    "reasoning_content": reasoning_content
                }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
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
                {"content": first_prompt, "role": "user"},
                {
                    "content": r#"{"name":"Ada"}"#,
                    "reasoning_content": reasoning_content,
                    "role": "assistant"
                },
                {"content": second_prompt, "role": "user"}
            ],
            "model": "kimi-k3",
            "response_format": {"type": "json_object"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness =
        Harness::new(kimi_model(&server, "kimi-k3")).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("kimi-k3-reasoning", person_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    let first = session
        .send(first_prompt)
        .await
        .expect("first K3 turn should succeed");
    let second = session
        .send(second_prompt)
        .await
        .expect("continued K3 turn should succeed");

    // Assert
    assert_eq!(first.output(), &json!({ "name": "Ada" }));
    assert_eq!(second.output(), first.output());
}

#[tokio::test]
async fn preserves_terminal_k2_6_reasoning_for_next_session_turn() {
    // Arrange
    let server = MockServer::start().await;
    let first_prompt = "identify the person";
    let second_prompt = "repeat the person";
    let reasoning_content = "The requested name is Ada.";
    let thinking = json!({"keep": "all", "type": "enabled"});
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
                {"content": first_prompt, "role": "user"}
            ],
            "model": "kimi-k2.6",
            "response_format": {"type": "json_object"},
            "thinking": thinking
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": r#"{"name":"Ada"}"#,
                    "reasoning_content": reasoning_content
                }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
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
                {"content": first_prompt, "role": "user"},
                {
                    "content": r#"{"name":"Ada"}"#,
                    "reasoning_content": reasoning_content,
                    "role": "assistant"
                },
                {"content": second_prompt, "role": "user"}
            ],
            "model": "kimi-k2.6",
            "response_format": {"type": "json_object"},
            "thinking": thinking
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(kimi(&server)).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("kimi-k2.6-reasoning", person_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    let first = session
        .send(first_prompt)
        .await
        .expect("first K2.6 turn should succeed");
    let second = session
        .send(second_prompt)
        .await
        .expect("continued K2.6 turn should succeed");

    // Assert
    assert_eq!(first.output(), &json!({ "name": "Ada" }));
    assert_eq!(second.output(), first.output());
}

#[tokio::test]
async fn rejects_oversized_reasoning_content() {
    // Arrange
    let server = MockServer::start().await;
    mount_tool_response(
        &server,
        "inspect the manifest",
        json!({
            "content": null,
            "reasoning_content":
                "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1),
            "tool_calls": [{
                "id": "call_oversized_reasoning",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": r#"{"path":"Cargo.toml"}"#
                }
            }]
        }),
    )
    .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(read_request("inspect the manifest"))
        .await
        .expect_err("oversized reasoning content should fail");

    // Assert
    assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
}
