//! Machine-scoped agent executable discovery.

use std::env;
use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use semver::Version;

use crate::model::agent::{AgentCliInfo, AgentKind};

/// Oldest Antigravity CLI release supported by Agentty's native stream
/// protocol.
const ANTIGRAVITY_MINIMUM_VERSION: Version = Version::new(1, 1, 18);
/// Maximum time spent waiting for one provider CLI `--version` command.
const AGENT_CLI_VERSION_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum time spent waiting for one provider CLI `update` command.
const AGENT_CLI_UPDATE_TIMEOUT: Duration = Duration::from_mins(5);
/// Poll interval used while waiting for one bounded provider CLI subprocess.
const AGENT_CLI_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Canonical npm-global path segment for the Gemini CLI package.
const GEMINI_NPM_PACKAGE_PATH: &str = "/lib/node_modules/@google/gemini-cli/";
/// npm package spec used to refresh a globally installed Gemini CLI.
const GEMINI_NPM_PACKAGE_SPEC: &str = "@google/gemini-cli@latest";

/// Cached result of validating one exact Antigravity executable.
#[derive(Clone)]
struct AntigravityCompatibilitySnapshot {
    fingerprint: Option<AntigravityExecutableFingerprint>,
    result: Result<(), String>,
}

/// Metadata used to invalidate compatibility after `agy` changes on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AntigravityExecutableFingerprint {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    modified_nanoseconds: i64,
    modified_seconds: i64,
    path: PathBuf,
}

/// Process-wide Antigravity compatibility snapshot populated by startup
/// discovery and CLI refresh.
static ANTIGRAVITY_COMPATIBILITY: OnceLock<Mutex<Option<AntigravityCompatibilitySnapshot>>> =
    OnceLock::new();

/// Executable plus arguments for one provider CLI startup update.
struct AgentCliUpdateCommand {
    args: &'static [&'static str],
    executable_path: PathBuf,
}

impl AgentCliUpdateCommand {
    /// Creates one provider update command.
    fn new(executable_path: PathBuf, args: &'static [&'static str]) -> Self {
        Self {
            args,
            executable_path,
        }
    }
}

/// Detects which provider CLIs are locally runnable on the current machine.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait AgentAvailabilityProbe: Send + Sync {
    /// Returns the agent kinds whose backing CLI executable is available.
    fn available_agent_kinds(&self) -> Vec<AgentKind>;

    /// Returns available agent CLI executables and their refreshed versions.
    fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        AgentCliInfo::from_kinds(&self.available_agent_kinds())
    }
}

/// Production availability probe backed by `PATH` executable discovery.
pub struct RealAgentAvailabilityProbe;

impl AgentAvailabilityProbe for RealAgentAvailabilityProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        available_agent_kinds_from_path(env::var_os("PATH").as_deref())
    }

    fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        available_agent_clis_from_path(env::var_os("PATH").as_deref())
    }
}

/// Availability probe that returns one caller-provided snapshot.
pub struct StaticAgentAvailabilityProbe {
    /// Agent kinds reported as available by the static probe.
    pub available_agent_kinds: Vec<AgentKind>,
}

impl AgentAvailabilityProbe for StaticAgentAvailabilityProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        self.available_agent_kinds.clone()
    }
}

/// Returns the CLI executable name used by the provided agent kind.
#[must_use]
pub fn executable_name(agent_kind: AgentKind) -> &'static str {
    agent_kind.executable_name()
}

/// Returns available agent CLI metadata from one `PATH` value.
fn available_agent_clis_from_path(path_value: Option<&OsStr>) -> Vec<AgentCliInfo> {
    let executable_agent_clis = AgentKind::ALL
        .iter()
        .copied()
        .filter_map(|agent_kind| {
            let executable_path = executable_path_on_path(path_value, executable_name(agent_kind))?;

            Some((agent_kind, executable_path))
        })
        .collect();

    refresh_agent_cli_versions(executable_agent_clis, |agent_kind, executable_path| {
        refresh_agent_cli_version(agent_kind, executable_path, path_value)
    })
}

