//! Version discovery and auto-update helpers.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use semver::Version;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time;
use tracing::{debug, warn};

const AGENTTY_NPM_PACKAGE: &str = "agentty";
const NPM_REGISTRY_LATEST_URL: &str = "https://registry.npmjs.org/agentty/latest";
/// Maximum runtime for each npm/curl version-discovery command.
const VERSION_LOOKUP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum runtime for the npm global-install command.
const VERSION_UPDATE_COMMAND_TIMEOUT: Duration = Duration::from_mins(5);

/// Typed error returned by version infrastructure operations.
///
/// Wraps subprocess and I/O failures so callers can distinguish version
/// command errors without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VersionError {
    /// A version command subprocess failed to launch or produce output.
    #[error("Failed to run `{command}`: {message}")]
    CommandSpawn {
        /// The program that was being launched (e.g. `"npm"`, `"curl"`).
        command: String,
        /// Human-readable detail from the underlying I/O error.
        message: String,
    },

    /// A version command exceeded its configured runtime bound.
    #[error("`{command}` timed out after {timeout:?}")]
    CommandTimedOut {
        /// The program that exceeded its deadline.
        command: String,
        /// Configured command deadline.
        timeout: Duration,
    },

    /// A version command subprocess exited with a non-zero status.
    #[error("`{command}` exited with status {status}")]
    NonZeroExit {
        /// The program that exited unsuccessfully.
        command: String,
        /// Stringified process exit status.
        status: String,
        /// Combined stderr output from the failed process.
        stderr: String,
    },

    /// A successful command returned a response that could not be decoded.
    #[error("Failed to parse `{provider}` version response")]
    ResponseParse {
        /// Command or service whose response was invalid.
        provider: &'static str,
    },
}

/// Minimal command output needed by version-resolution logic.
#[derive(Debug)]
struct VersionCommandOutput {
    status: String,
    stderr: String,
    success: bool,
    stdout: String,
}

impl VersionCommandOutput {
    /// Returns stdout for a successful command or a contextual exit error.
    fn successful_stdout(self, command: &str) -> Result<String, VersionError> {
        if self.success {
            return Ok(self.stdout);
        }

        Err(VersionError::NonZeroExit {
            command: command.to_string(),
            status: self.status,
            stderr: self.stderr,
        })
    }
}

/// External command boundary for npm/curl version discovery commands.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
trait VersionCommandRunner: Send + Sync {
    /// Runs one command and returns normalized output for parsing.
    async fn run_command(
        &self,
        program: &str,
        args: Vec<String>,
        timeout: Duration,
    ) -> Result<VersionCommandOutput, VersionError>;
}

/// Production command runner backed by [`tokio::process::Command`].
struct RealVersionCommandRunner;

#[async_trait]
impl VersionCommandRunner for RealVersionCommandRunner {
    async fn run_command(
        &self,
        program: &str,
        args: Vec<String>,
        timeout: Duration,
    ) -> Result<VersionCommandOutput, VersionError> {
        run_version_command_with_timeout(program, args, timeout).await
    }
}

