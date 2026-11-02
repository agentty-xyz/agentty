use std::env;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::Mutex;

use ag_harness::provider::{ChatGptCredentials, ChatGptSignInError};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Browser, login};

/// Browser that answers the sign-in like an approving user: it requests
/// the callback URL with the given query suffix and the sign-in's state.
struct ApprovingBrowser {
    opened: Mutex<Vec<String>>,
    page: Mutex<Option<std::thread::JoinHandle<String>>>,
    query: &'static str,
}

impl ApprovingBrowser {
    fn new(query: &'static str) -> Self {
        Self {
            opened: Mutex::new(Vec::new()),
            page: Mutex::new(None),
            query,
        }
    }

    fn page(&self) -> String {
        self.page
            .lock()
            .expect("page lock")
            .take()
            .map_or_default(|handle| handle.join().expect("browser thread"))
    }
}

impl Browser for ApprovingBrowser {
    fn open(&self, url: &str) {
        self.opened
            .lock()
            .expect("opened lock")
            .push(url.to_string());
        let query = url.split_once('?').map_or_default(|(_, query)| query);
        let parameter = |name: &str| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == name)
                .map_or_default(|(_, value)| value.into_owned())
        };
        let redirect = parameter("redirect_uri");
        let state = parameter("state");
        let suffix = self.query;
        let handle = std::thread::spawn(move || {
            let address = redirect
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let mut stream = TcpStream::connect(&address).expect("callback listener");
            // A stray request first, which the listener answers with 404.
            write!(
                stream,
                "GET /favicon.ico HTTP/1.1\r\nhost: {address}\r\n\r\n"
            )
            .expect("stray request");
            let mut ignored = String::new();
            let _ = stream.read_to_string(&mut ignored);
            let mut stream = TcpStream::connect(&address).expect("callback listener");
            write!(
                stream,
                "GET /auth/callback?state={state}{suffix} HTTP/1.1\r\nhost: {address}\r\n\r\n"
            )
            .expect("callback request");
            let mut page = String::new();
            let _ = stream.read_to_string(&mut page);

            page
        });
        *self.page.lock().expect("page lock") = Some(handle);
    }
}

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

fn environment(
    auth_file: String,
    server: &MockServer,
) -> impl FnMut(&str) -> Result<String, env::VarError> {
    let issuer = server.uri();
    move |name| match name {
        "CHATGPT_AUTH_FILE" => Ok(auth_file.clone()),
        "CHATGPT_AUTH_URL" => Ok(issuer.clone()),
        _ => Err(env::VarError::NotPresent),
    }
}

#[tokio::test]
async fn login_stores_the_approved_sign_in() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("auth/chatgpt.json");
    let browser = ApprovingBrowser::new("&code=code&client_id=oaiapp_issued");
    let mut output = Vec::new();

    // Act
    login(
        environment(auth_file.to_string_lossy().into_owned(), &server),
        &browser,
        &mut output,
        0,
    )
    .await
    .expect("sign-in should succeed");

    // Assert
    let stored = ChatGptCredentials::load(&auth_file).expect("stored sign-in");
    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(stored.client_id, "oaiapp_issued");
    assert_eq!(stored.access_token, "access");
    assert!(output.contains("Signed in as user@example.com"));
    assert!(output.contains(&server.uri()));
    assert!(browser.page().contains("Signed in to Agentty"));
    assert!(browser.opened.lock().expect("opened lock")[0].contains("agent_name_hint=Agentty"));
}

#[tokio::test]
async fn login_reuses_the_registered_app_and_reports_denial() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    ChatGptCredentials {
        access_token: "old".to_string(),
        client_id: "oaiapp_saved".to_string(),
        email: None,
        expires_at: 0,
        ext_agent_host_id: "urn:uuid:host".to_string(),
        id_token: "saved-id-token".to_string(),
        refresh_token: "old-refresh".to_string(),
        scopes: vec!["chatgpt.tokens.use.direct".to_string()],
    }
    .save(&auth_file)
    .expect("saved sign-in");
    let browser = ApprovingBrowser::new("&error=access_denied");
    let mut output = Vec::new();

    // Act
    let result = login(
        environment(auth_file.to_string_lossy().into_owned(), &server),
        &browser,
        &mut output,
        0,
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(ChatGptSignInError::Auth(
            ag_harness::provider::ChatGptAuthError::Denied { .. }
        ))
    ));
    assert!(browser.page().contains("sign-in failed"));
    let opened = browser.opened.lock().expect("opened lock")[0].clone();
    assert!(opened.contains("client_id=oaiapp_saved"));
    assert!(!opened.contains("saved-id-token"));
    assert!(!String::from_utf8_lossy(&output).contains("saved-id-token"));
    assert_eq!(
        ChatGptCredentials::load(&auth_file)
            .expect("unchanged sign-in")
            .access_token,
        "old"
    );
}

#[tokio::test]
async fn login_rejects_an_unreadable_sign_in_and_falls_back_to_a_free_port() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let invalid = directory.path().join("invalid.json");
    std::fs::write(&invalid, "not json").expect("invalid file");
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("occupied port");
    let port = occupied.local_addr().expect("address").port();
    let server = token_server().await;
    let browser = ApprovingBrowser::new("&code=code&client_id=oaiapp_issued");
    let auth_file = directory.path().join("chatgpt.json");
    let mut output = Vec::new();

    // Act
    let invalid_result = login(
        environment(invalid.to_string_lossy().into_owned(), &server),
        &browser,
        &mut Vec::new(),
        port,
    )
    .await;
    let fallback = login(
        environment(auth_file.to_string_lossy().into_owned(), &server),
        &browser,
        &mut output,
        port,
    )
    .await;
    let unlocated = login(
        |_: &str| Err(env::VarError::NotPresent),
        &browser,
        &mut Vec::new(),
        port,
    )
    .await;

    // Assert
    assert!(matches!(
        invalid_result,
        Err(ChatGptSignInError::Auth(
            ag_harness::provider::ChatGptAuthError::InvalidCredentials { .. }
        ))
    ));
    assert!(fallback.is_ok());
    assert!(!String::from_utf8_lossy(&output).contains(&format!("127.0.0.1%3A{port}")));
    assert!(matches!(unlocated, Err(ChatGptSignInError::AuthFile(_))));
}