/// Returns agent kinds whose executables are present on one `PATH` value.
fn available_agent_kinds_from_path(path_value: Option<&OsStr>) -> Vec<AgentKind> {
    AgentKind::ALL
        .iter()
        .copied()
        .filter(|agent_kind| {
            if *agent_kind == AgentKind::Antigravity {
                return ensure_antigravity_cli_supported_on_path(path_value).is_ok();
            }

            executable_path_on_path(path_value, executable_name(*agent_kind)).is_some()
        })
        .collect()
}

/// Validates one Antigravity executable resolved from the provided `PATH`.
fn ensure_antigravity_cli_supported_on_path(path_value: Option<&OsStr>) -> Result<(), String> {
    let Some(executable_path) =
        executable_path_on_path(path_value, executable_name(AgentKind::Antigravity))
    else {
        let result = Err(format!(
            "Antigravity CLI {ANTIGRAVITY_MINIMUM_VERSION} or newer is required, but `agy` was \
             not found on `PATH`. Install it or run `agy update`, then restart Agentty."
        ));

        cache_antigravity_cli_support(None, result.clone());

        return result;
    };
    let detected_version = detect_agent_cli_version(&executable_path);
    let result = validate_antigravity_cli_version(detected_version.as_deref());

    cache_antigravity_cli_support(Some(&executable_path), result.clone());

    result
}

/// Checks one `PATH` against the cached Antigravity compatibility snapshot.
pub(super) fn ensure_cached_antigravity_cli_supported_on_path(
    path_value: Option<&OsStr>,
) -> Result<(), String> {
    let executable_path =
        executable_path_on_path(path_value, executable_name(AgentKind::Antigravity));
    let current_fingerprint = executable_path
        .as_deref()
        .and_then(antigravity_executable_fingerprint);
    let snapshot = ANTIGRAVITY_COMPATIBILITY
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|snapshot| snapshot.clone());

    validate_cached_antigravity_cli_support(snapshot.as_ref(), current_fingerprint.as_ref())
}

/// Returns a cached result only when it describes the current executable.
fn validate_cached_antigravity_cli_support(
    snapshot: Option<&AntigravityCompatibilitySnapshot>,
    current_fingerprint: Option<&AntigravityExecutableFingerprint>,
) -> Result<(), String> {
    let Some(snapshot) = snapshot else {
        return Err(
            "Antigravity CLI has not been validated yet. Wait for CLI discovery to finish or \
             restart Agentty, then retry."
                .to_string(),
        );
    };
    if snapshot.fingerprint.as_ref() != current_fingerprint {
        return Err(
            "Antigravity CLI installation changed after Agentty validated it. Wait for CLI \
             discovery to finish or restart Agentty, then retry."
                .to_string(),
        );
    }

    snapshot.result.clone()
}

/// Stores one compatibility result alongside the exact executable it covers.
fn cache_antigravity_cli_support(executable_path: Option<&Path>, result: Result<(), String>) {
    let snapshot = AntigravityCompatibilitySnapshot {
        fingerprint: executable_path.and_then(antigravity_executable_fingerprint),
        result,
    };
    if let Ok(mut cached_snapshot) = ANTIGRAVITY_COMPATIBILITY
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        *cached_snapshot = Some(snapshot);
    }
}

/// Captures stable metadata for one resolved Antigravity executable.
fn antigravity_executable_fingerprint(
    executable_path: &Path,
) -> Option<AntigravityExecutableFingerprint> {
    let metadata = executable_path.metadata().ok()?;

    Some(AntigravityExecutableFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_nanoseconds: metadata.mtime_nsec(),
        modified_seconds: metadata.mtime(),
        mode: metadata.mode(),
        path: executable_path.to_path_buf(),
    })
}

