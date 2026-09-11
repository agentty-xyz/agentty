use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{kimi, request};
use crate::chat_completion::{ERROR_BODY_LIMIT_BYTES, SUCCESS_BODY_LIMIT_BYTES};
use crate::{model, schema_contract};

#[tokio::test]
async fn rejects_structured_response_stopped_for_length() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("extract the name"))
        .await
        .expect_err("truncated response should fail");

    // Assert
    assert!(matches!(
        error,
        model::ModelError::IncompleteResponse { reason } if reason == "length"
    ));
}

#[tokio::test]
async fn bounds_incomplete_response_reason() {
    // Arrange
    let server = MockServer::start().await;
    let finish_reason = "x".repeat(1024);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": finish_reason.clone(),
                "message": {"content": r#"{"name":"Ada"}"#}
            }]
        })))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("extract the name"))
        .await
        .expect_err("incomplete response should fail");

    // Assert
    assert!(matches!(
        error,
        model::ModelError::IncompleteResponse { reason }
            if reason == schema_contract::bounded_diagnostic(finish_reason)
    ));
}

#[tokio::test]
async fn rejects_oversized_success_body_before_decoding() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_bytes(vec![b'x'; SUCCESS_BODY_LIMIT_BYTES + 1]),
        )
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("oversized successful response should fail");

    // Assert
    assert!(matches!(error, model::ModelError::ResponseBodyTooLarge));
}

#[tokio::test]
async fn rejects_oversized_response_content() {
    // Arrange
    let server = MockServer::start().await;
    let content = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": content}
            }]
        })))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("oversized response content should fail");

    // Assert
    assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
}

#[tokio::test]
async fn returns_request_error_for_http_failure() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid API key"}
        })))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("HTTP failure should fail");

    // Assert
    assert_eq!(
        error.to_string(),
        "model request failed: Kimi returned HTTP 401 Unauthorized: \
         {\"error\":{\"message\":\"invalid API key\"}}"
    );
    let provider_error =
        std::error::Error::source(&error).expect("HTTP failure should retain its provider error");
    let source = provider_error
        .source()
        .and_then(<dyn std::error::Error>::downcast_ref::<reqwest::Error>)
        .expect("HTTP failure should retain its reqwest source");
    assert_eq!(source.status(), Some(reqwest::StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn bounds_http_error_body() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500).set_body_string("x".repeat(ERROR_BODY_LIMIT_BYTES + 1)),
        )
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("HTTP failure should fail");
    let message = error.to_string();

    // Assert
    assert_eq!(
        message,
        format!(
            "model request failed: Kimi returned HTTP 500 Internal Server Error: {} ...",
            "x".repeat(ERROR_BODY_LIMIT_BYTES)
        )
    );
}

#[tokio::test]
async fn returns_request_error_for_malformed_response() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .mount(&server)
        .await;
    let model = kimi(&server);

    // Act
    let error = model
        .complete(request("hello"))
        .await
        .expect_err("malformed response should fail");

    // Assert
    assert!(matches!(error, model::ModelError::Request(_)));
}
