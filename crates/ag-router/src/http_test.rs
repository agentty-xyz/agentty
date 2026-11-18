//! HTTP transport tests for body limits, error classification, timeouts, and
//! rate-limit retries.

use std::io;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use super::{
    ERROR_BODY_LIMIT_BYTES, HttpTransport, MAX_PROVIDER_RETRY_DELAY, MAX_RATE_LIMIT_RETRIES,
    MAX_RETRY_DELAY, ProviderRequest, RETRY_DELAY, SUCCESS_BODY_LIMIT_BYTES, TransportError,
    append_success_chunk, has_context_overflow_marker, is_quota_exceeded, rate_limit_retry_delay,
    retry_after,
};
use crate::model;

const REQUEST_TIMEOUT: Duration = Duration::from_mins(3);

/// Returns a production transport with millisecond retry delays.
fn transport(request_timeout: Duration) -> HttpTransport {
    HttpTransport {
        max_retry_delay: Duration::from_millis(5),
        retry_delay: Duration::from_millis(1),
        ..HttpTransport::new(request_timeout)
    }
}

/// Returns an authenticated request with an empty body to `address`.
fn request(address: std::net::SocketAddr) -> ProviderRequest {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key"));

    ProviderRequest::new(headers, format!("http://{address}"), serde_json::json!({}))
}

/// Returns a request to `address` whose query and `api-key` header carry
/// secrets, as a gateway's may.
fn secret_request(address: std::net::SocketAddr) -> ProviderRequest {
    let mut headers = HeaderMap::new();
    headers.insert("api-key", HeaderValue::from_static("header-secret"));

    ProviderRequest::new(
        headers,
        format!("http://{address}/v1?key=query-secret"),
        serde_json::json!({}),
    )
}

/// Returns the display of `error` and every source, joined by `: `.
fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        chain.push_str(": ");
        chain.push_str(&error.to_string());
        source = error.source();
    }

    chain
}

#[tokio::test]
async fn refuses_a_redirect_so_secrets_never_reach_its_target() {
    // Arrange
    let target = TcpListener::bind("127.0.0.1:0").expect("target listener should bind");
    let target_address = target
        .local_addr()
        .expect("target listener should have an address");
    let response = format!(
        "HTTP/1.1 308 Permanent Redirect\r\nLocation: http://{target_address}/v1\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let (address, server) = serve_once(response.into_bytes());
    let client = transport(Duration::from_secs(5));

    // Act
    let result = client.post_json::<Value>(&secret_request(address)).await;
    server.await.expect("server should finish");

    // Assert
    let error = result.expect_err("a redirect must fail");
    assert!(
        matches!(error, TransportError::Redirected(status) if status.as_u16() == 308),
        "{error:?}"
    );
    target
        .set_nonblocking(true)
        .expect("target listener should poll without blocking");
    assert_eq!(
        target.accept().map(drop).map_err(|error| error.kind()),
        Err(io::ErrorKind::WouldBlock),
        "the redirect target must receive no request"
    );
    let model_error = error.into_model_error("Gateway", &[]);
    assert_eq!(model_error.http_status(), Some(308));
    assert_eq!(model_error.error_class(), model::ErrorClass::Provider);
    assert!(!error_chain(&model_error).contains(&target_address.to_string()));
}

#[tokio::test]
async fn transport_failures_never_name_the_url() {
    // Arrange
    let refused = TcpListener::bind("127.0.0.1:0")
        .expect("listener should bind")
        .local_addr()
        .expect("listener should have an address");
    let body = "{\"partial\":";
    let truncated = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len() + 10
    );
    let (address, server) = serve_once(truncated.into_bytes());
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let refused_error = client
        .post_json::<Value>(&secret_request(refused))
        .await
        .map(drop)
        .expect_err("a refused connection must fail");
    let chunk_error = client
        .post_json::<Value>(&secret_request(address))
        .await
        .map(drop)
        .expect_err("a body cut short must fail");
    server.await.expect("server should finish");

    // Assert
    for error in [refused_error, chunk_error] {
        assert!(matches!(error, TransportError::Transport(_)), "{error:?}");
        let chain = error_chain(&error);
        assert!(!chain.contains("query-secret"), "{chain}");
        assert!(!chain.contains("header-secret"), "{chain}");
    }
}

