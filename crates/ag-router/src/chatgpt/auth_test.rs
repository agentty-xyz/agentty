use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::ChatGptAuth;
use crate::chatgpt::credentials::ChatGptCredentials;
use crate::chatgpt::error::ChatGptAuthError;
use crate::credential::BearerCredential;

const TOKEN_PATH: &str = "/api/accounts/oauth/token";

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn credentials(expires_at: u64) -> ChatGptCredentials {
    ChatGptCredentials {
        access_token: "stored-access".to_string(),
        client_id: "client-1".to_string(),
        email: Some("ada@example.com".to_string()),
        expires_at,
        ext_agent_host_id: "urn:uuid:host".to_string(),
        id_token: "stored-id".to_string(),
        refresh_token: "stored-refresh".to_string(),
        scopes: vec!["chatgpt.tokens.use.direct".to_string()],
    }
}

fn form_pairs(body: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

#[tokio::test]
async fn returns_a_fresh_stored_token_without_refreshing() {
    // Arrange
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("chatgpt.json");
    credentials(unix_now() + 3600)
        .save(&path)
        .expect("credentials save");
    let auth = ChatGptAuth::new(&path).with_issuer(server.uri());

    // Act
    let token = auth.bearer_token().await.expect("token is fresh");

    // Assert
    assert_eq!(token, "stored-access");
    assert_eq!(auth.path(), path);
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(requests.is_empty());
}

#[tokio::test]
async fn refreshes_an_expired_token_saves_the_rotation_and_caches_it() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "refreshed-access",
            "expires_in": 3600,
            "refresh_token": "rotated-refresh"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("chatgpt.json");
    credentials(0).save(&path).expect("credentials save");
    let auth = ChatGptAuth::new(&path).with_issuer(server.uri());

    // Act
    let first = auth.access_token().await.expect("token refreshes");
    let saved = ChatGptCredentials::load(&path).expect("refreshed credentials load");
    fs::remove_file(&path).expect("remove credential file");
    let second = auth.access_token().await.expect("token is cached");

    // Assert
    assert_eq!(first, "refreshed-access");
    assert_eq!(second, "refreshed-access");
    assert_eq!(saved.access_token, "refreshed-access");
    assert_eq!(saved.refresh_token, "rotated-refresh");
    assert_eq!(saved.id_token, "stored-id");
    assert_eq!(saved.client_id, "client-1");
    assert!(saved.expires_at > unix_now());
    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(
        form_pairs(&requests[0].body),
        [
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("client_id".to_string(), "client-1".to_string()),
            ("refresh_token".to_string(), "stored-refresh".to_string()),
            (
                "resource".to_string(),
                "https://api.openai.com/v1".to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn propagates_a_sign_in_required_refresh_failure() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "refresh_token_reused"})),
        )
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("chatgpt.json");
    credentials(0).save(&path).expect("credentials save");
    let auth = ChatGptAuth::new(&path).with_issuer(server.uri());

    // Act
    let direct = auth.access_token().await;
    let bearer = auth.bearer_token().await;

    // Assert
    assert!(matches!(
        direct,
        Err(ChatGptAuthError::SignInRequired { ref code }) if code == "refresh_token_reused"
    ));
    let bearer_error = bearer.expect_err("bearer token fails");
    assert!(matches!(
        bearer_error.downcast_ref::<ChatGptAuthError>(),
        Some(ChatGptAuthError::SignInRequired { .. })
    ));
    assert_eq!(
        ChatGptCredentials::load(&path)
            .expect("credentials stay")
            .refresh_token,
        "stored-refresh"
    );
}

#[tokio::test]
async fn reports_missing_credentials_and_an_unusable_lock_file() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.json");
    let blocking_file = directory.path().join("file");
    fs::write(&blocking_file, "").expect("write blocking file");
    let locked_dir = directory.path().join("locked.json");
    fs::create_dir(locked_dir.with_extension("lock")).expect("create lock directory");

    // Act
    let missing_result = ChatGptAuth::new(&missing).access_token().await;
    let parent_result = ChatGptAuth::new(blocking_file.join("chatgpt.json"))
        .access_token()
        .await;
    let lock_result = ChatGptAuth::new(&locked_dir).access_token().await;

    // Assert
    assert!(matches!(
        missing_result,
        Err(ChatGptAuthError::NotSignedIn { ref path }) if *path == missing
    ));
    assert!(matches!(parent_result, Err(ChatGptAuthError::Io { .. })));
    assert!(matches!(
        lock_result,
        Err(ChatGptAuthError::Io { ref path, .. }) if *path == locked_dir.with_extension("lock")
    ));
}
