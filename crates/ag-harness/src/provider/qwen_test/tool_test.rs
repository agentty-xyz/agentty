use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{
    mount_read_response, mount_structured_response, mount_tool_response, person_schema_value, qwen,
    read_request, request,
};
use crate::{model, tool};

#[tokio::test]
async fn completes_terminal_response_with_null_tool_calls() {
    // Arrange
    let server = MockServer::start().await;
    let schema_value = person_schema_value();
    mount_structured_response(
        &server,
        "extract the name",
        &schema_value,
        r#"{"name":"Ada"}"#,
    )
    .await;
    let model: Box<dyn model::Model> = Box::new(qwen(&server));

    // Act
    let response = model
        .complete(request("extract the name"))
        .await
        .expect("Qwen request should succeed");

    // Assert
    assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
}

#[tokio::test]
async fn advertises_and_decodes_read_tool_call() {
    // Arrange
    let server = MockServer::start().await;
    mount_tool_response(
        &server,
        "inspect the manifest",
        json!({
            "content": "",
            "tool_calls": [{
                "id": "call_qwen_read",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                }
            }]
        }),
    )
    .await;
    let model = qwen(&server);

    // Act
    let response = model
        .complete(read_request("inspect the manifest"))
        .await
        .expect("Qwen read request should decode");

    // Assert
    assert!(response.output().is_none());
    let call = response
        .call()
        .expect("response should contain a tool call");
    assert_eq!(call.id(), "call_qwen_read");
    assert_eq!(call.name(), "read");
    let arguments = call
        .read_arguments()
        .expect("provider should decode read arguments");
    assert_eq!(arguments.path(), "Cargo.toml");
    assert_eq!(arguments.offset(), Some(1));
    assert_eq!(arguments.limit(), Some(12));
}

#[tokio::test]
async fn rejects_terminal_response_with_tool_calls() {
    // Arrange
    let server = MockServer::start().await;
    mount_read_response(
        &server,
        "inspect the manifest",
        "stop",
        json!({
            "content": r#"{"name":"Cargo"}"#,
            "tool_calls": [{
                "id": "call_late",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": r#"{"path":"Cargo.toml"}"#
                }
            }]
        }),
    )
    .await;
    let model = qwen(&server);

    // Act
    let error = model
        .complete(read_request("inspect the manifest"))
        .await
        .expect_err("terminal response with tool calls should fail");

    // Assert
    assert!(matches!(
        error,
        model::ModelError::TerminalResponseWithToolCalls
    ));
}

#[tokio::test]
async fn keeps_malformed_read_arguments_and_replays_them_as_json_objects() {
    // Arrange
    let cases = [
        "{",
        r#"{"path":"Cargo.toml","offset":0}"#,
        r#"{"path":"Cargo.toml","extra":true}"#,
    ];

    // Act
    let mut calls = Vec::new();
    for (index, arguments) in cases.into_iter().enumerate() {
        let server = MockServer::start().await;
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": null,
                "tool_calls": [{
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": {"name": "read", "arguments": arguments}
                }]
            }),
        )
        .await;
        let completion = qwen(&server)
            .complete(read_request("inspect the manifest"))
            .await
            .expect("malformed read arguments are kept");
        let model::ModelResponse::ToolCall(call) = completion.response() else {
            std::panic::resume_unwind(Box::new("tool call expected"));
        };
        calls.push(call.clone());
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .mount(&server)
        .await;
    let mut replay = read_request("inspect the manifest");
    replay.record_tool_calls(calls.clone());
    for call in &calls {
        replay.record_tool_result(call, r#"{"status":"rejected"}"#.to_string());
    }
    qwen(&server)
        .complete(replay)
        .await
        .expect("replayed malformed calls are accepted");
    let requests = server.received_requests().await.expect("recorded");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("JSON body");

    // Assert
    for (call, raw) in calls.iter().zip(cases) {
        assert_eq!(
            call.malformed_arguments()
                .map(tool::MalformedArguments::raw),
            Some(raw)
        );
    }
    let wire = body["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["tool_calls"].is_array())
        })
        .expect("replayed tool calls");
    let replayed: Vec<serde_json::Value> = wire["tool_calls"]
        .as_array()
        .expect("tool calls")
        .iter()
        .map(|call| {
            serde_json::from_str(call["function"]["arguments"].as_str().expect("string"))
                .expect("JSON object on the wire")
        })
        .collect();
    assert_eq!(replayed[0], json!({"malformed_arguments": "{"}));
    assert_eq!(replayed[1], json!({"path":"Cargo.toml","offset":0}));
    assert_eq!(replayed[2], json!({"path":"Cargo.toml","extra":true}));
}

#[tokio::test]
async fn rejects_unsupported_tool_type_and_name() {
    // Arrange
    let messages = [
        (
            json!({
                "content": null,
                "tool_calls": [{
                    "id": "call_type",
                    "type": "custom"
                }]
            }),
            "model requested unsupported tool type: custom",
        ),
        (
            json!({
                "content": null,
                "tool_calls": [{
                    "id": "call_name",
                    "type": "function",
                    "function": {"name": "write", "arguments": r#"{"path":"Cargo.toml"}"#}
                }]
            }),
            "model requested unsupported tool: write",
        ),
        (
            json!({
                "content": null,
                "tool_calls": [{
                    "id": "call_function_payload",
                    "type": "function"
                }]
            }),
            "model returned invalid tool arguments:",
        ),
    ];

    // Act
    let mut errors = Vec::new();
    for (message, expected) in messages {
        let server = MockServer::start().await;
        mount_tool_response(&server, "inspect the manifest", message).await;
        let error = qwen(&server)
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("unsupported tool response should fail");
        errors.push((error.to_string(), expected));
    }

    // Assert
    assert!(errors.iter().all(|(error, expected)| {
        error == expected || (expected.ends_with(':') && error.starts_with(expected))
    }));
}

#[tokio::test]
async fn keeps_bounded_content_with_tool_calls_as_assistant_text() {
    // Arrange
    let server = MockServer::start().await;
    mount_tool_response(
        &server,
        "inspect the manifest",
        json!({
            "content": "I will inspect the manifest.",
            "tool_calls": [{
                "id": "call_content",
                "type": "function",
                "function": {"name": "read", "arguments": r#"{"path":"Cargo.toml"}"#}
            }]
        }),
    )
    .await;

    // Act
    let response = qwen(&server)
        .complete(read_request("inspect the manifest"))
        .await
        .expect("tool-call content should be kept");

    // Assert
    let call = response
        .call()
        .expect("response should contain a tool call");
    assert_eq!(call.id(), "call_content");
    assert_eq!(call.assistant_text(), Some("I will inspect the manifest."));
}
