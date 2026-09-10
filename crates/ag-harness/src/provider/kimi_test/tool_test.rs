use serde_json::json;
use wiremock::MockServer;

use super::support::{kimi, mount_tool_response, read_request};
use crate::{model, schema_contract};

#[tokio::test]
async fn advertises_and_decodes_read_tool_call() {
    // Arrange
    let server = MockServer::start().await;
    mount_tool_response(
        &server,
        "inspect the manifest",
        json!({
            "content": null,
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
    let model = kimi(&server);

    // Act
    let response = model
        .complete(read_request("inspect the manifest"))
        .await
        .expect("Kimi read request should decode");

    // Assert
    assert!(response.output().is_none());
    let call = response
        .call()
        .expect("response should contain a tool call");
    assert_eq!(call.id(), "call_kimi_read");
    assert_eq!(call.name(), "read");
    let arguments = call
        .read_arguments()
        .expect("provider should decode read arguments");
    assert_eq!(arguments.path(), "Cargo.toml");
    assert_eq!(arguments.offset(), Some(1));
    assert_eq!(arguments.limit(), Some(12));
}

#[tokio::test]
async fn rejects_missing_tool_call() {
    // Arrange
    let server = MockServer::start().await;
    mount_tool_response(&server, "inspect the manifest", json!({"content": null})).await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(read_request("inspect the manifest"))
        .await
        .expect_err("missing tool call should fail");

    // Assert
    assert_eq!(error.to_string(), "model returned no tool call");
}

#[tokio::test]
async fn rejects_invalid_read_range() {
    // Arrange
    let server = MockServer::start().await;
    mount_tool_response(
        &server,
        "inspect the manifest",
        json!({
            "content": null,
            "tool_calls": [{
                "id": "call_invalid_limit",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": r#"{"path":"Cargo.toml","limit":0}"#
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
        .expect_err("zero read limit should fail");

    // Assert
    assert!(matches!(
        error,
        model::ModelError::InvalidToolArguments { .. }
    ));
}

#[tokio::test]
async fn rejects_oversized_read_arguments() {
    // Arrange
    let server = MockServer::start().await;
    let arguments = format!(
        r#"{{"path":"{}"}}"#,
        "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES)
    );
    mount_tool_response(
        &server,
        "inspect the manifest",
        json!({
            "content": null,
            "tool_calls": [{
                "id": "call_oversized",
                "type": "function",
                "function": {"name": "read", "arguments": arguments}
            }]
        }),
    )
    .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(read_request("inspect the manifest"))
        .await
        .expect_err("oversized read arguments should fail");

    // Assert
    assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
}
