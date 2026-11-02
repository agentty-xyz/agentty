//! Stored `ChatGPT` sign-in: tokens, client registration, and the credential
//! file format.

use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fmt, fs};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use super::error::ChatGptAuthError;
use super::token::{PLAN_SCOPE, TokenResponse};

/// Access tokens closer than this to expiry are refreshed before use.
const REFRESH_MARGIN: Duration = Duration::from_mins(5);

/// Tokens and app registration from one `ChatGPT` sign-in.
///
/// `Debug` redacts every token.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct ChatGptCredentials {
    /// Bearer token for `OpenAI` Responses requests, valid for one hour.
    pub access_token: String,
    /// Client identifier `OpenAI` issued to this app at first sign-in.
    pub client_id: String,
    /// Signed-in account email, when the ID token carries one.
    pub email: Option<String>,
    /// Unix time, in seconds, when `access_token` expires.
    pub expires_at: u64,
    /// Stable identifier of this host, reused for later sign-ins.
    pub ext_agent_host_id: String,
    /// ID token from the latest grant.
    pub id_token: String,
    /// Rotating refresh token; each refresh replaces it. Empty once `OpenAI`
    /// rejected the sign-in and the user must sign in again.
    pub refresh_token: String,
    /// Scopes the user granted.
    pub scopes: Vec<String>,
}

impl ChatGptCredentials {
    /// Reads credentials from `path`.
    ///
    /// # Errors
    /// Returns [`ChatGptAuthError::NotSignedIn`] when the file does not exist,
    /// or an I/O or decoding failure.
    pub fn load(path: &Path) -> Result<Self, ChatGptAuthError> {
        let text = fs::read_to_string(path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                ChatGptAuthError::NotSignedIn {
                    path: path.to_path_buf(),
                }
            } else {
                ChatGptAuthError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;

        serde_json::from_str(&text).map_err(|source| ChatGptAuthError::InvalidCredentials {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Atomically writes credentials to `path`, readable only by the owner,
    /// creating missing parent directories.
    ///
    /// # Errors
    /// Returns an I/O failure.
    pub fn save(&self, path: &Path) -> Result<(), ChatGptAuthError> {
        let io_error = |source| ChatGptAuthError::Io {
            path: path.to_path_buf(),
            source,
        };
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        create_private_dir(parent).map_err(io_error)?;
        // `tempfile` creates the file readable only by its owner.
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        let json = serde_json::to_vec_pretty(self)
            .map_err(io::Error::other)
            .map_err(io_error)?;
        file.write_all(&json).map_err(io_error)?;
        file.as_file().sync_all().map_err(io_error)?;
        file.persist(path).map_err(|error| io_error(error.error))?;

        Ok(())
    }

    /// Returns whether the sign-in can still serve requests. A sign-in that
    /// `OpenAI` rejected is kept only for its app registration and account.
    pub fn is_signed_in(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// Returns these credentials without tokens, keeping the app
    /// registration and account the next sign-in reuses.
    pub(super) fn signed_out(&self) -> Self {
        Self {
            access_token: String::new(),
            expires_at: 0,
            refresh_token: String::new(),
            ..self.clone()
        }
    }

    /// Returns whether the access token stays valid past the refresh margin.
    pub(super) fn is_fresh(&self) -> bool {
        self.expires_at > unix_now().saturating_add(REFRESH_MARGIN.as_secs())
    }

    /// Builds credentials from a token grant, keeping `previous` values the
    /// grant omits.
    pub(super) fn from_grant(
        grant: TokenResponse,
        client_id: String,
        ext_agent_host_id: String,
        previous: Option<&Self>,
    ) -> Result<Self, ChatGptAuthError> {
        let scopes: Vec<String> = match grant.scope {
            Some(scope) => scope.split_whitespace().map(str::to_string).collect(),
            None => previous.map_or_else(Vec::new, |credentials| credentials.scopes.clone()),
        };
        if !scopes.iter().any(|scope| scope == PLAN_SCOPE) {
            return Err(ChatGptAuthError::MissingPlanScope);
        }
        let id_token = grant
            .id_token
            .or_else(|| previous.map(|credentials| credentials.id_token.clone()))
            .unwrap_or_default();
        let refresh_token = grant
            .refresh_token
            .or_else(|| previous.map(|credentials| credentials.refresh_token.clone()))
            .unwrap_or_default();

        Ok(Self {
            access_token: grant.access_token,
            client_id,
            email: id_token_email(&id_token)
                .or_else(|| previous.and_then(|credentials| credentials.email.clone())),
            expires_at: unix_now().saturating_add(grant.expires_in),
            ext_agent_host_id,
            id_token,
            refresh_token,
            scopes,
        })
    }
}

impl fmt::Debug for ChatGptCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptCredentials")
            .field("client_id", &self.client_id)
            .field("email", &self.email)
            .field("expires_at", &self.expires_at)
            .field("ext_agent_host_id", &self.ext_agent_host_id)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// Creates `directory` and its missing parents, accessible only by the owner
/// where the platform supports it.
pub(super) fn create_private_dir(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);

    builder.create(directory)
}

/// Reads the `email` claim of an ID token received directly from the token
/// endpoint over TLS. The claim is display-only, so the signature is not
/// checked.
fn id_token_email(id_token: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Claims {
        email: Option<String>,
    }

    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;

    serde_json::from_slice::<Claims>(&bytes).ok()?.email
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
#[path = "credentials_test.rs"]
mod tests;
