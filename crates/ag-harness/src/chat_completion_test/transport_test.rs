use std::io;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use super::support::max_as_xhigh;
use crate::chat_completion::{
    ChatCompletionBackend, ChatCompletionClient as _, ChatCompletionError,
    ChatCompletionProviderPolicy, ChatCompletionRequest, MAX_RATE_LIMIT_RETRIES, MAX_RETRY_DELAY,
    RETRY_DELAY, ReasoningFormat, ReqwestChatCompletionClient, SUCCESS_BODY_LIMIT_BYTES,
    StructuredOutputMode, append_success_chunk, default_client, rate_limit_retry_delay,
};
use crate::model;

#[test]
fn rejects_success_chunk_that_exceeds_remaining_capacity() {
    // Arrange
    let mut body = vec![0; SUCCESS_BODY_LIMIT_BYTES - 1];

    // Act
    let error = append_success_chunk(&mut body, &[0, 1])
        .expect_err("chunk exceeding the limit should fail");

    // Assert
    assert!(matches!(error, ChatCompletionError::ResponseBodyTooLarge));
}

#[test]
fn wraps_transport_error_with_its_source() {
    // Arrange
    let source = io::Error::other("connection reset");

    // Act
    let error = ChatCompletionError::transport(source);

    // Assert
    assert_eq!(
        error.to_string(),
        "Chat Completions transport failed: connection reset"
    );
    assert_eq!(
        std::error::Error::source(&error)
            .expect("transport failure should retain its source")
            .to_string(),
        "connection reset"
    );
}

#[test]
fn normalizes_transport_error_classification() {
    // Arrange
    let backend = ChatCompletionBackend::with_client(
        "test-key".to_string(),
        "https://example.com/v1".to_string(),
        "model".to_string(),
        ChatCompletionProviderPolicy {
            display_name: "Provider",
            reasoning_format: ReasoningFormat::Effort(max_as_xhigh),
            response_format_with_tools: true,
            structured_output: StructuredOutputMode::JsonSchema,
            telemetry_name: "provider",
            unsupported_schema_reason: "object schema required",
        },
        default_client(),
    );
    let transport_error = ChatCompletionError::transport(io::Error::other("offline"));

    // Act
    let error = backend.map_completion_error(transport_error);

    // Assert
    assert_eq!(error.error_type(), model::ModelErrorType::Transport);
    assert_eq!(error.http_status(), None);
    assert_eq!(
        error.to_string(),
        "model request failed: Chat Completions transport failed: offline"
    );
}

#[test]
fn rate_limit_retry_delay_uses_bounded_headers_and_backoff() {
    // Arrange
    let mut headers = reqwest::header::HeaderMap::new();

    // Act and Assert
    assert_eq!(rate_limit_retry_delay(&headers, 0), Duration::from_secs(1));
    assert_eq!(rate_limit_retry_delay(&headers, 1), Duration::from_secs(2));
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "1".parse().expect("valid header"),
    );
    assert_eq!(rate_limit_retry_delay(&headers, 2), Duration::from_secs(4));
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "99".parse().expect("valid header"),
    );
    assert_eq!(rate_limit_retry_delay(&headers, 0), MAX_RETRY_DELAY);
    headers.insert(
        reqwest::header::RETRY_AFTER,
        reqwest::header::HeaderValue::from_bytes(b"invalid").expect("valid header bytes"),
    );
    assert_eq!(rate_limit_retry_delay(&headers, 0), RETRY_DELAY);
}

