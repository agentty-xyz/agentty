use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::ChatGptLogin;
use crate::chatgpt::credentials::ChatGptCredentials;
use crate::chatgpt::error::ChatGptAuthError;

const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const TOKEN_PATH: &str = "/api/accounts/oauth/token";

fn previous(id_token: &str) -> ChatGptCredentials {
    ChatGptCredentials {
        access_token: "old-access".to_string(),
        client_id: "client-registered".to_string(),
        email: None,
        expires_at: 0,
        ext_agent_host_id: "urn:uuid:saved-host".to_string(),
        id_token: id_token.to_string(),
        refresh_token: "old-refresh".to_string(),
        scopes: vec!["chatgpt.tokens.use.direct".to_string()],
    }
}

fn query_pairs(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .expect("valid authorize URL")
        .query_pairs()
        .into_owned()
        .collect()
}

fn form_pairs(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

async fn token_server(scope: &str) -> MockServer {
    let server = MockServer::start().await;
    let claims = URL_SAFE_NO_PAD.encode(r#"{"email":"ada@example.com"}"#);
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access",
            "expires_in": 3600,
            "id_token": format!("header.{claims}.signature"),
            "refresh_token": "refresh",
            "scope": scope
        })))
        .mount(&server)
        .await;

    server
}

#[test]
fn first_sign_in_registers_the_app_with_a_pkce_challenge() {
    // Arrange
    let login = ChatGptLogin::new(REDIRECT_URI, None).with_issuer("https://issuer.test/");

    // Act
    let url = login.authorize_url();

    // Assert
    assert!(url.starts_with("https://issuer.test/api/accounts/authorize?"));
    let query = query_pairs(&url);
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(login.code_verifier.as_bytes()));
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["agent_name_hint"], "Agentty");
    assert!(!query.contains_key("id_token_hint"));
    assert!(query["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["redirect_uri"], REDIRECT_URI);
    assert!(
        query["scope"]
            .split(' ')
            .any(|scope| scope == "chatgpt.tokens.use.direct")
    );
    assert_eq!(query["resource"], "https://api.openai.com/v1");
    assert_eq!(query["state"], login.state);
    assert_eq!(query["nonce"], login.nonce);
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["code_challenge"], expected_challenge);
}

#[test]
fn reauthorization_reuses_the_registered_client() {
    // Arrange
    let with_hint = ChatGptLogin::new(REDIRECT_URI, Some(previous("saved-id-token")));
    let without_hint = ChatGptLogin::new(REDIRECT_URI, Some(previous("")));

    // Act
    let with_hint_query = query_pairs(&with_hint.authorize_url());
    let without_hint_query = query_pairs(&without_hint.authorize_url());

    // Assert
    assert_eq!(with_hint_query["client_id"], "client-registered");
    assert_eq!(with_hint_query["id_token_hint"], "saved-id-token");
    assert_eq!(with_hint_query["ext_agent_host_id"], "urn:uuid:saved-host");
    assert!(!with_hint_query.contains_key("agent_name_hint"));
    assert_eq!(without_hint_query["client_id"], "client-registered");
    assert!(!without_hint_query.contains_key("id_token_hint"));
    assert!(!without_hint_query.contains_key("agent_name_hint"));
}

#[tokio::test]
async fn finish_rejects_callbacks_that_do_not_complete_this_sign_in() {
    // Arrange
    let registration = ChatGptLogin::new(REDIRECT_URI, None);
    let reauthorization = ChatGptLogin::new(REDIRECT_URI, Some(previous("id")));
    let registration_state = registration.state.clone();
    let reauthorization_state = reauthorization.state.clone();

    // Act
    let wrong_state = registration.finish("state=other&code=abc").await;
    let denied = registration
        .finish(&format!("state={registration_state}&error=access_denied"))
        .await;
    let missing_code = registration
        .finish(&format!("state={registration_state}"))
        .await;
    let missing_client = registration
        .finish(&format!("state={registration_state}&code=abc"))
        .await;
    let other_client = reauthorization
        .finish(&format!(
            "state={reauthorization_state}&code=abc&client_id=client-other"
        ))
        .await;

    // Assert
    assert!(matches!(
        wrong_state,
        Err(ChatGptAuthError::InvalidCallback {
            reason: "state does not match this sign-in"
        })
    ));
    assert!(matches!(
        denied,
        Err(ChatGptAuthError::Denied { ref error }) if error == "access_denied"
    ));
    assert!(matches!(
        missing_code,
        Err(ChatGptAuthError::InvalidCallback {
            reason: "missing authorization code"
        })
    ));
    assert!(matches!(
        missing_client,
        Err(ChatGptAuthError::InvalidCallback {
            reason: "app registration returned no client_id"
        })
    ));
    assert!(matches!(
        other_client,
        Err(ChatGptAuthError::InvalidCallback {
            reason: "client_id differs from the registered app"
        })
    ));
}

#[tokio::test]
async fn finish_exchanges_the_code_for_the_issued_client() {
    // Arrange
    let server = token_server("openid chatgpt.tokens.use.direct").await;
    let login = ChatGptLogin::new(REDIRECT_URI, None).with_issuer(server.uri());
    let callback = format!("?state={}&code=auth-code&client_id=client-new", login.state);

    // Act
    let credentials = login.finish(&callback).await.expect("sign-in completes");

    // Assert
    assert_eq!(credentials.access_token, "access");
    assert_eq!(credentials.client_id, "client-new");
    assert_eq!(credentials.email.as_deref(), Some("ada@example.com"));
    assert_eq!(credentials.ext_agent_host_id, login.ext_agent_host_id);
    assert_eq!(credentials.refresh_token, "refresh");
    let requests = server.received_requests().await.expect("recording enabled");
    let form = form_pairs(&requests[0].body);
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], "client-new");
    assert_eq!(form["code"], "auth-code");
    assert_eq!(form["code_verifier"], login.code_verifier);
    assert_eq!(form["redirect_uri"], REDIRECT_URI);
    assert_eq!(form["resource"], "https://api.openai.com/v1");
}

#[tokio::test]
async fn finish_reauthorizes_with_the_saved_client() {
    // Arrange
    let server = token_server("chatgpt.tokens.use.direct").await;
    let login = ChatGptLogin::new(REDIRECT_URI, Some(previous("id"))).with_issuer(server.uri());
    let callback = format!(
        "state={}&code=auth-code&client_id=client-registered",
        login.state
    );

    // Act
    let credentials = login.finish(&callback).await.expect("sign-in completes");

    // Assert
    assert_eq!(credentials.client_id, "client-registered");
    assert_eq!(credentials.ext_agent_host_id, "urn:uuid:saved-host");
    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(
        form_pairs(&requests[0].body)["client_id"],
        "client-registered"
    );
}

#[tokio::test]
async fn finish_requires_the_plan_scope() {
    // Arrange
    let server = token_server("openid profile").await;
    let login = ChatGptLogin::new(REDIRECT_URI, Some(previous("id"))).with_issuer(server.uri());
    let callback = format!("state={}&code=auth-code", login.state);

    // Act
    let result = login.finish(&callback).await;

    // Assert
    assert!(matches!(result, Err(ChatGptAuthError::MissingPlanScope)));
}
