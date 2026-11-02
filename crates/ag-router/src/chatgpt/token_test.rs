use std::net::TcpListener;

use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{TokenResponse, request_tokens};
use crate::chatgpt::error::ChatGptAuthError;
use crate::http::ERROR_BODY_LIMIT_BYTES;

const TOKEN_PATH: &str = "/api/accounts/oauth/token";

fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();

    reqwest::Client::new()
}

async fn token_server(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(response)
        .mount(&server)
        .await;

    server
}

async fn request(server: &MockServer) -> Result<TokenResponse, ChatGptAuthError> {
    request_tokens(
        &http_client(),
        &server.uri(),
        &[("grant_type", "refresh_token")],
    )
    .await
}

#[tokio::test]
async fn posts_the_form_and_decodes_a_successful_grant() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string(
            "grant_type=refresh_token&refresh_token=a+b%26c",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access",
            "expires_in": 3600,
            "id_token": "id",
            "refresh_token": "refresh",
            "scope": "openid chatgpt.tokens.use.direct"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Act
    let grant = request_tokens(
        &http_client(),
        &format!("{}/", server.uri()),
        &[("grant_type", "refresh_token"), ("refresh_token", "a b&c")],
    )
    .await
    .expect("grant decodes");

    // Assert
    assert_eq!(grant.access_token, "access");
    assert_eq!(grant.expires_in, 3600);
    assert_eq!(grant.id_token.as_deref(), Some("id"));
    assert_eq!(grant.refresh_token.as_deref(), Some("refresh"));
    assert_eq!(
        grant.scope.as_deref(),
        Some("openid chatgpt.tokens.use.direct")
    );
}

#[tokio::test]
async fn rejects_a_successful_response_with_an_invalid_body() {
    // Arrange
    let server = token_server(ResponseTemplate::new(200).set_body_string("{}")).await;

    // Act
    let result = request(&server).await;

    // Assert
    assert!(matches!(
        result,
        Err(ChatGptAuthError::InvalidTokenResponse(_))
    ));
}

#[tokio::test]
async fn maps_dead_refresh_token_codes_to_sign_in_required() {
    // Arrange
    let codes = [
        "invalid_grant",
        "invalid_refresh_token",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
        "token_expired",
    ];

    for code in codes {
        let server = token_server(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": code})),
        )
        .await;

        // Act
        let result = request(&server).await;

        // Assert
        assert!(
            matches!(&result, Err(ChatGptAuthError::SignInRequired { code: returned }) if returned == code),
            "{code}"
        );
    }
}

#[tokio::test]
async fn reports_other_failures_with_a_bounded_body() {
    // Arrange
    let other_code = token_server(
        ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": "server_error"})),
    )
    .await;
    let long_body = token_server(
        ResponseTemplate::new(502).set_body_string("x".repeat(ERROR_BODY_LIMIT_BYTES + 10)),
    )
    .await;

    // Act
    let other_code_result = request(&other_code).await;
    let long_body_result = request(&long_body).await;

    // Assert
    assert!(matches!(
        other_code_result,
        Err(ChatGptAuthError::TokenRequest { status: 500, ref body })
            if body == r#"{"error":"server_error"}"#
    ));
    assert!(matches!(
        long_body_result,
        Err(ChatGptAuthError::TokenRequest { status: 502, ref body })
            if body.len() == ERROR_BODY_LIMIT_BYTES
    ));
}

#[tokio::test]
async fn reports_an_unreachable_token_endpoint_as_transport_failure() {
    // Arrange
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    let issuer = format!("http://{}", listener.local_addr().expect("local address"));
    drop(listener);

    // Act
    let result = request_tokens(&http_client(), &issuer, &[]).await;

    // Assert
    assert!(matches!(result, Err(ChatGptAuthError::Transport(_))));
}
