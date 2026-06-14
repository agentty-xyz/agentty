//! Machine-scoped agent executable discovery.

use std::env;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::domain::agent::{AgentCliInfo, AgentKind};

/// Maximum time spent waiting for one provider CLI `--version` command.
const AGENT_CLI_VERSION_TIMEOUT: Duration = Duration::from_secs(2);
/// Poll interval used while waiting for a bounded provider CLI version probe.
const AGENT_CLI_VERSION_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Detects which provider CLIs are locally runnable on the current machine.
#[cfg_attr(test, mockall::automock)]
pub trait AgentAvailabilityProbe: Send + Sync {
    /// Returns the agent kinds whose backing CLI executable is available.
    fn available_agent_kinds(&self) -> Vec<AgentKind>;

    /// Returns available agent CLI executables and their detected versions.
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
    AgentKind::ALL
        .iter()
        .copied()
        .filter_map(|agent_kind| {
            let executable_path = executable_path_on_path(path_value, executable_name(agent_kind))?;

            Some((agent_kind, executable_path))
        })
        .map(|(agent_kind, executable_path)| {
            AgentCliInfo::new(agent_kind, detect_agent_cli_version(&executable_path))
        })
        .collect()
}

/// Returns agent kinds whose executables are present on one `PATH` value.
fn available_agent_kinds_from_path(path_value: Option<&OsStr>) -> Vec<AgentKind> {
    AgentKind::ALL
        .iter()
        .copied()
        .filter(|agent_kind| {
            executable_path_on_path(path_value, executable_name(*agent_kind)).is_some()
        })
        .collect()
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
    let mut child = Command::new(executable_path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started_at = Instant::now();

    loop {
        if child.try_wait().ok()?.is_some() {
            return child.wait_with_output().ok();
        }

        if started_at.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();

            return None;
        }

        std::thread::sleep(
            AGENT_CLI_VERSION_POLL_INTERVAL.min(timeout.saturating_sub(started_at.elapsed())),
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
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    /// Ensures executable names stay aligned with provider command names.
    fn test_executable_name_matches_agent_cli_names() {
        // Arrange / Act / Assert
        assert_eq!(executable_name(AgentKind::Antigravity), "agy");
        assert_eq!(executable_name(AgentKind::Gemini), "gemini");
        assert_eq!(executable_name(AgentKind::Claude), "claude");
        assert_eq!(executable_name(AgentKind::Codex), "codex");
    }

    #[test]
    /// Ensures the production probe reports only agent kinds whose
    /// executables are present on the current `PATH`.
    fn test_real_agent_availability_probe_filters_missing_executables() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let antigravity_path = temp_directory.path().join("agy");
        let codex_path = temp_directory.path().join("codex");
        let gemini_path = temp_directory.path().join("gemini");
        fs::write(&antigravity_path, "").expect("failed to create agy executable");
        fs::write(&codex_path, "").expect("failed to create codex executable");
        fs::write(&gemini_path, "").expect("failed to create gemini executable");
        fs::set_permissions(&antigravity_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark agy executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");
        fs::set_permissions(&gemini_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark gemini executable");
        let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

        // Act
        let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

        // Assert
        assert_eq!(
            available_agent_kinds,
            vec![AgentKind::Gemini, AgentKind::Antigravity, AgentKind::Codex]
        );
    }

    #[test]
    /// Ensures available CLI metadata includes parsed command versions.
    fn test_available_agent_clis_from_path_includes_versions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        fs::write(&codex_path, "#!/bin/sh\nprintf 'codex-cli 1.2.3\\n'\n")
            .expect("failed to create codex executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");
        let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

        // Act
        let available_agent_clis = available_agent_clis_from_path(Some(path_value.as_os_str()));

        // Assert
        assert_eq!(
            available_agent_clis,
            vec![AgentCliInfo::new(
                AgentKind::Codex,
                Some("1.2.3".to_string())
            )]
        );
    }

    #[test]
    /// Ensures unresponsive CLI version commands time out without returning a
    /// version.
    fn test_detect_agent_cli_version_with_timeout_handles_hanging_commands() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        fs::write(&codex_path, "#!/bin/sh\nwhile :; do :; done\n")
            .expect("failed to create hanging codex executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");

        // Act
        let detected_version =
            detect_agent_cli_version_with_timeout(&codex_path, Duration::from_millis(50));

        // Assert
        assert_eq!(detected_version, None);
    }

    #[test]
    /// Ensures non-version text falls back to the first useful output line.
    fn test_parse_agent_cli_version_output_falls_back_to_line() {
        // Arrange
        let output = "Claude Code development build\n";

        // Act
        let parsed_version = parse_agent_cli_version_output(output);

        // Assert
        assert_eq!(
            parsed_version,
            Some("Claude Code development build".to_string())
        );
    }

    #[test]
    /// Ensures probe discovery ignores non-executable files even when their
    /// names match supported agent CLIs.
    fn test_real_agent_availability_probe_ignores_non_executable_files() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        fs::write(&codex_path, "").expect("failed to create codex file");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o644))
            .expect("failed to mark codex non-executable");
        let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

        // Act
        let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

        // Assert
        assert!(available_agent_kinds.is_empty());
    }
}