/// Validates one parsed Antigravity version string against the supported
/// minimum.
fn validate_antigravity_cli_version(detected_version: Option<&str>) -> Result<(), String> {
    let Some(detected_version) = detected_version else {
        return Err(format!(
            "Antigravity CLI {ANTIGRAVITY_MINIMUM_VERSION} or newer is required, but `agy \
             --version` did not report a version. Run `agy update`, then retry."
        ));
    };
    let normalized_version = detected_version
        .strip_prefix('v')
        .unwrap_or(detected_version);
    let parsed_version = Version::parse(normalized_version).map_err(|_| {
        format!(
            "Antigravity CLI {ANTIGRAVITY_MINIMUM_VERSION} or newer is required, but `agy \
             --version` reported `{detected_version}`. Run `agy update`, then retry."
        )
    })?;
    if parsed_version < ANTIGRAVITY_MINIMUM_VERSION {
        return Err(format!(
            "Antigravity CLI {ANTIGRAVITY_MINIMUM_VERSION} or newer is required, but \
             `{detected_version}` is installed. Run `agy update`, then retry."
        ));
    }

    Ok(())
}

/// Returns the first executable path matching one command name on `PATH`.
fn executable_path_on_path(path_value: Option<&OsStr>, executable_name: &str) -> Option<PathBuf> {
    path_value
        .map(env::split_paths)
        .into_iter()
        .flatten()
        .map(|path_entry| candidate_path_for_executable_name(&path_entry, executable_name))
        .find(|candidate_path| is_executable_file(candidate_path))
}

/// Returns the candidate filesystem path for one executable name within a
/// single `PATH` entry.
fn candidate_path_for_executable_name(path_entry: &Path, executable_name: &str) -> PathBuf {
    path_entry.join(executable_name)
}

/// Returns whether the candidate path is a regular file with at least one
/// execute bit set.
fn is_executable_file(candidate_path: &Path) -> bool {
    let Ok(metadata) = candidate_path.metadata() else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    metadata.permissions().mode() & 0o111 != 0
}

/// Runs one available CLI's update command, then extracts the installed
/// version token from a fresh version probe.
fn refresh_agent_cli_version(
    agent_kind: AgentKind,
    executable_path: &Path,
    path_value: Option<&OsStr>,
) -> Option<String> {
    run_agent_cli_update(agent_kind, executable_path, path_value);

    let detected_version = detect_agent_cli_version(executable_path);
    if agent_kind == AgentKind::Antigravity {
        let result = validate_antigravity_cli_version(detected_version.as_deref());
        cache_antigravity_cli_support(Some(executable_path), result);
    }

    detected_version
}

/// Refreshes all available CLI versions concurrently while preserving
/// provider display order.
fn refresh_agent_cli_versions(
    executable_agent_clis: Vec<(AgentKind, PathBuf)>,
    refresh_cli_version: impl Fn(AgentKind, &Path) -> Option<String> + Sync,
) -> Vec<AgentCliInfo> {
    std::thread::scope(|scope| {
        let refresh_cli_version = &refresh_cli_version;
        let refresh_handles = executable_agent_clis
            .into_iter()
            .map(|(agent_kind, executable_path)| {
                (
                    agent_kind,
                    scope.spawn(move || refresh_cli_version(agent_kind, &executable_path)),
                )
            })
            .collect::<Vec<_>>();

        refresh_handles
            .into_iter()
            .map(|(agent_kind, refresh_handle)| {
                AgentCliInfo::new(agent_kind, refresh_handle.join().unwrap_or(None))
            })
            .collect()
    })
}

/// Runs one available CLI's best-effort provider or package-manager update.
fn run_agent_cli_update(agent_kind: AgentKind, executable_path: &Path, path_value: Option<&OsStr>) {
    let _ = run_agent_cli_update_with_timeout(
        agent_kind,
        executable_path,
        path_value,
        AGENT_CLI_UPDATE_TIMEOUT,
    );
}

/// Runs one available CLI's best-effort update with a caller-provided timeout.
fn run_agent_cli_update_with_timeout(
    agent_kind: AgentKind,
    executable_path: &Path,
    path_value: Option<&OsStr>,
    timeout: Duration,
) -> bool {
    let Some(update_command) = agent_cli_update_command(agent_kind, executable_path, path_value)
    else {
        return false;
    };

    command_status_with_timeout(&update_command, timeout).is_some()
}

