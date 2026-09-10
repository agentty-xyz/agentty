use serde_json::json;
use wiremock::MockServer;

use super::support::{
    mount_read_response, mount_structured_response, mount_tool_response, person_schema_value, qwen,
    read_request, request,
};
use crate::model;

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
async fn rejects_malformed_and_invalid_read_arguments() {
    // Arrange
    let cases = [
        ("{", "model returned invalid tool arguments:"),
        (
            r#"{"path":"Cargo.toml","offset":0}"#,
            "model returned invalid tool arguments:",
        ),
        (
            r#"{"path":"Cargo.toml","extra":true}"#,
            "model returned invalid tool arguments:",
        ),
    ];

    // Act
    let mut errors = Vec::new();
    for (index, (arguments, expected)) in cases.into_iter().enumerate() {
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
        let error = qwen(&server)
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("invalid read arguments should fail");
        errors.push((error.to_string(), expected));
    }

    // Assert
    assert!(
        errors
            .iter()
            .all(|(error, expected)| error.starts_with(expected))
    );
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
async fn accepts_bounded_content_with_tool_calls() {
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
        .expect("incidental tool-call content should be ignored");

    // Assert
    assert_eq!(
        response
            .call()
            .expect("response should contain a tool call")
            .id(),
        "call_content"
    );
}
