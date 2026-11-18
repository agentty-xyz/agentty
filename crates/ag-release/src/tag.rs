//! Release tag parsing.

use semver::Version;

/// A release tag, `v<semver>` or a bare `<semver>`, and the version it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseTag {
    pub(crate) name: String,
    pub(crate) version: Version,
}

impl ReleaseTag {
    /// Parses `tag`, keeping its exact spelling as the tag name.
    ///
    /// # Errors
    /// Returns an error when `tag` is neither `v<semver>` nor `<semver>`.
    pub(crate) fn parse(tag: &str) -> Result<Self, String> {
        let version_text = tag.strip_prefix('v').unwrap_or(tag);
        let version = Version::parse(version_text).map_err(|error| {
            format!("release tag `{tag}` must be `v<semver>` or `<semver>`: {error}")
        })?;

        Ok(Self {
            name: tag.to_string(),
            version,
        })
    }
}

#[cfg(test)]
#[path = "tag_test.rs"]
mod tests;
