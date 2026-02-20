//! Version discovery helpers for update notifications.

use std::process::Command;

const AGENTTY_GITHUB_REPO: &str = "https://github.com/opencloudtool/agentty";
const TAG_REF_PREFIX: &str = "refs/tags/";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StableVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl StableVersion {
    fn to_tag(self) -> String {
        format!("v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Returns the latest stable `vX.Y.Z` tag from the official GitHub repository.
pub async fn latest_stable_version_tag() -> Option<String> {
    tokio::task::spawn_blocking(fetch_latest_stable_version_tag_sync)
        .await
        .ok()
        .flatten()
}

/// Returns `true` when `candidate_version` is a newer stable version.
pub(crate) fn is_newer_than_current_version(
    current_version: &str,
    candidate_version: &str,
) -> bool {
    let Some(current_version) = parse_stable_version(current_version) else {
        return false;
    };
    let Some(candidate_version) = parse_stable_version(candidate_version) else {
        return false;
    };

    candidate_version > current_version
}

fn fetch_latest_stable_version_tag_sync() -> Option<String> {
    let output = Command::new("git")
        .args(["ls-remote", "--tags", "--refs", AGENTTY_GITHUB_REPO, "v*"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let refs_output = String::from_utf8_lossy(&output.stdout);

    latest_stable_version_tag_from_ls_remote_output(refs_output.as_ref())
}

fn latest_stable_version_tag_from_ls_remote_output(refs_output: &str) -> Option<String> {
    let mut latest_version = None;
    for ref_line in refs_output.lines() {
        let Some(tag_name) = parse_tag_name_from_ref_line(ref_line) else {
            continue;
        };
        let Some(parsed_version) = parse_stable_version(tag_name) else {
            continue;
        };
        if latest_version.is_none_or(|current_version| parsed_version > current_version) {
            latest_version = Some(parsed_version);
        }
    }

    latest_version.map(StableVersion::to_tag)
}

fn parse_tag_name_from_ref_line(ref_line: &str) -> Option<&str> {
    let (_, reference_name) = ref_line.split_once('\t')?;

    reference_name.strip_prefix(TAG_REF_PREFIX)
}

fn parse_stable_version(version: &str) -> Option<StableVersion> {
    let normalized_version = version.strip_prefix('v').unwrap_or(version);
    if normalized_version.contains('-') || normalized_version.contains('+') {
        return None;
    }

    let mut version_parts = normalized_version.split('.');
    let major = parse_version_part(version_parts.next()?)?;
    let minor = parse_version_part(version_parts.next()?)?;
    let patch = parse_version_part(version_parts.next()?)?;
    if version_parts.next().is_some() {
        return None;
    }

    Some(StableVersion {
        major,
        minor,
        patch,
    })
}

fn parse_version_part(version_part: &str) -> Option<u64> {
    if version_part.is_empty() {
        return None;
    }

    version_part.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stable_version_accepts_prefixed_version() {
        // Arrange
        let version = "v1.2.3";

        // Act
        let parsed_version = parse_stable_version(version);

        // Assert
        assert_eq!(
            parsed_version,
            Some(StableVersion {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
    }

    #[test]
    fn test_parse_stable_version_rejects_prerelease_version() {
        // Arrange
        let version = "v1.2.3-beta.1";

        // Act
        let parsed_version = parse_stable_version(version);

        // Assert
        assert_eq!(parsed_version, None);
    }

    #[test]
    fn test_latest_stable_version_tag_from_ls_remote_output_returns_latest_stable() {
        // Arrange
        let refs_output = concat!(
            "abcdef0\trefs/tags/v0.1.8\n",
            "abcdef1\trefs/tags/v0.1.12\n",
            "abcdef2\trefs/tags/v0.1.9\n",
        );

        // Act
        let latest_version_tag = latest_stable_version_tag_from_ls_remote_output(refs_output);

        // Assert
        assert_eq!(latest_version_tag.as_deref(), Some("v0.1.12"));
    }

    #[test]
    fn test_latest_stable_version_tag_from_ls_remote_output_ignores_prerelease_and_invalid_tags() {
        // Arrange
        let refs_output = concat!(
            "abcdef0\trefs/tags/v0.1.11\n",
            "abcdef1\trefs/tags/v0.1.12-beta.1\n",
            "abcdef2\trefs/tags/vnext\n",
            "abcdef3\trefs/tags/notes\n",
        );

        // Act
        let latest_version_tag = latest_stable_version_tag_from_ls_remote_output(refs_output);

        // Assert
        assert_eq!(latest_version_tag.as_deref(), Some("v0.1.11"));
    }

    #[test]
    fn test_latest_stable_version_tag_from_ls_remote_output_returns_none_for_empty_input() {
        // Arrange
        let refs_output = "";

        // Act
        let latest_version_tag = latest_stable_version_tag_from_ls_remote_output(refs_output);

        // Assert
        assert_eq!(latest_version_tag, None);
    }

    #[test]
    fn test_is_newer_than_current_version_returns_true_when_candidate_is_newer() {
        // Arrange
        let current_version = "0.1.11";
        let candidate_version = "v0.1.12";

        // Act
        let is_newer = is_newer_than_current_version(current_version, candidate_version);

        // Assert
        assert!(is_newer);
    }

    #[test]
    fn test_is_newer_than_current_version_returns_false_when_candidate_is_not_newer() {
        // Arrange
        let current_version = "0.1.12";
        let candidate_version = "v0.1.11";

        // Act
        let is_newer = is_newer_than_current_version(current_version, candidate_version);

        // Assert
        assert!(!is_newer);
    }
}
