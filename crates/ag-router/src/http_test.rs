use std::io;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use serde_json::Value;

use super::{
    HttpTransport, MAX_PROVIDER_RETRY_DELAY, MAX_RATE_LIMIT_RETRIES, MAX_RETRY_DELAY,
    ProviderRequest, RETRY_DELAY, SUCCESS_BODY_LIMIT_BYTES, TransportError, append_success_chunk,
    has_context_overflow_marker, rate_limit_retry_delay,
};
use crate::model;

const REQUEST_TIMEOUT: Duration = Duration::from_mins(3);

/// Returns a transport with millisecond retry delays.
fn transport(request_timeout: Duration) -> HttpTransport {
    let _ = rustls::crypto::ring::default_provider().install_default();

    HttpTransport {
        client: reqwest::Client::new(),
        max_retry_delay: Duration::from_millis(5),
        request_timeout,
        retry_delay: Duration::from_millis(1),
    }
}

/// Returns an authenticated request with an empty body to `address`.
fn request(address: std::net::SocketAddr) -> ProviderRequest<'static> {
    ProviderRequest::new(
        Some("test-key"),
        format!("http://{address}"),
        serde_json::json!({}),
    )
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

    // Act
    let error = transport_error.into_model_error("Provider", &[]);
    let oversized = oversized.into_model_error("Provider", &[]);

    // Assert
    assert_eq!(error.error_class(), model::ErrorClass::Transport);
    assert_eq!(error.http_status(), None);
    assert_eq!(
        error.to_string(),
        "model request failed: model API transport failed: offline"
    );
    assert_eq!(oversized.error_class(), model::ErrorClass::ResponseTooLarge);
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
        &["exceeds the context window"]
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
fn rate_limit_retry_delay_uses_bounded_headers_and_backoff() {
    // Arrange
    let mut headers = reqwest::header::HeaderMap::new();

    // Act and Assert
    assert_eq!(
        rate_limit_retry_delay(&headers, 0, RETRY_DELAY, MAX_RETRY_DELAY),
        Duration::from_secs(1)
    );
    assert_eq!(
        rate_limit_retry_delay(&headers, 1, RETRY_DELAY, MAX_RETRY_DELAY),
        Duration::from_secs(2)
    );
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "1".parse().expect("valid header"),
    );
    assert_eq!(
        rate_limit_retry_delay(&headers, 2, RETRY_DELAY, MAX_RETRY_DELAY),
        Duration::from_secs(4)
    );
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "30".parse().expect("valid header"),
    );
    assert_eq!(
        rate_limit_retry_delay(&headers, 0, RETRY_DELAY, MAX_RETRY_DELAY),
        Duration::from_secs(30)
    );
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "99".parse().expect("valid header"),
    );
    assert_eq!(
        rate_limit_retry_delay(&headers, 0, RETRY_DELAY, MAX_RETRY_DELAY),
        MAX_PROVIDER_RETRY_DELAY
    );
    headers.remove(reqwest::header::RETRY_AFTER);
    assert_eq!(
        rate_limit_retry_delay(&headers, 9, RETRY_DELAY, MAX_RETRY_DELAY),
        MAX_RETRY_DELAY
    );
    headers.insert(
        reqwest::header::RETRY_AFTER,
        reqwest::header::HeaderValue::from_bytes(b"invalid").expect("valid header bytes"),
    );
    assert_eq!(
        rate_limit_retry_delay(&headers, 0, RETRY_DELAY, MAX_RETRY_DELAY),
        RETRY_DELAY
    );
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
        .post_json::<Value>(&request(address))
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
