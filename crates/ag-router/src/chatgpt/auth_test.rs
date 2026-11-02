use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        .expect(1)
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
        Some(ChatGptAuthError::SignInRequired { code }) if code == "signed_out"
    ));
    let stored = ChatGptCredentials::load(&path).expect("registration stays");
    assert!(!stored.is_signed_in());
    assert_eq!(stored.access_token, "");
    assert_eq!(stored.client_id, "client-1");
    assert_eq!(stored.email.as_deref(), Some("ada@example.com"));
}

#[tokio::test]
async fn refresh_completes_and_stores_the_rotation_when_the_request_is_cancelled() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "access_token": "rotated-access",
                    "expires_in": 3600,
                    "refresh_token": "rotated-refresh"
                }))
                .set_delay(Duration::from_millis(300)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("chatgpt.json");
    credentials(0).save(&path).expect("credentials save");
    let auth = ChatGptAuth::new(&path).with_issuer(server.uri());

    // Act
    let cancelled = tokio::time::timeout(Duration::from_millis(50), auth.access_token()).await;
    let token = auth.access_token().await.expect("stored rotation is used");

    // Assert
    assert!(cancelled.is_err());
    assert_eq!(token, "rotated-access");
    assert_eq!(
        ChatGptCredentials::load(&path)
            .expect("rotation stored")
            .refresh_token,
        "rotated-refresh"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn creates_the_credential_directory_and_lock_file_private_to_the_owner() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_directory = directory.path().join("auth");
    let path = auth_directory.join("chatgpt.json");

    // Act
    let result = ChatGptAuth::new(&path).access_token().await;

    // Assert
    assert!(matches!(result, Err(ChatGptAuthError::NotSignedIn { .. })));
    let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&auth_directory), 0o700);
    assert_eq!(mode(&path.with_extension("lock")), 0o600);
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

#[tokio::test]
async fn keeps_the_sign_in_when_a_refresh_fails_temporarily() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(serde_json::json!({"error": "unavailable"})),
        )
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("chatgpt.json");
    credentials(0).save(&path).expect("credentials save");
    let auth = ChatGptAuth::new(&path).with_issuer(server.uri());

    // Act
    let result = auth.access_token().await;

    // Assert
    assert!(matches!(
        result,
        Err(ChatGptAuthError::TokenRequest { status: 503, .. })
    ));
    assert_eq!(
        ChatGptCredentials::load(&path)
            .expect("sign-in stays")
            .refresh_token,
        "stored-refresh"
    );
}