#[tokio::test]
async fn retries_rate_limit_response_before_decoding_success() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("retry listener should bind");
    let address = listener
        .local_addr()
        .expect("retry listener should have an address");
    let success_body =
        r#"{"choices":[{"finish_reason":"stop","message":{"content":"{\"message\":\"ok\"}"}}]}"#;
    let server = tokio::task::spawn_blocking(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener
                .accept()
                .expect("retry listener should accept a request");
            let mut request = [0; 2_048];
            assert!(
                stream.read(&mut request).expect("request should be read") > 0,
                "retry request should not be empty"
            );
            if attempt == 0 {
                stream
                    .write_all(
                        b"HTTP/1.1 429 Too Many Requests\r\n\
                          Content-Length: 0\r\n\
                          Retry-After: 0\r\n\
                          Connection: close\r\n\r\n",
                    )
                    .expect("rate-limit response should be written");
            } else {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    success_body.len(),
                    success_body
                )
                .expect("success response should be written");
            }
        }
    });
    let client = ReqwestChatCompletionClient {
        client: reqwest::Client::new(),
    };
    let request = ChatCompletionRequest::new(
        "test-key",
        format!("http://{address}"),
        serde_json::json!({}),
    );

    // Act
    let completion = client
        .complete(request)
        .await
        .expect("retry should recover")
        .expect("response should contain one choice");
    server.await.expect("retry server should finish");

    // Assert
    assert_eq!(completion.content.as_deref(), Some(r#"{"message":"ok"}"#));
}

#[tokio::test]
async fn retries_transport_failure_before_decoding_success() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("retry listener should bind");
    let address = listener
        .local_addr()
        .expect("retry listener should have an address");
    let success_body =
        r#"{"choices":[{"finish_reason":"stop","message":{"content":"{\"message\":\"ok\"}"}}]}"#;
    let server = tokio::task::spawn_blocking(move || {
        let (mut failed_stream, _) = listener
            .accept()
            .expect("retry listener should accept the failed request");
        let mut request = [0; 2_048];
        assert!(
            failed_stream
                .read(&mut request)
                .expect("failed request should be read")
                > 0,
            "failed request should not be empty"
        );
        drop(failed_stream);

        let (mut stream, _) = listener
            .accept()
            .expect("retry listener should accept the successful request");
        assert!(
            stream
                .read(&mut request)
                .expect("successful request should be read")
                > 0,
            "successful request should not be empty"
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            success_body.len(),
            success_body
        )
        .expect("success response should be written");
    });
    let client = ReqwestChatCompletionClient {
        client: reqwest::Client::new(),
    };
    let request = ChatCompletionRequest::new(
        "test-key",
        format!("http://{address}"),
        serde_json::json!({}),
    );

    // Act
    let completion = client
        .complete(request)
        .await
        .expect("transport retry should recover")
        .expect("response should contain one choice");
    server.await.expect("retry server should finish");

    // Assert
    assert_eq!(completion.content.as_deref(), Some(r#"{"message":"ok"}"#));
}

#[tokio::test]
async fn retains_http_status_when_error_body_read_fails() {
    // Arrange
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("truncated-response listener should bind");
    let address = listener
        .local_addr()
        .expect("truncated-response listener should have an address");
    let server = tokio::task::spawn_blocking(move || {
        for _ in 0..=MAX_RATE_LIMIT_RETRIES {
            let (mut stream, _) = listener
                .accept()
                .expect("truncated-response listener should accept a request");
            let mut request = [0; 2_048];
            let bytes_read = stream
                .read(&mut request)
                .expect("truncated-response server should read the request");
            assert!(
                bytes_read > 0,
                "truncated-response request should not be empty"
            );
            stream
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\n\
                      Content-Length: 64\r\n\
                      Retry-After: 0\r\n\
                      Connection: close\r\n\r\n\
                      partial error body",
                )
                .expect("truncated-response server should write the response");
        }
    });
    let client = ReqwestChatCompletionClient {
        client: reqwest::Client::new(),
    };
    let request = ChatCompletionRequest::new(
        "test-key",
        format!("http://{address}"),
        serde_json::json!({}),
    );

    // Act
    let result = client.complete(request).await;
    server
        .await
        .expect("truncated-response server should finish");

    // Assert
    assert!(result.is_err(), "truncated HTTP error response should fail");
    let error = result
        .err()
        .expect("truncated HTTP error response should contain an error");
    assert!(matches!(
        &error,
        ChatCompletionError::Http { status, .. }
            if *status == reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    assert!(error.to_string().contains("error body read failed"));
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<reqwest::Error>())
        .expect("HTTP failure should retain its status-bearing source");
    assert_eq!(
        source.status(),
        Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
    );
}
