//! Loopback browser sign-in for the `chatgpt` provider, shared by hosts
//! such as `ag-harness login chatgpt` and Agentty's sign-in popup.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, io};

use ag_router::chatgpt::{ChatGptAuthError, ChatGptCredentials, ChatGptLogin};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::chatgpt::{AuthFileUnavailable, auth_file};

/// Environment variable replacing the `OpenAI` authorization server, such
/// as with a local test server.
pub const CHATGPT_AUTH_URL_ENV: &str = "CHATGPT_AUTH_URL";
/// Loopback port `OpenAI` documents for sign-in callbacks.
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
/// Time one loopback connection gets to send its request line.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest request line read from one loopback connection.
const MAX_REQUEST_LINE_BYTES: u64 = 8 * 1024;
/// Time the user gets to approve the sign-in in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_mins(10);
const NOT_FOUND_RESPONSE: &[u8] =
    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
const SUCCESS_PAGE: &str = "<!doctype html><title>Agentty</title><p>Signed in to Agentty with \
                            ChatGPT. You can close this tab.</p>";
const FAILURE_PAGE: &str = "<!doctype html><title>Agentty</title><p>Agentty sign-in failed. \
                            Return to Agentty for details.</p>";
const STALE_PAGE: &str = "<!doctype html><title>Agentty</title><p>This sign-in link is no longer \
                          active. Use the newest link Agentty showed.</p>";

