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

    // Assert
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
