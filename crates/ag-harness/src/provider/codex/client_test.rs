use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth::{CodexAuth, ORIGINATOR_VALUE};
use super::super::error::CodexClientError;
use super::super::model::response_message;
use super::super::sse::{read_response_body, with_stream_idle_timeout};
use super::super::test_support::{
    auth_with_account, auth_with_fedramp, person_schema, success_sse, valid_auth, write_auth,
};
use super::{
    CodexResponsesClient, CodexResponsesRequest, HttpCodexResponsesClient, STREAM_IDLE_TIMEOUT,
};
use crate::model::{ModelErrorType, ReasoningEffort};
use crate::transport;

#[tokio::test]
async fn http_client_sends_subscription_headers_and_parses_output() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(directory.path(), &auth_with_fedramp(true));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer access-token"))
        .and(header("chatgpt-account-id", "account-1"))
        .and(header("x-openai-fedramp", "true"))
        .and(header("originator", ORIGINATOR_VALUE))
        .and(header("accept", "text/event-stream"))
        .and(body_json(json!({
            "input": [{
                "content": [{ "text": "Extract", "type": "input_text" }],
                "role": "user",
                "type": "message"
            }],
            "include": ["reasoning.encrypted_content"],
            "instructions": "Use the schema",
            "model": "gpt-test",
            "reasoning": { "effort": "high" },
            "store": false,
            "stream": true,
            "text": {
                "format": {
                    "name": "ag_harness_output",
                    "schema": person_schema().value(),
                    "strict": true,
                    "type": "json_schema"
                }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(success_sse(), "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let client =
        HttpCodexResponsesClient::with_endpoint(auth_file, format!("{}/responses", server.uri()));
    let request = CodexResponsesRequest {
        input: vec![response_message("user", "input_text", "Extract")],
        instructions: "Use the schema".to_string(),
        model: "gpt-test".to_string(),
        output_schema: person_schema().value().clone(),
        reasoning_effort: Some(ReasoningEffort::High),
        replay_account_fingerprint: None,
    };

    // Act
    let completion = client
        .complete(request)
        .await
        .expect("request should succeed");

    // Assert
    assert_eq!(completion.output, r#"{"name":"Ada"}"#);
    assert!(completion.account_fingerprint.is_some());
    assert_eq!(completion.response_id.as_deref(), Some("response-1"));
    assert_eq!(completion.response_model.as_deref(), Some("gpt-test"));
    let usage = completion.usage.expect("usage should be available");
    assert_eq!(usage.cache_hit_tokens(), Some(2));
    assert_eq!(usage.cache_miss_tokens(), Some(8));
    assert_eq!(usage.reasoning_tokens(), Some(1));
    assert_eq!(usage.total_tokens(), Some(14));
}

#[tokio::test]
async fn http_client_uses_header_model_when_body_omits_it() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(directory.path(), &valid_auth());
    let body = concat!(
        "data: {\"type\":\"response.output_text.delta\",",
        "\"delta\":\"{\\\"name\\\":\\\"Ada\\\"}\"}\n\n",
        "data: {\"type\":\"response.completed\",",
        "\"response\":{\"id\":\"response-1\"}}\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("openai-model", "routed-model")
                .set_body_raw(body, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client =
        HttpCodexResponsesClient::with_endpoint(auth_file, format!("{}/responses", server.uri()));

    // Act
    let completion = client
        .complete(CodexResponsesRequest {
            input: vec![],
            instructions: "instructions".to_string(),
            model: "requested-model".to_string(),
            output_schema: person_schema().value().clone(),
            reasoning_effort: None,
            replay_account_fingerprint: None,
        })
        .await
        .expect("request should succeed");

    // Assert
    assert_eq!(completion.output, r#"{"name":"Ada"}"#);
    assert_eq!(completion.response_model.as_deref(), Some("routed-model"));
}

#[tokio::test]
async fn http_client_accepts_token_refresh_and_rejects_account_changes() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(
        directory.path(),
        &auth_with_account("account-1", "access-token"),
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(success_sse(), "text/event-stream"))
        .expect(2)
        .mount(&server)
        .await;
    let client = HttpCodexResponsesClient::with_endpoint(
        auth_file.clone(),
        format!("{}/responses", server.uri()),
    );
    let request = |replay_account_fingerprint| CodexResponsesRequest {
        input: vec![],
        instructions: "instructions".to_string(),
        model: "gpt-test".to_string(),
        output_schema: person_schema().value().clone(),
        reasoning_effort: None,
        replay_account_fingerprint,
    };
    let first_account_fingerprint = CodexAuth {
        access_token: String::new(),
        account_id: "account-1".to_string(),
        is_fedramp_account: false,
    }
    .account_fingerprint();

    // Act
    client
        .complete(request(None))
        .await
        .expect("first account request should succeed");
    write_auth(
        directory.path(),
        &auth_with_account("account-1", "refreshed-token"),
    );
    client
        .complete(request(None))
        .await
        .expect("refreshed token for the bound account should succeed");
    write_auth(
        directory.path(),
        &auth_with_account("account-2", "other-account-token"),
    );
    let changed_account_error = client.complete(request(None)).await.err();
    let restarted_client =
        HttpCodexResponsesClient::with_endpoint(auth_file, format!("{}/responses", server.uri()));
    let resumed_account_error = restarted_client
        .complete(request(Some(first_account_fingerprint)))
        .await
        .err();

    // Assert
    assert!(
        changed_account_error
            .expect("changed account should be rejected")
            .to_string()
            .contains("different ChatGPT account")
    );
    assert!(
        resumed_account_error
            .expect("resumed session under another account should be rejected")
            .to_string()
            .contains("different ChatGPT account")
    );
}

#[tokio::test]
async fn http_client_preserves_provider_status() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(directory.path(), &valid_auth());
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(401).set_body_string("expired"))
        .expect(1)
        .mount(&server)
        .await;
    let client =
        HttpCodexResponsesClient::with_endpoint(auth_file, format!("{}/responses", server.uri()));

    // Act
    let error = client
        .complete(CodexResponsesRequest {
            input: vec![],
            instructions: "instructions".to_string(),
            model: "gpt-test".to_string(),
            output_schema: person_schema().value().clone(),
            reasoning_effort: None,
            replay_account_fingerprint: None,
        })
        .await
        .err()
        .expect("unauthorized request should fail");

    // Assert
    assert_eq!(error.error_type(), ModelErrorType::Provider);
    assert_eq!(error.http_status(), Some(401));
    assert!(error.to_string().contains("expired"));
}

#[tokio::test]
async fn http_client_does_not_follow_redirects() {
    // Arrange
    let redirect_server = MockServer::start().await;
    let target_server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(directory.path(), &valid_auth());
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/target", target_server.uri())),
        )
        .expect(1)
        .mount(&redirect_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(success_sse(), "text/event-stream"))
        .expect(0)
        .mount(&target_server)
        .await;
    let client = HttpCodexResponsesClient::with_endpoint(
        auth_file,
        format!("{}/responses", redirect_server.uri()),
    );

    // Act
    let error = client
        .complete(CodexResponsesRequest {
            input: vec![],
            instructions: "instructions".to_string(),
            model: "gpt-test".to_string(),
            output_schema: person_schema().value().clone(),
            reasoning_effort: None,
            replay_account_fingerprint: None,
        })
        .await
        .err();

    // Assert
    assert_eq!(
        error
            .expect("redirect response should not be followed")
            .error_type(),
        ModelErrorType::InvalidProviderResponse
    );
    assert!(
        target_server
            .received_requests()
            .await
            .expect("target requests should be recorded")
            .is_empty()
    );
}

#[tokio::test]
async fn http_client_rejects_a_closed_stream_without_completion() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(directory.path(), &valid_auth());
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"type\":\"response.created\"}\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client =
        HttpCodexResponsesClient::with_endpoint(auth_file, format!("{}/responses", server.uri()));

    // Act
    let error = client
        .complete(CodexResponsesRequest {
            input: vec![],
            instructions: "instructions".to_string(),
            model: "gpt-test".to_string(),
            output_schema: person_schema().value().clone(),
            reasoning_effort: None,
            replay_account_fingerprint: None,
        })
        .await
        .err();

    // Assert
    assert_eq!(
        error
            .expect("stream without completion should fail")
            .error_type(),
        ModelErrorType::InvalidProviderResponse
    );
}

#[tokio::test]
async fn http_client_bounds_error_bodies_and_classifies_transport_failures() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let auth_file = write_auth(directory.path(), &valid_auth());
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(500).set_body_bytes(vec![
            b'x';
            transport::ERROR_BODY_LIMIT_BYTES
                + 1
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let timeout_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(50))
                .set_body_raw(success_sse(), "text/event-stream"),
        )
        .mount(&timeout_server)
        .await;
    let oversized_client = HttpCodexResponsesClient::with_endpoint(
        auth_file.clone(),
        format!("{}/responses", server.uri()),
    );
    let timeout_client = HttpCodexResponsesClient::with_endpoint_and_timeouts(
        auth_file.clone(),
        format!("{}/responses", timeout_server.uri()),
        Duration::from_millis(1),
        super::RESPONSE_TIMEOUT,
        STREAM_IDLE_TIMEOUT,
    );
    let transport_client =
        HttpCodexResponsesClient::with_endpoint(auth_file.clone(), "https://[::1".to_string());
    let client_error = reqwest::Client::new()
        .get("https://[::1")
        .build()
        .expect_err("invalid URL should fail to build");
    let client_configuration_client =
        HttpCodexResponsesClient::with_http_error(auth_file, client_error);
    let request = || CodexResponsesRequest {
        input: vec![],
        instructions: "instructions".to_string(),
        model: "gpt-test".to_string(),
        output_schema: person_schema().value().clone(),
        reasoning_effort: None,
        replay_account_fingerprint: None,
    };

    // Act
    let oversized_error = oversized_client.complete(request()).await.err();
    let timeout_error = timeout_client.complete(request()).await.err();
    let transport_error = transport_client.complete(request()).await.err();
    let client_configuration_error = client_configuration_client.complete(request()).await.err();

    // Assert
    assert!(
        oversized_error
            .expect("oversized error response should fail")
            .to_string()
            .contains("Codex response exceeds the size limit")
    );
    assert_eq!(
        timeout_error
            .expect("request deadline should fail")
            .error_type(),
        ModelErrorType::Transport
    );
    assert_eq!(
        transport_error
            .expect("invalid endpoint should fail")
            .error_type(),
        ModelErrorType::Transport
    );
    assert_eq!(
        client_configuration_error
            .expect("invalid HTTP client configuration should fail")
            .error_type(),
        ModelErrorType::Transport
    );
}

#[tokio::test]
async fn response_reader_rejects_oversized_and_non_utf8_bodies() {
    // Arrange
    let oversized_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"too large"))
        .mount(&oversized_server)
        .await;
    let invalid_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff]))
        .mount(&invalid_server)
        .await;
    let mut oversized_response = reqwest::get(oversized_server.uri())
        .await
        .expect("oversized fixture should respond");
    let mut invalid_response = reqwest::get(invalid_server.uri())
        .await
        .expect("invalid UTF-8 fixture should respond");

    // Act
    let oversized_error = read_response_body(&mut oversized_response, 1, STREAM_IDLE_TIMEOUT)
        .await
        .err();
    let invalid_error = read_response_body(&mut invalid_response, 1, STREAM_IDLE_TIMEOUT)
        .await
        .err();
    let idle_error = with_stream_idle_timeout(
        std::future::pending::<Result<(), reqwest::Error>>(),
        Duration::from_millis(1),
    )
    .await
    .err();

    // Assert
    assert!(matches!(
        oversized_error,
        Some(CodexClientError::ResponseTooLarge)
    ));
    assert!(matches!(
        invalid_error,
        Some(CodexClientError::InvalidSse { .. })
    ));
    assert!(matches!(
        idle_error,
        Some(CodexClientError::StreamIdleTimeout)
    ));
}
