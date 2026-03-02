//! Version discovery helpers for update notifications.

use std::process::Command;

use semver::Version;
use serde::Deserialize;

const AGENTTY_NPM_PACKAGE: &str = "agentty";
const NPM_REGISTRY_LATEST_URL: &str = "https://registry.npmjs.org/agentty/latest";

/// Minimal command output needed by version-resolution logic.
struct VersionCommandOutput {
    success: bool,
    stdout: String,
}

/// External command boundary for npm/curl version discovery commands.
#[cfg_attr(test, mockall::automock)]
trait VersionCommandRunner: Send + Sync {
    /// Runs one command and returns normalized output for parsing.
    fn run_command(&self, program: &str, args: Vec<String>)
    -> Result<VersionCommandOutput, String>;
}

/// Production command runner backed by [`std::process::Command`].
struct RealVersionCommandRunner;

impl VersionCommandRunner for RealVersionCommandRunner {
    fn run_command(
        &self,
        program: &str,
        args: Vec<String>,
    ) -> Result<VersionCommandOutput, String> {
        let output = Command::new(program)
            .args(&args)
            .output()
            .map_err(|error| format!("Failed to run `{program}`: {error}"))?;

        Ok(VersionCommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct NpmRegistryLatestResponse {
    version: String,
}

/// Returns the latest npmjs version tag (`vX.Y.Z`) for `agentty`.
pub async fn latest_npm_version_tag() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let command_runner = RealVersionCommandRunner;

        fetch_latest_npm_version_tag_sync(&command_runner)
    })
    .await
    .ok()
    .flatten()
}

/// Returns `true` when `candidate_version` is newer than `current_version`.
pub(crate) fn is_newer_than_current_version(
    current_version: &str,
    candidate_version: &str,
) -> bool {
    let Some(current_version) = parse_version(current_version) else {
        return false;
    };

    let Some(candidate_version) = parse_version(candidate_version) else {
        return false;
    };

    candidate_version > current_version
}

fn fetch_latest_npm_version_tag_sync(command_runner: &dyn VersionCommandRunner) -> Option<String> {
    if let Some(latest_version) = fetch_latest_version_with_npm_cli(command_runner) {
        return Some(version_tag(&latest_version));
    }

    let latest_version = fetch_latest_version_with_registry_curl(command_runner)?;

    Some(version_tag(&latest_version))
}

fn fetch_latest_version_with_npm_cli(command_runner: &dyn VersionCommandRunner) -> Option<Version> {
    let output = command_runner
        .run_command(
            "npm",
            vec![
                "view".to_string(),
                AGENTTY_NPM_PACKAGE.to_string(),
                "version".to_string(),
                "--json".to_string(),
            ],
        )
        .ok()?;
    if !output.success {
        return None;
    }

    parse_npm_cli_version_response(&output.stdout)
}

fn parse_npm_cli_version_response(response: &str) -> Option<Version> {
    let version: String = serde_json::from_str(response).ok()?;

    parse_version(&version)
}

fn fetch_latest_version_with_registry_curl(
    command_runner: &dyn VersionCommandRunner,
) -> Option<Version> {
    let output = command_runner
        .run_command(
            "curl",
            vec!["-fsSL".to_string(), NPM_REGISTRY_LATEST_URL.to_string()],
        )
        .ok()?;
    if !output.success {
        return None;
    }

    parse_registry_latest_response(&output.stdout)
}

fn parse_registry_latest_response(response: &str) -> Option<Version> {
    let payload: NpmRegistryLatestResponse = serde_json::from_str(response).ok()?;

    parse_version(&payload.version)
}

fn parse_version(version: &str) -> Option<Version> {
    let normalized_version = version.strip_prefix('v').unwrap_or(version);

    Version::parse(normalized_version).ok()
}

fn version_tag(version: &Version) -> String {
    format!("v{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_accepts_prefixed_version() {
        // Arrange
        let version = "v1.2.3";

        // Act
        let parsed_version = parse_version(version);

        // Assert
        assert_eq!(parsed_version, Some(Version::new(1, 2, 3)));
    }

    #[test]
    fn test_parse_version_rejects_invalid_version() {
        // Arrange
        let version = "vnext";

        // Act
        let parsed_version = parse_version(version);

        // Assert
        assert_eq!(parsed_version, None);
    }

    #[test]
    fn test_parse_npm_cli_version_response_accepts_json_string() {
        // Arrange
        let response = "\"0.1.14\"";

        // Act
        let parsed_version = parse_npm_cli_version_response(response);

        // Assert
        assert_eq!(parsed_version, Some(Version::new(0, 1, 14)));
    }

    #[test]
    fn test_parse_registry_latest_response_extracts_version() {
        // Arrange
        let response = r#"{"name":"agentty","version":"0.1.14"}"#;

        // Act
        let parsed_version = parse_registry_latest_response(response);

        // Assert
        assert_eq!(parsed_version, Some(Version::new(0, 1, 14)));
    }

    #[test]
    fn test_version_tag_prefixes_semver_with_v() {
        // Arrange
        let version = Version::new(0, 1, 14);

        // Act
        let version_tag = version_tag(&version);

        // Assert
        assert_eq!(version_tag, "v0.1.14");
    }

    #[test]
    fn test_fetch_latest_npm_version_tag_sync_prefers_npm_cli_result() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, args| {
                assert_eq!(program, "npm");
                assert_eq!(
                    args,
                    vec![
                        "view".to_string(),
                        AGENTTY_NPM_PACKAGE.to_string(),
                        "version".to_string(),
                        "--json".to_string(),
                    ]
                );

                Ok(VersionCommandOutput {
                    success: true,
                    stdout: "\"0.2.0\"".to_string(),
                })
            });

        // Act
        let latest_version_tag = fetch_latest_npm_version_tag_sync(&command_runner);

        // Assert
        assert_eq!(latest_version_tag, Some("v0.2.0".to_string()));
    }

    #[test]
    fn test_fetch_latest_npm_version_tag_sync_falls_back_to_registry_curl() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, args| {
                assert_eq!(program, "npm");
                assert_eq!(
                    args,
                    vec![
                        "view".to_string(),
                        AGENTTY_NPM_PACKAGE.to_string(),
                        "version".to_string(),
                        "--json".to_string(),
                    ]
                );

                Ok(VersionCommandOutput {
                    success: false,
                    stdout: String::new(),
                })
            });
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, args| {
                assert_eq!(program, "curl");
                assert_eq!(
                    args,
                    vec!["-fsSL".to_string(), NPM_REGISTRY_LATEST_URL.to_string(),]
                );

                Ok(VersionCommandOutput {
                    success: true,
                    stdout: r#"{"name":"agentty","version":"0.3.1"}"#.to_string(),
                })
            });

        // Act
        let latest_version_tag = fetch_latest_npm_version_tag_sync(&command_runner);

        // Assert
        assert_eq!(latest_version_tag, Some("v0.3.1".to_string()));
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