/// Runs one cancellable version subprocess with an explicit deadline.
async fn run_version_command_with_timeout(
    program: &str,
    args: Vec<String>,
    timeout: Duration,
) -> Result<VersionCommandOutput, VersionError> {
    let mut process = Command::new(program);
    process.args(&args).stdin(Stdio::null()).kill_on_drop(true);

    let output = time::timeout(timeout, process.output())
        .await
        .map_err(|_| VersionError::CommandTimedOut {
            command: program.to_string(),
            timeout,
        })?
        .map_err(|error| VersionError::CommandSpawn {
            command: program.to_string(),
            message: error.to_string(),
        })?;

    Ok(VersionCommandOutput {
        status: output.status.to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// Runs `npm i -g agentty@latest` with a cancellable deadline.
pub(crate) async fn run_npm_update() -> Result<String, VersionError> {
    run_npm_update_with_runner(&RealVersionCommandRunner).await
}

/// Runs the npm update through one injected command boundary.
async fn run_npm_update_with_runner(
    command_runner: &dyn VersionCommandRunner,
) -> Result<String, VersionError> {
    let output = command_runner
        .run_command(
            "npm",
            vec![
                "i".to_string(),
                "-g".to_string(),
                "agentty@latest".to_string(),
            ],
            VERSION_UPDATE_COMMAND_TIMEOUT,
        )
        .await?;

    output.successful_stdout("npm")
}

#[derive(Debug, Deserialize)]
struct NpmRegistryLatestResponse {
    version: String,
}

/// Returns the latest npmjs version tag (`vX.Y.Z`) for `agentty`.
pub async fn latest_npm_version_tag() -> Option<String> {
    latest_npm_version_tag_with_runner(&RealVersionCommandRunner).await
}

/// Runs latest-version discovery through an injected command boundary.
async fn latest_npm_version_tag_with_runner(
    command_runner: &dyn VersionCommandRunner,
) -> Option<String> {
    let result = fetch_latest_npm_version_tag(command_runner).await;

    latest_version_from_result(result)
}

/// Converts one lookup result while retaining diagnostics.
fn latest_version_from_result(result: Result<String, VersionError>) -> Option<String> {
    match result {
        Ok(version_tag) => Some(version_tag),
        Err(error) => {
            warn!(%error, "Failed to discover latest npm version");

            None
        }
    }
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

async fn fetch_latest_npm_version_tag(
    command_runner: &dyn VersionCommandRunner,
) -> Result<String, VersionError> {
    match fetch_latest_version_with_npm_cli(command_runner).await {
        Ok(latest_version) => return Ok(version_tag(&latest_version)),
        Err(error) => {
            debug!(%error, "npm CLI version lookup failed; trying registry fallback");
        }
    }

    let latest_version = fetch_latest_version_with_registry_curl(command_runner).await?;

    Ok(version_tag(&latest_version))
}

async fn fetch_latest_version_with_npm_cli(
    command_runner: &dyn VersionCommandRunner,
) -> Result<Version, VersionError> {
    let output = command_runner
        .run_command(
            "npm",
            vec![
                "view".to_string(),
                AGENTTY_NPM_PACKAGE.to_string(),
                "version".to_string(),
                "--json".to_string(),
            ],
            VERSION_LOOKUP_COMMAND_TIMEOUT,
        )
        .await?;
    let stdout = output.successful_stdout("npm")?;

    parse_npm_cli_version_response(&stdout).ok_or(VersionError::ResponseParse { provider: "npm" })
}

fn parse_npm_cli_version_response(response: &str) -> Option<Version> {
    let version: String = serde_json::from_str(response).ok()?;

    parse_version(&version)
}

async fn fetch_latest_version_with_registry_curl(
    command_runner: &dyn VersionCommandRunner,
) -> Result<Version, VersionError> {
    let output = command_runner
        .run_command(
            "curl",
            vec!["-fsSL".to_string(), NPM_REGISTRY_LATEST_URL.to_string()],
            VERSION_LOOKUP_COMMAND_TIMEOUT,
        )
        .await?;
    let stdout = output.successful_stdout("curl")?;

    parse_registry_latest_response(&stdout).ok_or(VersionError::ResponseParse {
        provider: "npm registry",
    })
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
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    const LATEST_VERSION_CHILD_ENV: &str = "AGENTTY_TEST_LATEST_VERSION_CHILD";

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

    #[tokio::test]
    async fn test_fetch_latest_npm_version_tag_prefers_npm_cli_result() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, args, timeout| {
                assert_eq!(program, "npm");
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
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
                    status: "exit status: 0".to_string(),
                    stderr: String::new(),
                    success: true,
                    stdout: "\"0.2.0\"".to_string(),
                })
            });

        // Act
        let latest_version_tag = fetch_latest_npm_version_tag(&command_runner).await;

        // Assert
        assert_eq!(latest_version_tag.expect("lookup should succeed"), "v0.2.0");
    }

    #[tokio::test]
    async fn test_fetch_latest_npm_version_tag_falls_back_to_registry_curl() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, args, timeout| {
                assert_eq!(program, "npm");
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
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
                    status: "exit status: 1".to_string(),
                    stderr: "npm unavailable".to_string(),
                    success: false,
                    stdout: String::new(),
                })
            });
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, args, timeout| {
                assert_eq!(program, "curl");
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
                assert_eq!(
                    args,
                    vec!["-fsSL".to_string(), NPM_REGISTRY_LATEST_URL.to_string(),]
                );

                Ok(VersionCommandOutput {
                    status: "exit status: 0".to_string(),
                    stderr: String::new(),
                    success: true,
                    stdout: r#"{"name":"agentty","version":"0.3.1"}"#.to_string(),
                })
            });

        // Act
        let latest_version_tag = fetch_latest_npm_version_tag(&command_runner).await;

        // Assert
        assert_eq!(
            latest_version_tag.expect("fallback should succeed"),
            "v0.3.1"
        );
    }

    #[tokio::test]
    async fn test_fetch_latest_npm_version_tag_falls_back_after_npm_timeout() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        let mut sequence = mockall::Sequence::new();
        command_runner
            .expect_run_command()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|program, _, timeout| {
                assert_eq!(program, "npm");
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

                Err(VersionError::CommandTimedOut {
                    command: program.to_string(),
                    timeout,
                })
            });
        command_runner
            .expect_run_command()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|program, _, timeout| {
                assert_eq!(program, "curl");
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

                Ok(VersionCommandOutput {
                    status: "exit status: 0".to_string(),
                    stderr: String::new(),
                    success: true,
                    stdout: r#"{"version":"0.3.2"}"#.to_string(),
                })
            });

        // Act
        let latest_version_tag = fetch_latest_npm_version_tag(&command_runner).await;

        // Assert
        assert_eq!(
            latest_version_tag.expect("fallback should succeed"),
            "v0.3.2"
        );
    }

    #[tokio::test]
    async fn test_fetch_latest_npm_version_tag_preserves_fallback_failure() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(2)
            .returning(|program, _, timeout| {
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

                Ok(VersionCommandOutput {
                    status: "exit status: 7".to_string(),
                    stderr: format!("{program} unavailable"),
                    success: false,
                    stdout: String::new(),
                })
            });

        // Act
        let error = fetch_latest_npm_version_tag(&command_runner)
            .await
            .expect_err("both lookup commands should fail");

        // Assert
        assert!(matches!(
            error,
            VersionError::NonZeroExit {
                command,
                status,
                stderr,
            } if command == "curl"
                && status == "exit status: 7"
                && stderr == "curl unavailable"
        ));
    }

    #[tokio::test]
    async fn test_fetch_latest_npm_version_tag_reports_invalid_fallback_response() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(2)
            .returning(|_, _, timeout| {
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

                Ok(VersionCommandOutput {
                    status: "exit status: 0".to_string(),
                    stderr: String::new(),
                    success: true,
                    stdout: "not-json".to_string(),
                })
            });

        // Act
        let error = fetch_latest_npm_version_tag(&command_runner)
            .await
            .expect_err("invalid fallback response should fail");

        // Assert
        assert!(matches!(
            error,
            VersionError::ResponseParse {
                provider: "npm registry"
            }
        ));
    }

    #[tokio::test]
    async fn test_latest_npm_version_tag_uses_injected_runner() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|program, _, timeout| {
                assert_eq!(program, "npm");
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

                Ok(VersionCommandOutput {
                    status: "exit status: 0".to_string(),
                    stderr: String::new(),
                    success: true,
                    stdout: "\"0.5.0\"".to_string(),
                })
            });

        // Act
        let version_tag = latest_npm_version_tag_with_runner(&command_runner).await;

        // Assert
        assert_eq!(version_tag.as_deref(), Some("v0.5.0"));
    }

    #[tokio::test]
    async fn test_latest_npm_version_tag_uses_isolated_real_runner() {
        if std::env::var_os(LATEST_VERSION_CHILD_ENV).is_some() {
            // Arrange
            let expected_version_tag = "v0.6.0";

            // Act
            let version_tag = latest_npm_version_tag().await;

            // Assert
            assert_eq!(version_tag.as_deref(), Some(expected_version_tag));

            return;
        }

        // Arrange
        let command_dir = tempdir().expect("failed to create fake command directory");
        let npm_path = command_dir.path().join("npm");
        std::fs::write(&npm_path, "#!/bin/sh\nprintf '\"0.6.0\"'\n")
            .expect("failed to write fake npm command");
        let mut permissions = std::fs::metadata(&npm_path)
            .expect("failed to load fake npm metadata")
            .permissions();
        permissions.set_mode(0o750);
        std::fs::set_permissions(&npm_path, permissions)
            .expect("failed to make fake npm executable");
        let current_test_binary =
            std::env::current_exe().expect("failed to resolve current test binary");

        // Act
        let output = tokio::process::Command::new(current_test_binary)
            .arg("--exact")
            .arg("infra::version::tests::test_latest_npm_version_tag_uses_isolated_real_runner")
            .arg("--nocapture")
            .env("PATH", command_dir.path())
            .env(LATEST_VERSION_CHILD_ENV, "1")
            .output()
            .await
            .expect("failed to run isolated latest-version test");

        // Assert
        assert!(
            output.status.success(),
            "isolated latest-version test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn test_fetch_latest_npm_version_tag_preserves_command_errors() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(2)
            .returning(|program, _, timeout| {
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

                Err(VersionError::CommandSpawn {
                    command: program.to_string(),
                    message: "not installed".to_string(),
                })
            });

        // Act
        let error = fetch_latest_npm_version_tag(&command_runner)
            .await
            .expect_err("fallback command error should propagate");

        // Assert
        assert!(matches!(
            error,
            VersionError::CommandSpawn { command, message }
                if command == "curl" && message == "not installed"
        ));
    }

    #[tokio::test]
    async fn test_real_version_command_runner_captures_process_output() {
        // Arrange
        let command_runner = RealVersionCommandRunner;

        // Act
        let output = command_runner
            .run_command(
                "sh",
                vec![
                    "-c".to_string(),
                    "printf '0.4.0'; printf 'notice' >&2".to_string(),
                ],
                Duration::from_secs(1),
            )
            .await
            .expect("command should run");

        // Assert
        assert!(output.success);
        assert_eq!(output.stdout, "0.4.0");
        assert_eq!(output.stderr, "notice");
        assert!(output.status.contains('0'));
    }

    #[tokio::test]
    async fn test_real_version_command_runner_reports_spawn_failure() {
        // Arrange
        let command_runner = RealVersionCommandRunner;

        // Act
        let error = command_runner
            .run_command(
                "agentty-command-that-does-not-exist",
                Vec::new(),
                Duration::from_secs(1),
            )
            .await
            .expect_err("missing command should fail");

        // Assert
        assert!(matches!(
            error,
            VersionError::CommandSpawn { command, message }
                if command == "agentty-command-that-does-not-exist" && !message.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_real_version_command_runner_cancels_timed_out_process() {
        // Arrange
        let command_runner = RealVersionCommandRunner;
        let timeout = Duration::from_millis(25);

        // Act
        let error = command_runner
            .run_command(
                "sh",
                vec!["-c".to_string(), "exec sleep 5".to_string()],
                timeout,
            )
            .await
            .expect_err("long-running command should time out");

        // Assert
        assert!(matches!(
            error,
            VersionError::CommandTimedOut {
                command,
                timeout: actual_timeout,
            } if command == "sh" && actual_timeout == timeout
        ));
    }

    #[tokio::test]
    async fn test_latest_version_result_preserves_optional_public_contract() {
        // Arrange
        let success = Ok("v1.2.3".to_string());
        let lookup_failure = Err(VersionError::ResponseParse { provider: "npm" });

        // Act
        let successful_version = latest_version_from_result(success);
        let missing_version = latest_version_from_result(lookup_failure);

        // Assert
        assert_eq!(successful_version.as_deref(), Some("v1.2.3"));
        assert_eq!(missing_version, None);
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

    #[test]
    fn test_is_newer_than_current_version_rejects_invalid_versions() {
        // Arrange, Act
        let invalid_current = is_newer_than_current_version("current", "v1.0.0");
        let invalid_candidate = is_newer_than_current_version("1.0.0", "candidate");

        // Assert
        assert!(!invalid_current);
        assert!(!invalid_candidate);
    }

    #[tokio::test]
    async fn test_run_npm_update_calls_npm_install_global() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|command, args, timeout| {
                assert_eq!(command, "npm");
                assert_eq!(timeout, VERSION_UPDATE_COMMAND_TIMEOUT);
                assert_eq!(
                    args,
                    vec![
                        "i".to_string(),
                        "-g".to_string(),
                        "agentty@latest".to_string(),
                    ]
                );

                Ok(VersionCommandOutput {
                    status: "exit status: 0".to_string(),
                    stderr: String::new(),
                    success: true,
                    stdout: "added 1 package".to_string(),
                })
            });

        // Act
        let output = run_npm_update_with_runner(&command_runner)
            .await
            .expect("update should succeed");

        // Assert
        assert_eq!(output, "added 1 package");
    }

    #[tokio::test]
    async fn test_run_npm_update_preserves_runner_error_without_displaying_stderr() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|_, _, timeout| {
                assert_eq!(timeout, VERSION_UPDATE_COMMAND_TIMEOUT);

                Ok(VersionCommandOutput {
                    status: "exit status: 1".to_string(),
                    stderr: "permission denied".to_string(),
                    success: false,
                    stdout: String::new(),
                })
            });

        // Act
        let error = run_npm_update_with_runner(&command_runner)
            .await
            .expect_err("should propagate runner error");

        // Assert
        assert_eq!(error.to_string(), "`npm` exited with status exit status: 1");
        assert!(matches!(
            error,
            VersionError::NonZeroExit { stderr, .. } if stderr == "permission denied"
        ));
    }

    #[tokio::test]
    async fn test_run_npm_update_preserves_timeout() {
        // Arrange
        let mut command_runner = MockVersionCommandRunner::new();
        command_runner
            .expect_run_command()
            .times(1)
            .returning(|command, _, timeout| {
                Err(VersionError::CommandTimedOut {
                    command: command.to_string(),
                    timeout,
                })
            });

        // Act
        let error = run_npm_update_with_runner(&command_runner)
            .await
            .expect_err("timed-out update should fail");

        // Assert
        assert!(matches!(
            error,
            VersionError::CommandTimedOut {
                command,
                timeout,
            } if command == "npm" && timeout == VERSION_UPDATE_COMMAND_TIMEOUT
        ));
    }
}
