//! Version discovery and auto-update helpers.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::time;
use tracing::{debug, warn};

const AGENTTY_NPM_PACKAGE: &str = "agentty";
const NPM_REGISTRY_LATEST_URL: &str = "https://registry.npmjs.org/agentty/latest";
/// Maximum runtime for each npm/curl version-discovery command.
const VERSION_LOOKUP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum runtime for a package-manager global-install command.
const VERSION_UPDATE_COMMAND_TIMEOUT: Duration = Duration::from_mins(5);

/// Typed error returned by version infrastructure operations.
///
/// Wraps subprocess and I/O failures so callers can distinguish version
/// command errors without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VersionError {
    /// The running binary cannot be tied to exactly one global installer.
    #[error(
        "Update manually with your original installer: npm i -g agentty@latest, bun add -g \
         agentty@latest, pnpm add -g agentty@latest, or rerun the Cargo/shell installation command"
    )]
    UnknownInstallation,

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
    stdout: String,
    success: bool,
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
    /// Resolves installation links without relying on path naming conventions.
    fn canonicalize_path(&self, path: &Path) -> Option<PathBuf>;

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
    fn canonicalize_path(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }

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

/// Runs the original global package manager, or requires a manual update.
pub(crate) async fn run_update() -> Result<String, VersionError> {
    let executable = std::env::current_exe().ok();

    run_update_with_runner(executable.as_deref(), &RealVersionCommandRunner).await
}

/// Supported global package managers. Local and transient installs are manual.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageManager {
    Npm,
    Bun,
    Pnpm,
}

impl PackageManager {
    fn program(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Bun => "bun",
            Self::Pnpm => "pnpm",
        }
    }

    fn discovery_args(self) -> Vec<String> {
        let args = match self {
            Self::Npm => vec!["root", "--global"],
            Self::Pnpm => vec!["list", "--global", "--depth", "0", "--json"],
            Self::Bun => vec!["pm", "bin", "--global"],
        };

        args.into_iter().map(str::to_string).collect()
    }

    fn update_args(self) -> Vec<String> {
        let operation = match self {
            Self::Npm => "i",
            Self::Bun | Self::Pnpm => "add",
        };

        [operation, "-g", "agentty@latest"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}

/// Matches the current binary to configured global locations before mutation.
async fn run_update_with_runner(
    executable: Option<&Path>,
    command_runner: &dyn VersionCommandRunner,
) -> Result<String, VersionError> {
    let executable = executable
        .and_then(|path| command_runner.canonicalize_path(path))
        .ok_or(VersionError::UnknownInstallation)?;
    let mut matches = Vec::new();
    for manager in [
        PackageManager::Npm,
        PackageManager::Bun,
        PackageManager::Pnpm,
    ] {
        let Ok(output) = command_runner
            .run_command(
                manager.program(),
                manager.discovery_args(),
                VERSION_LOOKUP_COMMAND_TIMEOUT,
            )
            .await
        else {
            continue;
        };
        let Ok(directory) = output.successful_stdout(manager.program()) else {
            continue;
        };
        let candidates = match manager {
            PackageManager::Pnpm => {
                let Ok(packages) = serde_json::from_str::<Vec<Value>>(&directory) else {
                    continue;
                };

                packages
                    .iter()
                    .filter_map(|package| {
                        package["dependencies"][AGENTTY_NPM_PACKAGE]["path"].as_str()
                    })
                    .map(Path::new)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join("node_modules/.bin_real/agentty"))
                    .collect::<Vec<_>>()
            }
            PackageManager::Npm | PackageManager::Bun => {
                let directory = Path::new(directory.trim());
                if !directory.is_absolute() {
                    continue;
                }
                let package = if manager == PackageManager::Bun {
                    let Some(launcher) =
                        command_runner.canonicalize_path(&directory.join("agentty"))
                    else {
                        continue;
                    };
                    let Some(package) = launcher.parent() else {
                        continue;
                    };

                    package.to_path_buf()
                } else {
                    directory.join(AGENTTY_NPM_PACKAGE)
                };

                vec![package.join("node_modules/.bin_real/agentty")]
            }
        };
        if candidates.iter().any(|candidate| {
            command_runner.canonicalize_path(candidate).as_ref() == Some(&executable)
        }) {
            matches.push(manager);
        }
    }
    let [manager] = matches.as_slice() else {
        return Err(VersionError::UnknownInstallation);
    };

    run_package_update_with_runner(*manager, command_runner).await
}

/// Runs one identified global installer through the injected command boundary.
async fn run_package_update_with_runner(
    manager: PackageManager,
    command_runner: &dyn VersionCommandRunner,
) -> Result<String, VersionError> {
    let output = command_runner
        .run_command(
            manager.program(),
            manager.update_args(),
            VERSION_UPDATE_COMMAND_TIMEOUT,
        )
        .await?;

    output.successful_stdout(manager.program())
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
#[path = "version_test.rs"]
mod tests;
