use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ag_harness::provider::ChatGptCredentials;
use ag_session::{ChatGptSignInOutcome, ChatGptSignInUrlHandler};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::sign_in_chatgpt;

/// Browser stand-in that records the sign-in URL and answers it like a
/// user: it requests the callback with the sign-in's state and `suffix`.
struct ApprovingBrowser {
    opened: Arc<Mutex<Vec<String>>>,
    page: Arc<Mutex<Option<JoinHandle<String>>>>,
}

impl ApprovingBrowser {
    fn new() -> Self {
        Self {
            opened: Arc::new(Mutex::new(Vec::new())),
            page: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the URL handler that approves with callback query `suffix`.
    fn handler(&self, suffix: &'static str) -> ChatGptSignInUrlHandler {
        let opened = Arc::clone(&self.opened);
        let page = Arc::clone(&self.page);

        Box::new(move |url: String| {
            opened.lock().expect("opened lock").push(url.clone());
            let query = url.split_once('?').map_or_default(|(_, query)| query);
            let parameter = |name: &str| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == name)
                    .map_or_default(|(_, value)| value.into_owned())
            };
            let redirect = parameter("redirect_uri");
            let state = parameter("state");
            let handle = std::thread::spawn(move || {
                let address = redirect
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let mut stream = TcpStream::connect(&address).expect("callback listener");
                write!(
                    stream,
                    "GET /auth/callback?state={state}{suffix} HTTP/1.1\r\nhost: {address}\r\n\r\n"
                )
                .expect("callback request");
                let mut body = String::new();
                let _ = stream.read_to_string(&mut body);

                body
            });
            *page.lock().expect("page lock") = Some(handle);
        })
    }

    fn page(&self) -> String {
        self.page
            .lock()
            .expect("page lock")
            .take()
            .map_or_default(|handle| handle.join().expect("browser thread"))
    }
}

/// Starts an authorization server whose token endpoint grants a plan
/// credential for `user@example.com`.
async fn token_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access",
            "expires_in": 3600,
            "id_token": "header.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20ifQ.signature",
            "refresh_token": "refresh",
            "scope": "openid chatgpt.tokens.use.direct",
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    server
}

/// Builds the environment naming `auth_file` and `server` as the issuer.
fn environment(auth_file: &std::path::Path, server: &MockServer) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "CHATGPT_AUTH_FILE".to_string(),
            auth_file.to_string_lossy().into_owned(),
        ),
        ("CHATGPT_AUTH_URL".to_string(), server.uri()),
    ])
}

#[tokio::test]
async fn sign_in_chatgpt_opens_the_url_and_stores_the_approved_credential() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("auth/chatgpt.json");
    let browser = ApprovingBrowser::new();

    // Act
    let outcome = sign_in_chatgpt(
        &environment(&auth_file, &server),
        browser.handler("&code=code&client_id=oaiapp_issued"),
    )
    .await;

    // Assert
    assert_eq!(
        outcome,
        Ok(ChatGptSignInOutcome {
            email: Some("user@example.com".to_string()),
        })
    );
    let stored = ChatGptCredentials::load(&auth_file).expect("stored sign-in");
    assert_eq!(stored.client_id, "oaiapp_issued");
    assert!(browser.page().contains("Signed in to Agentty"));
    assert!(browser.opened.lock().expect("opened lock")[0].starts_with(&server.uri()));
}

#[tokio::test]
async fn sign_in_chatgpt_reports_denial_and_unlocated_credentials() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    let invalid_file = directory.path().join("invalid.json");
    std::fs::write(&invalid_file, "not json").expect("invalid file");
    let browser = ApprovingBrowser::new();
    let unused_browser = ApprovingBrowser::new();

    // Act
    let denied = sign_in_chatgpt(
        &environment(&auth_file, &server),
        browser.handler("&error=access_denied"),
    )
    .await;
    let unreadable = sign_in_chatgpt(
        &environment(&invalid_file, &server),
        unused_browser.handler(""),
    )
    .await;
    let unlocated = sign_in_chatgpt(&BTreeMap::new(), unused_browser.handler("")).await;

    // Assert
    assert!(denied.is_err_and(|error| !error.is_empty()));
    assert!(browser.page().contains("sign-in failed"));
    assert!(!auth_file.exists());
    assert!(unreadable.is_err());
    assert!(unlocated.is_err());
    assert!(
        unused_browser
            .opened
            .lock()
            .expect("opened lock")
            .is_empty()
    );
}
