//! Tests for the `embed` example against a scripted Chat Completions server.

use std::process::ExitCode;

use ag_harness::{ApprovalHandler, ApprovalRequest, Decision};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{ApproveTickets, main, run};

#[tokio::test]
async fn files_a_ticket_through_the_approved_tool_and_reports_events() {
    // Arrange
    let server = MockServer::start().await;
    let tool_call = json!({"choices": [{"finish_reason": "tool_calls", "message": {
        "content": null,
        "tool_calls": [{"id": "call-1", "type": "function", "function": {
            "name": "file_ticket", "arguments": "{\"title\":\"Login times out\"}"
        }}]
    }}]});
    let answer = json!({"choices": [{"finish_reason": "stop", "message": {
        "content": "{\"ticket\":\"TICKET-15\"}"
    }}]});
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(&server)
        .await;
    let mut output = Vec::new();

    // Act
    let code = run(Some(format!("{}/v1", server.uri())), None, &mut output)
        .await
        .expect("example run");

    // Assert
    assert_eq!(code, ExitCode::SUCCESS);
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(
        output,
        "approval requested: file_ticket\ntool file_ticket: {\"id\":\"TICKET-15\"}\nticket: \
         \"TICKET-15\"\n"
    );
}

#[tokio::test]
async fn approval_denies_every_other_tool() {
    // Arrange
    let request: ApprovalRequest = serde_json::from_value(json!({
        "arguments": {}, "call_id": "call-1", "effects": "executes",
        "run": null, "session": null, "tool": "bash"
    }))
    .expect("approval request");

    // Act
    let decision = ApproveTickets.decide(&request).await;

    // Assert
    assert_eq!(
        decision,
        Decision::Deny {
            feedback: "only tickets may be filed".to_string()
        }
    );
}

#[test]
fn main_requires_an_endpoint() {
    // Act
    let error = main().expect_err("no endpoint is configured in tests");

    // Assert
    assert!(error.to_string().contains("AG_HARNESS_EXAMPLE_ENDPOINT"));
}
