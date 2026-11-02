//! Loopback browser sign-in for the `chatgpt` provider, shared by hosts
//! such as `ag-harness login chatgpt` and Agentty's sign-in popup.

use std::path::{Path, PathBuf};
use std::{env, io};

use ag_router::chatgpt::{ChatGptAuthError, ChatGptCredentials, ChatGptLogin};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener;

use super::chatgpt::{AuthFileUnavailable, auth_file};

/// Environment variable replacing the `OpenAI` authorization server, such
/// as with a local test server.
pub const CHATGPT_AUTH_URL_ENV: &str = "CHATGPT_AUTH_URL";
/// Loopback port `OpenAI` documents for sign-in callbacks.
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const SUCCESS_PAGE: &str = "<!doctype html><title>Agentty</title><p>Signed in to Agentty with \
                            ChatGPT. You can close this tab.</p>";
const FAILURE_PAGE: &str = "<!doctype html><title>Agentty</title><p>Agentty sign-in failed. \
                            Return to Agentty for details.</p>";

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
}

/// Configuration of a browser sign-in that stores a `ChatGPT` plan
/// credential.
pub struct ChatGptSignIn {
    auth_file: PathBuf,
    issuer: Option<String>,
    port: u16,
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
        })
    }

    /// Replaces the preferred callback port; a busy port falls back to any
    /// free one.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;

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
        })
    }
}

/// Browser sign-in waiting for the user to approve it.
pub struct PendingChatGptSignIn {
    auth_file: PathBuf,
    listener: TcpListener,
    login: ChatGptLogin,
}

impl PendingChatGptSignIn {
    /// Returns the URL the user opens to approve the sign-in.
    pub fn authorize_url(&self) -> String {
        self.login.authorize_url()
    }

    /// Waits for the browser callback, answers it with a page telling the
    /// user whether it worked, and stores the credential.
    ///
    /// # Errors
    ///
    /// Returns an error when the user denied access, the callback does not
    /// belong to this sign-in, the token exchange fails, or the credential
    /// cannot be stored.
    pub async fn complete(self) -> Result<ChatGptCredentials, ChatGptSignInError> {
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(ChatGptSignInError::Callback)?;
            let mut stream = BufReader::new(stream);
            let mut request_line = String::new();
            stream
                .read_line(&mut request_line)
                .await
                .map_err(ChatGptSignInError::Callback)?;
            let target = request_line.split_whitespace().nth(1).unwrap_or_default();
            let Some(query) = target
                .strip_prefix(CALLBACK_PATH)
                .and_then(|rest| rest.strip_prefix('?'))
            else {
                let _ = stream
                    .get_mut()
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            };
            let result = self.login.finish(query).await;
            let page = if result.is_ok() {
                SUCCESS_PAGE
            } else {
                FAILURE_PAGE
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: \
                 {}\r\nconnection: close\r\n\r\n{page}",
                page.len()
            );
            let _ = stream.get_mut().write_all(response.as_bytes()).await;
            let credentials = result?;
            credentials.save(&self.auth_file)?;

            return Ok(credentials);
        }
    }
}
