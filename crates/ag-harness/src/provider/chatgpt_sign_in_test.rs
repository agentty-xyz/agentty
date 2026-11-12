use std::path::Path;
use std::time::Duration;
use std::{env, fs};

use ag_router::chatgpt::{ChatGptAuthError, ChatGptCredentials};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{ChatGptSignIn, ChatGptSignInError, PendingChatGptSignIn};

async fn token_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access",
            "expires_in": 3600,
            "refresh_token": "refresh",
            "scope": "openid chatgpt.tokens.use.direct"
        })))
        .mount(&server)
        .await;

    server
}

/// Starts a sign-in on a free port that stores to `auth_file` and exchanges
/// codes with `server`.
async fn start(auth_file: &Path, server: &MockServer, timeout: Duration) -> PendingChatGptSignIn {
    let auth_file = auth_file.to_string_lossy().into_owned();
    let issuer = server.uri();

    ChatGptSignIn::from_environment(move |name| match name {
        "CHATGPT_AUTH_FILE" => Ok(auth_file.clone()),
        "CHATGPT_AUTH_URL" => Ok(issuer.clone()),
        _ => Err(env::VarError::NotPresent),
    })
    .expect("auth file located")
    .with_port(0)
    .with_timeout(timeout)
    .start()
    .await
    .expect("sign-in starts")
}

/// Returns the loopback address and `state` of a pending sign-in's URL.
fn callback_target(pending: &PendingChatGptSignIn) -> (String, String) {
    let url = pending.authorize_url();
    let parameter = |name: &str| {
        url.split(['?', '&'])
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .expect("parameter present")
            .to_string()
    };
    let address = parameter("redirect_uri")
        .trim_start_matches("http%3A%2F%2F")
        .split("%2F")
        .next()
        .expect("redirect address")
        .replace("%3A", ":");

    (address, parameter("state"))
}

/// Sends one GET request to the loopback listener and returns the response.
async fn get(address: &str, target: &str) -> String {
    let mut stream = TcpStream::connect(address).await.expect("listener accepts");
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nhost: {address}\r\n\r\n").as_bytes())
        .await
        .expect("request sent");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response).await;

    response
}

#[tokio::test]
async fn complete_ignores_silent_connections_and_stale_callbacks() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory.path().join("chatgpt.json");
    let pending = start(&auth_file, &server, Duration::from_secs(30)).await;
    let (address, state) = callback_target(&pending);
    let completion = tokio::spawn(pending.complete());

    // Act
    let _silent = TcpStream::connect(&address).await.expect("silent client");
    let mut garbled = TcpStream::connect(&address).await.expect("garbled client");
    garbled
        .write_all(b"\xff\xfe\n")
        .await
        .expect("garbled request sent");
    let mut garbled_response = Vec::new();
    let _ = garbled.read_to_end(&mut garbled_response).await;
    let stray = get(&address, "/favicon.ico").await;
    let earlier_callback = get(&address, "/auth/callback?state=earlier&code=old").await;
    let callback = get(
        &address,
        &format!("/auth/callback?state={state}&code=code&client_id=oaiapp_issued"),
    )
    .await;
    let credentials = completion
        .await
        .expect("sign-in task")
        .expect("sign-in succeeds");
    let restarted = start(&auth_file, &server, Duration::from_secs(30)).await;

    // Assert
    assert!(
        restarted
            .authorize_url()
            .contains("client_id=oaiapp_issued")
    );
    assert_eq!(garbled_response, Vec::<u8>::new());
    assert!(stray.starts_with("HTTP/1.1 404"));
    assert!(earlier_callback.starts_with("HTTP/1.1 400"));
    assert!(earlier_callback.contains("no longer active"));
    assert!(callback.contains("Signed in to Agentty"));
    assert_eq!(credentials.client_id, "oaiapp_issued");
    assert_eq!(
        ChatGptCredentials::load(&auth_file)
            .expect("stored sign-in")
            .access_token,
        "access"
    );
}

#[tokio::test]
async fn complete_reports_failure_to_the_browser_when_the_credential_cannot_be_saved() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_directory = directory.path().join("auth");
    let pending = start(
        &auth_directory.join("chatgpt.json"),
        &server,
        Duration::from_secs(30),
    )
    .await;
    let (address, state) = callback_target(&pending);
    fs::write(&auth_directory, "").expect("file blocks the credential directory");
    let completion = tokio::spawn(pending.complete());

    // Act
    let callback = get(
        &address,
        &format!("/auth/callback?state={state}&code=code&client_id=oaiapp_issued"),
    )
    .await;
    let result = completion.await.expect("sign-in task");

    // Assert
    assert!(callback.contains("sign-in failed"));
    assert!(matches!(
        result,
        Err(ChatGptSignInError::Auth(ChatGptAuthError::Io { .. }))
    ));
}

#[tokio::test]
async fn complete_times_out_when_the_user_never_approves() {
    // Arrange
    let server = token_server().await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let pending = start(
        &directory.path().join("chatgpt.json"),
        &server,
        Duration::from_millis(50),
    )
    .await;

    // Act
    let result = pending.complete().await;

    // Assert
    assert!(matches!(result, Err(ChatGptSignInError::TimedOut)));
}

#[tokio::test]
async fn start_rejects_unreadable_sign_in_and_falls_back_from_a_busy_port() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let unreadable_file = directory.path().join("unreadable.json");
    fs::write(&unreadable_file, "not json").expect("unreadable sign-in written");
    let missing_file = directory.path().join("missing.json");
    let busy_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("busy port bound");
    let busy_port = busy_listener.local_addr().expect("busy address").port();
    let sign_in_for = |auth_file: &Path| {
        let auth_file = auth_file.to_string_lossy().into_owned();

        ChatGptSignIn::from_environment(move |name| match name {
            "CHATGPT_AUTH_FILE" => Ok(auth_file.clone()),
            _ => Err(env::VarError::NotPresent),
        })
        .expect("auth file located")
        .with_port(busy_port)
    };
    let unreadable = sign_in_for(&unreadable_file);
    let fallback = sign_in_for(&missing_file);

    // Act
    let unreadable_auth_file = unreadable.auth_file().to_path_buf();
    let unreadable_result = unreadable.start().await;
    let pending = fallback.start().await.expect("sign-in falls back");
    let (address, _) = callback_target(&pending);

    // Assert
    assert_eq!(unreadable_auth_file, unreadable_file);
    assert!(unreadable_result.is_err());
    assert!(address.starts_with("127.0.0.1:"));
    assert_ne!(address, format!("127.0.0.1:{busy_port}"));
}

#[tokio::test]
async fn start_signs_in_with_openai_unless_the_issuer_is_replaced() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth_file = directory
        .path()
        .join("chatgpt.json")
        .to_string_lossy()
        .into_owned();
    let sign_in = ChatGptSignIn::from_environment(move |name| match name {
        "CHATGPT_AUTH_FILE" => Ok(auth_file.clone()),
        "CHATGPT_AUTH_URL" => Ok("  ".to_string()),
        _ => Err(env::VarError::NotPresent),
    })
    .expect("auth file located")
    .with_port(0);

    // Act
    let pending = sign_in.start().await.expect("sign-in starts");

    // Assert
    assert!(
        pending
            .authorize_url()
            .starts_with("https://auth.openai.com/api/accounts/authorize?")
    );
}
