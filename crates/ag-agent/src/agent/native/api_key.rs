//! Harness provider API keys stored in Agentty's `auth/api_keys.json`.
//!
//! The file is the only source of the keys harness models use. A key found
//! in the provider's environment variable, such as `KIMI_API_KEY`, is
//! imported into the file once, when the file holds no key for that
//! provider; afterwards the variable is ignored.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use ag_harness::provider::ModelProvider;
use serde::{Deserialize, Serialize};

use super::config::agentty_home;

/// API key file under Agentty's data directory.
const API_KEY_FILE: &str = "auth/api_keys.json";
/// Format version written to and accepted from the API key file.
const FILE_VERSION: u32 = 1;
/// Failure when no variable locates Agentty's data directory.
const UNLOCATED_API_KEY_FILE: &str =
    "Set AGENTTY_ROOT or HOME to locate Agentty's API key file, then retry.";

/// Returns `environment` with each provider API key variable holding the key
/// stored for that provider, or removed when none is stored.
///
/// Nonblank keys the variables hold for providers without a stored key are
/// imported into the file first. A file that cannot be read leaves every key
/// unset rather than falling back to the variables.
pub(crate) async fn with_stored_api_keys(
    mut environment: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let exported = key_providers()
        .filter_map(|provider| {
            let value = environment.remove(provider.api_key_environment())?;
            let value = value.trim();

            (!value.is_empty()).then(|| (provider, value.to_string()))
        })
        .collect::<Vec<_>>();
    let Some(path) = api_key_file(&environment) else {
        return environment;
    };
    let mut stored = match StoredApiKeys::load(&path).await {
        Ok(stored) => stored,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring unreadable API key file");

            return environment;
        }
    };
    let mut imported = false;
    for (provider, api_key) in exported {
        if stored.api_key(provider).is_none() {
            stored.set_api_key(provider, api_key);
            imported = true;
        }
    }
    if imported && let Err(error) = stored.save(&path).await {
        tracing::warn!(path = %path.display(), %error, "failed to import API keys");
    }
    for provider in key_providers() {
        if let Some(api_key) = stored.api_key(provider) {
            environment.insert(
                provider.api_key_environment().to_string(),
                api_key.to_string(),
            );
        }
    }

    environment
}

/// Stores `api_key` for the provider whose id is `provider`, such as `kimi`,
/// in the API key file `environment` locates, replacing a stored key.
///
/// # Errors
/// Returns a user-facing message when the provider takes no API key, the
/// key is blank, the file cannot be located or read, or the key cannot be
/// written.
pub(crate) async fn save_api_key(
    environment: &BTreeMap<String, String>,
    provider: &str,
    api_key: &str,
) -> Result<(), String> {
    let provider = key_providers()
        .find(|candidate| candidate.as_str() == provider)
        .ok_or_else(|| format!("The `{provider}` provider takes no API key."))?;
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("Paste a nonblank API key.".to_string());
    }
    let path = api_key_file(environment).ok_or(UNLOCATED_API_KEY_FILE)?;
    let mut stored = StoredApiKeys::load(&path).await.map_err(|error| {
        format!(
            "Cannot read `{}`: {error}. Fix or delete it, then retry.",
            path.display()
        )
    })?;
    stored.set_api_key(provider, api_key.to_string());

    stored
        .save(&path)
        .await
        .map_err(|error| format!("Cannot save `{}`: {error}.", path.display()))
}

/// Returns the providers that authenticate with an API key.
fn key_providers() -> impl Iterator<Item = ModelProvider> {
    ModelProvider::all()
        .iter()
        .copied()
        .filter(|provider| !provider.requires_sign_in())
}

/// Returns the API key file under the data directory `environment`
/// locates, or `None` when it locates none.
fn api_key_file(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    agentty_home(environment).map(|home| home.join(API_KEY_FILE))
}

/// Contents of the API key file.
#[derive(Deserialize, Serialize)]
struct StoredApiKeys {
    /// Stored keys by provider id, such as `kimi`.
    #[serde(default)]
    providers: BTreeMap<String, StoredApiKey>,
    /// Format version, [`FILE_VERSION`].
    version: u32,
}

impl Default for StoredApiKeys {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            version: FILE_VERSION,
        }
    }
}

impl StoredApiKeys {
    /// Reads the file at `path`, or empty contents when it does not exist.
    /// A file other users can access is read with a warning.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read, is not valid JSON of
    /// this shape, or has another format version.
    async fn load(path: &Path) -> io::Result<Self> {
        let contents = match tokio::fs::read(path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let stored: Self = serde_json::from_slice(&contents)?;
        if stored.version != FILE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported format version {}", stored.version),
            ));
        }
        let mode = tokio::fs::metadata(path).await?.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(path = %path.display(), "API key file is accessible to other users");
        }

        Ok(stored)
    }

    /// Returns the nonblank key stored for `provider`.
    fn api_key(&self, provider: ModelProvider) -> Option<&str> {
        self.providers
            .get(provider.as_str())
            .map(|stored| stored.api_key.trim())
            .filter(|api_key| !api_key.is_empty())
    }

    /// Stores `api_key` for `provider`, replacing a stored key.
    fn set_api_key(&mut self, provider: ModelProvider, api_key: String) {
        self.providers
            .insert(provider.as_str().to_string(), StoredApiKey { api_key });
    }

    /// Replaces the file at `path` atomically with these contents, readable
    /// only by the owner, creating its directory private to the owner.
    ///
    /// # Errors
    /// Returns an error when the directory or file cannot be written.
    async fn save(&self, path: &Path) -> io::Result<()> {
        let contents = serde_json::to_vec_pretty(self)?;
        let path = path.to_path_buf();

        tokio::task::spawn_blocking(move || Self::write_private(&contents, &path))
            .await
            .map_err(io::Error::other)
            .flatten()
    }

    /// Writes `contents` to an owner-only temporary file beside `path`, then
    /// moves it over `path`; the temporary file is removed on failure.
    fn write_private(contents: &[u8], path: &Path) -> io::Result<()> {
        // `path` always names a file inside the data directory.
        let directory = path.parent().unwrap_or(Path::new("."));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        // `tempfile` creates the file readable only by its owner.
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(contents)?;
        file.as_file().sync_all()?;
        file.persist(path)?;

        Ok(())
    }
}

/// One provider's stored key.
#[derive(Deserialize, Serialize)]
struct StoredApiKey {
    /// Key sent to the provider.
    api_key: String,
}

#[cfg(test)]
#[path = "api_key_test.rs"]
mod tests;