#[test]
fn rejects_success_chunk_that_exceeds_remaining_capacity() {
    // Arrange
    let mut body = vec![0; SUCCESS_BODY_LIMIT_BYTES - 1];

    // Act
    let error = append_success_chunk(&mut body, &[0, 1])
        .expect_err("chunk exceeding the limit should fail");

    // Assert
    assert!(matches!(error, TransportError::ResponseBodyTooLarge));
}

#[test]
fn wraps_transport_error_with_its_source() {
    // Arrange
    let source = io::Error::other("connection reset");

    // Act
    let error = TransportError::transport(source);

    // Assert
    assert_eq!(
        error.to_string(),
        "model API transport failed: connection reset"
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
    let transport_error = TransportError::transport(io::Error::other("offline"));
    let oversized = TransportError::ResponseBodyTooLarge;
    let invalid = TransportError::InvalidResponse(
        serde_json::from_str::<Value>("{").expect_err("truncated JSON should fail"),
    );

    // Act
    let error = transport_error.into_model_error("Provider", &[]);
    let oversized = oversized.into_model_error("Provider", &[]);
    let invalid = invalid.into_model_error("Provider", &[]);

    // Assert
    assert_eq!(error.error_class(), model::ErrorClass::Transport);
    assert_eq!(error.http_status(), None);
    assert_eq!(
        error.to_string(),
        "model request failed: model API transport failed: offline"
    );
    assert_eq!(oversized.error_class(), model::ErrorClass::ResponseTooLarge);
    assert_eq!(
        invalid.error_class(),
        model::ErrorClass::InvalidProviderResponse
    );
    assert_eq!(error.retry_after(), None);
    assert!(!error.is_quota_exceeded());
    assert_eq!(oversized.retry_after(), None);
}

#[test]
fn recognizes_a_spent_quota_by_status_and_rate_limit_body() {
    // Arrange
    let cases = [
        (reqwest::StatusCode::PAYMENT_REQUIRED, "", true),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"code":"insufficient_quota"}}"#,
            true,
        ),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "You Exceeded Your Current Quota, please check your plan",
            true,
        ),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"exceeded_current_quota_error"}}"#,
            true,
        ),
        (reqwest::StatusCode::TOO_MANY_REQUESTS, "slow down", false),
        (
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":{"code":"insufficient_quota"}}"#,
            false,
        ),
    ];

    for (status, body, expected) in cases {
        // Act
        let quota_exceeded = is_quota_exceeded(status, body);

        // Assert
        assert_eq!(quota_exceeded, expected, "{status} {body}");
    }
}

#[tokio::test]
async fn keeps_the_retry_after_of_an_unsuccessful_response() {
    // Arrange
    let response = b"HTTP/1.1 503 Service Unavailable\r\n\
                     Content-Length: 4\r\n\
                     Retry-After: 7\r\n\
                     Connection: close\r\n\r\n\
                     busy"
        .to_vec();
    let (address, server) = serve_once(response);
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let result = client.post_json::<Value>(&request(address)).await;
    server.await.expect("server should finish");
    let error = result
        .expect_err("HTTP error should fail")
        .into_model_error("Provider", &[]);
    let wrapped = error.with_metadata(model::CompletionMetadata::new(
        "stop".to_string(),
        None,
        None,
        None,
        None,
    ));

    // Assert
    assert_eq!(wrapped.http_status(), Some(503));
    assert_eq!(wrapped.retry_after(), Some(Duration::from_secs(7)));
    assert!(!wrapped.is_quota_exceeded());
}