/// Builds the supported startup update command for one provider CLI.
fn agent_cli_update_command(
    agent_kind: AgentKind,
    executable_path: &Path,
    path_value: Option<&OsStr>,
) -> Option<AgentCliUpdateCommand> {
    if agent_kind == AgentKind::Gemini {
        return gemini_npm_update_command(executable_path, path_value);
    }

    Some(AgentCliUpdateCommand::new(
        executable_path.to_path_buf(),
        &["update"],
    ))
}

/// Builds Gemini's supported npm-global update command when the discovered
/// executable resolves into the global Gemini CLI package.
///
/// Canonicalization failure is treated as an unknown installation because
/// Agentty cannot safely prove that npm owns the executable.
fn gemini_npm_update_command(
    executable_path: &Path,
    path_value: Option<&OsStr>,
) -> Option<AgentCliUpdateCommand> {
    let canonical_executable_path = executable_path.canonicalize().ok()?;
    let normalized_executable_path = canonical_executable_path.to_string_lossy();
    if !normalized_executable_path.contains(GEMINI_NPM_PACKAGE_PATH) {
        return None;
    }

    let npm_executable_path = executable_path_on_path(path_value, "npm")?;

    Some(AgentCliUpdateCommand::new(
        npm_executable_path,
        &["install", "-g", GEMINI_NPM_PACKAGE_SPEC],
    ))
}

/// Runs one available CLI's version command and extracts the installed
/// version token from its output.
fn detect_agent_cli_version(executable_path: &Path) -> Option<String> {
    detect_agent_cli_version_with_timeout(executable_path, AGENT_CLI_VERSION_TIMEOUT)
}

/// Runs one available CLI's version command with a caller-provided timeout.
fn detect_agent_cli_version_with_timeout(
    executable_path: &Path,
    timeout: Duration,
) -> Option<String> {
    let output = version_command_output(executable_path, timeout)?;
    if !output.status.success() {
        return None;
    }

    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    parse_agent_cli_version_output(&stdout_text)
        .or_else(|| parse_agent_cli_version_output(&stderr_text))
}

/// Runs one provider CLI `--version` command and stops waiting once the
/// timeout expires.
fn version_command_output(executable_path: &Path, timeout: Duration) -> Option<Output> {
    command_output_with_timeout(executable_path, &["--version"], timeout)
}

/// Runs one provider CLI command with output discarded and stops waiting once
/// the timeout expires.
fn command_status_with_timeout(
    update_command: &AgentCliUpdateCommand,
    timeout: Duration,
) -> Option<()> {
    let mut child = Command::new(&update_command.executable_path)
        .args(update_command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    wait_for_child_exit(&mut child, timeout)?;
    let _ = child.wait().ok()?;

    Some(())
}

/// Runs one provider CLI command and stops waiting once the timeout expires.
fn command_output_with_timeout(
    executable_path: &Path,
    args: &[&str],
    timeout: Duration,
) -> Option<Output> {
    let mut child = Command::new(executable_path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    wait_for_child_exit(&mut child, timeout)?;

    child.wait_with_output().ok()
}

/// Waits for one child process to exit, killing it when the timeout expires.
fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Option<()> {
    let started_at = Instant::now();

    loop {
        if child.try_wait().ok()?.is_some() {
            return Some(());
        }

        if started_at.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();

            return None;
        }

        std::thread::sleep(
            AGENT_CLI_COMMAND_POLL_INTERVAL.min(timeout.saturating_sub(started_at.elapsed())),
        );
    }
}

/// Parses a provider CLI version from the first useful `--version` output
/// line.
fn parse_agent_cli_version_output(output: &str) -> Option<String> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let version_token = line
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|character: char| {
                matches!(character, ',' | ';' | ':' | '(' | ')' | '[' | ']')
            })
        })
        .find(|token| {
            let normalized = token.strip_prefix('v').unwrap_or(token);

            normalized
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
                && normalized.contains('.')
        });

    Some(version_token.unwrap_or(line).to_string())
}

#[cfg(test)]
#[path = "availability_test.rs"]
mod tests;
