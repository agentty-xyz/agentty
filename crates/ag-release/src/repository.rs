//! GitHub repository identity parsed from a package's `repository` URL.

use serde::Serialize;

/// GitHub host prefix every supported `repository` URL starts with.
const GITHUB_PREFIX: &str = "https://github.com/";

/// The GitHub repository that hosts a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct GitHubRepository {
    pub(crate) name: String,
    pub(crate) owner: String,
    /// Canonical `https://github.com/<owner>/<name>` URL.
    pub(crate) url: String,
}

impl GitHubRepository {
    /// Parses `https://github.com/<owner>/<name>`, allowing a trailing `/` or
    /// `.git`.
    ///
    /// # Errors
    /// Returns an error when `url` is not a GitHub repository URL.
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
        let invalid =
            || format!("repository `{url}` must be a `https://github.com/<owner>/<name>` URL");
        let path = url.strip_prefix(GITHUB_PREFIX).ok_or_else(invalid)?;
        let path = path.strip_suffix('/').unwrap_or(path);
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/').ok_or_else(invalid)?;
        if !Self::is_segment(owner) || !Self::is_segment(name) {
            return Err(invalid());
        }

        Ok(Self {
            name: name.to_string(),
            owner: owner.to_string(),
            url: format!("{GITHUB_PREFIX}{owner}/{name}"),
        })
    }

    /// Returns whether `segment` is a nonempty GitHub owner or repository
    /// name.
    fn is_segment(segment: &str) -> bool {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    }
}

#[cfg(test)]
#[path = "repository_test.rs"]
mod tests;
