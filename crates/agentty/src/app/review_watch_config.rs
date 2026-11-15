//! Optional project configuration for review-request watching.

use std::path::Path;

use serde::Deserialize;

use crate::infra::fs::{FsClient, FsError};

/// Project-relative path of the review-watch configuration.
const CONFIG_PATH: &str = ".agentty/review-watch.json";
/// Largest accepted configuration file; a list of bot logins stays tiny.
const MAX_CONFIG_BYTES: u64 = 16_384;

/// Review-watch settings checked into or kept in the project folder.
#[derive(Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewWatchConfig {
    /// Review-bot logins whose comments are addressed like a trusted
    /// reviewer's. Their own failed checks are left to those comments.
    #[serde(default)]
    pub(crate) trusted_bots: Vec<String>,
}

impl ReviewWatchConfig {
    /// Loads the configuration from `project_folder` through the injected
    /// boundary. A missing file trusts no bots; an unreadable or invalid
    /// file fails visibly.
    pub(crate) async fn load(fs: &dyn FsClient, project_folder: &Path) -> Result<Self, String> {
        let bytes = match fs
            .read_file_prefix(project_folder.join(CONFIG_PATH), MAX_CONFIG_BYTES + 1)
            .await
        {
            Ok(bytes) => bytes,
            Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("Cannot load `{CONFIG_PATH}`: {error}")),
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(format!("`{CONFIG_PATH}` exceeds {MAX_CONFIG_BYTES} bytes"));
        }
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid `{CONFIG_PATH}`: {error}"))?;
        if config
            .trusted_bots
            .iter()
            .any(|login| login.is_empty() || login.chars().any(char::is_whitespace))
        {
            return Err(format!(
                "`{CONFIG_PATH}` needs nonempty `trusted_bots` logins without whitespace"
            ));
        }

        Ok(config)
    }
}

#[cfg(test)]
#[path = "review_watch_config_test.rs"]
mod tests;