/// Failure to run the browser sign-in.
#[derive(Debug, Error)]
pub enum ChatGptSignInError {
    /// The credential file location is unknown.
    #[error(transparent)]
    AuthFile(#[from] AuthFileUnavailable),
    /// The sign-in was denied, did not match, or could not be stored.
    #[error(transparent)]
    Auth(#[from] ChatGptAuthError),
    /// The loopback callback listener failed.
    #[error("cannot receive the sign-in callback on 127.0.0.1: {0}")]
    Callback(#[source] io::Error),
    /// The user did not finish the browser sign-in in time.
    #[error("timed out waiting for the ChatGPT sign-in in the browser")]
    TimedOut,
}

/// Configuration of a browser sign-in that stores a `ChatGPT` plan
/// credential.
pub struct ChatGptSignIn {
    auth_file: PathBuf,
    issuer: Option<String>,
    port: u16,
    timeout: Duration,
}

impl ChatGptSignIn {
    /// Signs in to the credential file the `chatgpt` provider reads, with
    /// the authorization server optionally replaced by `CHATGPT_AUTH_URL`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthFileUnavailable`] when the credential file cannot be
    /// located.
    pub fn from_environment(
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Self, AuthFileUnavailable> {
        let issuer = environment(CHATGPT_AUTH_URL_ENV)
            .ok()
            .filter(|issuer| !issuer.trim().is_empty());

        Ok(Self {
            auth_file: auth_file(environment)?,
            issuer,
            port: CALLBACK_PORT,
            timeout: SIGN_IN_TIMEOUT,
        })
    }

    /// Replaces the preferred callback port; a busy port falls back to any
    /// free one.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;

        self
    }

    /// Replaces how long [`PendingChatGptSignIn::complete`] waits for the
    /// user to approve the sign-in; the default is ten minutes.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;

        self
    }

    /// Returns the credential file the sign-in writes.
    pub fn auth_file(&self) -> &Path {
        &self.auth_file
    }

    /// Starts listening for the browser callback and returns the pending
    /// sign-in whose URL the host opens for the user. A previous sign-in
    /// is reused so `OpenAI` keeps the registered app.
    ///
    /// # Errors
    ///
    /// Returns an error when the stored sign-in is unreadable or no
    /// loopback port can be bound.
    pub async fn start(self) -> Result<PendingChatGptSignIn, ChatGptSignInError> {
        let previous = match ChatGptCredentials::load(&self.auth_file) {
            Ok(previous) => Some(previous),
            Err(ChatGptAuthError::NotSignedIn { .. }) => None,
            Err(error) => return Err(error.into()),
        };
        let listener = match TcpListener::bind(("127.0.0.1", self.port)).await {
            Ok(listener) => listener,
            Err(_) => TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(ChatGptSignInError::Callback)?,
        };
        let port = listener
            .local_addr()
            .map_err(ChatGptSignInError::Callback)?
            .port();
        let mut login =
            ChatGptLogin::new(format!("http://127.0.0.1:{port}{CALLBACK_PATH}"), previous);
        if let Some(issuer) = self.issuer {
            login = login.with_issuer(issuer);
        }

        Ok(PendingChatGptSignIn {
            auth_file: self.auth_file,
            listener,
            login,
            timeout: self.timeout,
        })
    }
}

/// Browser sign-in waiting for the user to approve it.
pub struct PendingChatGptSignIn {
    auth_file: PathBuf,
    listener: TcpListener,
    login: ChatGptLogin,
    timeout: Duration,
}

impl PendingChatGptSignIn {
    /// Returns the URL the user opens to approve the sign-in.
    pub fn authorize_url(&self) -> String {
        self.login.authorize_url()
    }

    /// Waits for this sign-in's browser callback, stores the credential, and
    /// answers the browser with a page telling the user whether it worked.
    ///
    /// Other requests, including callbacks of earlier sign-ins, are answered
    /// and ignored.
    ///
    /// # Errors
    ///
    /// Returns an error when the user denied access, the token exchange
    /// fails, the credential cannot be stored, the listener fails, or the
    /// user does not approve the sign-in in time.
    pub async fn complete(self) -> Result<ChatGptCredentials, ChatGptSignInError> {
        let timeout = self.timeout;

        tokio::time::timeout(timeout, self.serve_callbacks())
            .await
            .map_err(|_| ChatGptSignInError::TimedOut)?
    }

    /// Accepts loopback connections, reading each one's request line
    /// concurrently so a silent connection cannot hold up the callback.
    async fn serve_callbacks(self) -> Result<ChatGptCredentials, ChatGptSignInError> {
        let (sender, mut requests) = mpsc::unbounded_channel();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(ChatGptSignInError::Callback)?;
                    tokio::spawn(Self::forward_request(stream, sender.clone()));
                }
                Some((mut stream, target)) = requests.recv() => {
                    let Some(query) = target
                        .strip_prefix(CALLBACK_PATH)
                        .and_then(|rest| rest.strip_prefix('?'))
                    else {
                        let _ = stream.write_all(NOT_FOUND_RESPONSE).await;
                        continue;
                    };
                    if !self.login.is_own_callback(query) {
                        Self::respond(&mut stream, "400 Bad Request", STALE_PAGE).await;
                        continue;
                    }
                    let result = self.finish(query).await;
                    let page = if result.is_ok() {
                        SUCCESS_PAGE
                    } else {
                        FAILURE_PAGE
                    };
                    Self::respond(&mut stream, "200 OK", page).await;

                    return result;
                }
            }
        }
    }

    /// Reads one bounded request line and forwards the connection with its
    /// request target; a client that stays silent or sends garbage is
    /// dropped.
    async fn forward_request(
        stream: TcpStream,
        sender: mpsc::UnboundedSender<(TcpStream, String)>,
    ) {
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        let mut bounded = (&mut reader).take(MAX_REQUEST_LINE_BYTES);
        let read = tokio::time::timeout(
            CONNECTION_READ_TIMEOUT,
            bounded.read_line(&mut request_line),
        )
        .await;
        let Ok(Ok(_)) = read else {
            return;
        };
        let target = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let _ = sender.send((reader.into_inner(), target));
    }

    /// Exchanges the callback's code and stores the credential.
    async fn finish(&self, query: &str) -> Result<ChatGptCredentials, ChatGptSignInError> {
        let credentials = self.login.finish(query).await?;
        credentials.save(&self.auth_file)?;

        Ok(credentials)
    }

    async fn respond(stream: &mut TcpStream, status: &str, page: &str) {
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: \
             {}\r\nconnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }
}

#[cfg(test)]
#[path = "chatgpt_sign_in_test.rs"]
mod tests;
