//! Refreshing bearer credential backed by a stored `ChatGPT` sign-in.

use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::credentials::{self, ChatGptCredentials};
use super::error::ChatGptAuthError;
use super::token::{self, DEFAULT_ISSUER, RESOURCE};
use crate::credential::BearerCredential;

/// `OAuth`-style code reported when the stored sign-in was already rejected.
const SIGNED_OUT_CODE: &str = "signed_out";

/// [`BearerCredential`] that reads a `ChatGPT` sign-in from a credential file
/// and refreshes its access token shortly before it expires.
///
/// Refresh tokens rotate, so refreshes are serialized within the process
/// and, through an exclusive lock on a sibling `.lock` file, across
/// processes sharing the file; a process that waited re-reads the file and
/// uses a token another process already refreshed. A refresh runs to
/// completion even when the request awaiting it is cancelled, so a rotated
/// refresh token is always stored. When `OpenAI` rejects the sign-in for
/// good, the file keeps only the app registration, so hosts can offer a new
/// sign-in.
pub struct ChatGptAuth {
    cached: Mutex<Option<ChatGptCredentials>>,
    http: reqwest::Client,
    issuer: String,
    path: PathBuf,
}

impl ChatGptAuth {
    /// Creates a credential backed by the sign-in stored at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            cached: Mutex::new(None),
            http: token::http_client(),
            issuer: DEFAULT_ISSUER.to_string(),
            path: path.into(),
        }
    }

    /// Replaces the authorization server base URL, such as for a local test
    /// server.
    #[must_use]
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = issuer.into();

        self
    }

    /// Returns the credential file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a fresh access token, refreshing and storing it first when it
    /// is near expiry.
    ///
    /// # Errors
    /// Returns an error when the user is not signed in, the sign-in expired,
    /// or the credential file or token endpoint fails.
    pub async fn access_token(&self) -> Result<String, ChatGptAuthError> {
        let mut cached = self.cached.lock().await;
        if let Some(credentials) = cached.as_ref()
            && credentials.is_fresh()
        {
            return Ok(credentials.access_token.clone());
        }
        let refresh = tokio::spawn(Self::fresh_credentials(
            self.http.clone(),
            self.issuer.clone(),
            self.path.clone(),
        ));
        let credentials = refresh.await.map_err(|error| ChatGptAuthError::Io {
            path: self.path.clone(),
            source: io::Error::other(error),
        })??;
        let access_token = credentials.access_token.clone();
        *cached = Some(credentials);

        Ok(access_token)
    }

    /// Loads the stored sign-in under the cross-process lock and refreshes
    /// and stores it when its access token is near expiry.
    async fn fresh_credentials(
        http: reqwest::Client,
        issuer: String,
        path: PathBuf,
    ) -> Result<ChatGptCredentials, ChatGptAuthError> {
        let lock = Self::lock_file(&path).await?;
        let credentials = ChatGptCredentials::load(&path)?;
        if !credentials.is_signed_in() {
            return Err(ChatGptAuthError::SignInRequired {
                code: SIGNED_OUT_CODE.to_string(),
            });
        }
        if credentials.is_fresh() {
            return Ok(credentials);
        }
        let refreshed = match Self::refresh(&http, &issuer, &credentials).await {
            Ok(refreshed) => refreshed,
            Err(error @ ChatGptAuthError::SignInRequired { .. }) => {
                credentials.signed_out().save(&path)?;

                return Err(error);
            }
            Err(error) => return Err(error),
        };
        refreshed.save(&path)?;
        drop(lock);

        Ok(refreshed)
    }

    async fn lock_file(path: &Path) -> Result<File, ChatGptAuthError> {
        let path = path.with_extension("lock");
        let io_error = |source| ChatGptAuthError::Io {
            path: path.clone(),
            source,
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            credentials::create_private_dir(parent).map_err(io_error)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path).map_err(io_error)?;

        tokio::task::spawn_blocking(move || file.lock().map(|()| file))
            .await
            .map_err(|error| io_error(io::Error::other(error)))?
            .map_err(io_error)
    }

    async fn refresh(
        http: &reqwest::Client,
        issuer: &str,
        credentials: &ChatGptCredentials,
    ) -> Result<ChatGptCredentials, ChatGptAuthError> {
        let grant = token::request_tokens(
            http,
            issuer,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &credentials.client_id),
                ("refresh_token", &credentials.refresh_token),
                ("resource", RESOURCE),
            ],
        )
        .await?;

        ChatGptCredentials::from_grant(
            grant,
            credentials.client_id.clone(),
            credentials.ext_agent_host_id.clone(),
            Some(credentials),
        )
    }
}

#[async_trait]
impl BearerCredential for ChatGptAuth {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok(self.access_token().await?)
    }
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod tests;