#[tokio::test]
async fn reports_a_spent_quota_rate_limit_without_retrying() {
    // Arrange
    let body = r#"{"error":{"code":"insufficient_quota"}}"#;
    let response = format!(
        "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: \
         close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes();
    let (address, server) = serve_once(response);
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let result = client.post_json::<Value>(&request(address)).await;
    server.await.expect("server should finish");
    let error = result
        .expect_err("HTTP error should fail")
        .into_model_error("Provider", &[]);

    // Assert
    assert_eq!(error.http_status(), Some(429));
    assert!(error.is_quota_exceeded());
}

/// Serves one raw HTTP response to the first request on a local listener.
fn serve_once(response: Vec<u8>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let server = tokio::task::spawn_blocking(move || {
        let (mut stream, _) = listener.accept().expect("server should accept a request");
        let mut request = [0; 2_048];
        assert!(
            stream
                .read(&mut request)
                .expect("server should read request")
                > 0
        );
        let _ = stream.write_all(&response);
    });

    (address, server)
}

#[tokio::test]
async fn truncates_an_oversized_error_body_summary() {
    // Arrange
    let body = "word ".repeat(ERROR_BODY_LIMIT_BYTES);
    let response = format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (address, server) = serve_once(response.into_bytes());
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let result = client.post_json::<Value>(&request(address)).await;
    server.await.expect("server should finish");

    // Assert
    let error = result.expect_err("HTTP error should fail");
    assert!(
        matches!(
            &error,
            TransportError::Http { body, status, .. }
                if *status == reqwest::StatusCode::BAD_REQUEST
                    && body.starts_with("word word")
                    && body.ends_with(" ...")
                    && body.len() <= ERROR_BODY_LIMIT_BYTES + 4
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn rejects_a_success_body_whose_declared_length_exceeds_the_limit() {
    // Arrange
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        SUCCESS_BODY_LIMIT_BYTES + 1
    );
    let (address, server) = serve_once(response.into_bytes());
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let result = client.post_json::<Value>(&request(address)).await;
    server.await.expect("server should finish");

    // Assert
    assert!(matches!(result, Err(TransportError::ResponseBodyTooLarge)));
}

#[test]
fn context_overflow_markers_match_case_insensitively() {
    // Arrange
    let shared = "Error code: CONTEXT_LENGTH_EXCEEDED";
    let provider = "Your input Exceeds The Context Window of this model";

    // Act / Assert
    assert!(has_context_overflow_marker(shared, &[]));
    assert!(has_context_overflow_marker(
        provider,
        &["exceeds the context window".to_string()]
    ));
    assert!(!has_context_overflow_marker(provider, &[]));
}

#[test]
fn streaming_transport_keeps_the_whole_request_bound() {
    // Arrange / Act
    let transport = HttpTransport::streaming(Duration::from_secs(7), Duration::from_secs(70));

    // Assert
    assert_eq!(transport.request_timeout, Duration::from_secs(70));
    assert_eq!(transport.retry_delay, RETRY_DELAY);
    assert_eq!(transport.max_retry_delay, MAX_RETRY_DELAY);
}

#[test]
fn retry_after_reads_bounded_delta_seconds() {
    // Arrange
    let cases: [(&[u8], Option<Duration>); 6] = [
        (b"7", Some(Duration::from_secs(7))),
        (b"0", Some(Duration::ZERO)),
        (b"18446744073709551615", Some(MAX_PROVIDER_RETRY_DELAY)),
        (b"Wed, 21 Oct 2015 07:28:00 GMT", None),
        (b"soon", None),
        (b"\xff", None),
    ];

    for (value, expected) in cases {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_bytes(value).expect("valid header bytes"),
        );

        // Act
        let delay = retry_after(&headers);

        // Assert
        assert_eq!(delay, expected, "{value:?}");
    }
    assert_eq!(retry_after(&reqwest::header::HeaderMap::new()), None);
}

#[test]
fn rate_limit_retry_delay_uses_provider_delay_and_backoff() {
    // Arrange
    let cases = [
        (None, 0, Duration::from_secs(1)),
        (None, 1, Duration::from_secs(2)),
        (Some(Duration::from_secs(1)), 2, Duration::from_secs(4)),
        (Some(Duration::from_secs(30)), 0, Duration::from_secs(30)),
        (None, 9, MAX_RETRY_DELAY),
    ];

    for (provider_delay, retry, expected) in cases {
        // Act
        let delay = rate_limit_retry_delay(provider_delay, retry, RETRY_DELAY, MAX_RETRY_DELAY);

        // Assert
        assert_eq!(delay, expected, "{provider_delay:?} {retry}");
    }
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
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let body = client
        .post_json::<Value>(&request(address))
        .await
        .expect("retry should recover");
    server.await.expect("retry server should finish");

    // Assert
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
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
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let body = client
        .post_json::<Value>(&request(address))
        .await
        .expect("transport retry should recover");
    server.await.expect("retry server should finish");

    // Assert
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

#[tokio::test]
async fn timed_out_request_is_reported_without_a_retry() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("timeout listener should bind");
    let address = listener
        .local_addr()
        .expect("timeout listener should have an address");
    let (release, released) = std::sync::mpsc::channel::<()>();
    let server = tokio::task::spawn_blocking(move || {
        listener
            .set_nonblocking(true)
            .expect("listener should poll without blocking");
        let mut accepted = Vec::new();
        while released.try_recv().is_err() {
            match listener.accept() {
                Ok((stream, _)) => accepted.push(stream),
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "listener failed");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }

        accepted.len()
    });
    let client = transport(Duration::from_millis(50));

    // Act
    let error = client
        .post_json::<Value>(&secret_request(address))
        .await
        .map(|_| ())
        .expect_err("a stalled server must time out");
    tokio::time::sleep(Duration::from_millis(200)).await;
    release
        .send(())
        .expect("server should be waiting for release");
    let connections = server.await.expect("timeout server should finish");

    // Assert
    assert!(
        matches!(
            &error,
            TransportError::Transport(source)
                if source
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout)
        ),
        "expected a timeout, got {error}"
    );
    assert!(!error_chain(&error).contains("query-secret"));
    assert_eq!(connections, 1, "a timeout must not be retried");
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
        for attempt in 0..=MAX_RATE_LIMIT_RETRIES {
            // Only the final response's `Retry-After` should be surfaced.
            let retry_after = u8::from(attempt == MAX_RATE_LIMIT_RETRIES);
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
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 64\r\nRetry-After: \
                 {retry_after}\r\nConnection: close\r\n\r\npartial error body"
            );
            stream
                .write_all(response.as_bytes())
                .expect("truncated-response server should write the response");
        }
    });
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let result = client.post_json::<Value>(&request(address)).await;
    server
        .await
        .expect("truncated-response server should finish");

    // Assert
    let error = result.expect_err("truncated HTTP error response should fail");
    assert!(matches!(
        &error,
        TransportError::Http { status, .. }
            if *status == reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    assert!(error.to_string().contains("error body read failed"));
    let source = std::error::Error::source(&error)
        .and_then(<dyn std::error::Error>::downcast_ref::<reqwest::Error>)
        .expect("HTTP failure should retain its status-bearing source");
    assert_eq!(
        source.status(),
        Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
    );
    let error = error.into_model_error("Provider", &[]);
    assert_eq!(error.retry_after(), Some(Duration::from_secs(1)));
    assert!(!error.is_quota_exceeded());
}

#[tokio::test]
async fn reports_error_body_read_failure_without_partial_body() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have an address");
    let server = tokio::task::spawn_blocking(move || {
        let (mut stream, _) = listener.accept().expect("server should accept a request");
        let mut request = [0; 2_048];
        assert!(
            stream
                .read(&mut request)
                .expect("server should read request")
                > 0
        );
        stream
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\n\
                  Content-Length: 64\r\n\
                  Connection: close\r\n\r\n",
            )
            .expect("server should write headers");
    });
    let client = transport(REQUEST_TIMEOUT);

    // Act
    let result = client.post_json::<Value>(&request(address)).await;
    server.await.expect("server should finish");
    let error = result.expect_err("empty truncated response should fail");

    // Assert
    assert!(matches!(
        &error,
        TransportError::Http { status, .. }
            if *status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
    ));
    assert!(error.to_string().contains("[error body read failed:"));
}
