//! Refreshing bearer credential backed by a stored `ChatGPT` sign-in.

use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::credentials::ChatGptCredentials;
use super::error::ChatGptAuthError;
use super::token::{self, DEFAULT_ISSUER, RESOURCE};
use crate::credential::BearerCredential;

/// [`BearerCredential`] that reads a `ChatGPT` sign-in from a credential file
/// and refreshes its access token shortly before it expires.
///
/// Refresh tokens rotate, so refreshes are serialized within the process
/// and, through an exclusive lock on a sibling `.lock` file, across
/// processes sharing the file; a process that waited re-reads the file and
/// uses a token another process already refreshed.
pub struct ChatGptAuth {
    cached: Mutex<Option<ChatGptCredentials>>,
    http: reqwest::Client,
    issuer: String,
    path: PathBuf,
}

impl ChatGptAuth {
    /// Creates a credential backed by the sign-in stored at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            cached: Mutex::new(None),
            http: reqwest::Client::new(),
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
        let lock = self.lock_file().await?;
        let mut credentials = ChatGptCredentials::load(&self.path)?;
        if !credentials.is_fresh() {
            credentials = self.refresh(&credentials).await?;
            credentials.save(&self.path)?;
        }
        drop(lock);
        let access_token = credentials.access_token.clone();
        *cached = Some(credentials);

        Ok(access_token)
    }

    async fn lock_file(&self) -> Result<File, ChatGptAuthError> {
        let path = self.path.with_extension("lock");
        let io_error = |source| ChatGptAuthError::Io {
            path: path.clone(),
            source,
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_error)?;

        tokio::task::spawn_blocking(move || file.lock().map(|()| file))
            .await
            .map_err(|error| io_error(std::io::Error::other(error)))?
            .map_err(io_error)
    }

    async fn refresh(
        &self,
        credentials: &ChatGptCredentials,
    ) -> Result<ChatGptCredentials, ChatGptAuthError> {
        let grant = token::request_tokens(
            &self.http,
            &self.issuer,
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
