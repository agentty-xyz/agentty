//! Machine-scoped agent executable discovery.

use std::env;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::model::agent::{AgentCliInfo, AgentKind};

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

/// Runs one available CLI's update command, then extracts the installed
/// version token from a fresh version probe.
fn refresh_agent_cli_version(
    agent_kind: AgentKind,
    executable_path: &Path,
    path_value: Option<&OsStr>,
) -> Option<String> {
    run_agent_cli_update(agent_kind, executable_path, path_value);

    detect_agent_cli_version(executable_path)
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
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::tempdir;

    use super::*;

    #[test]
    /// Ensures executable names stay aligned with provider command names.
    fn test_executable_name_matches_agent_cli_names() {
        // Arrange / Act / Assert
        assert_eq!(executable_name(AgentKind::Antigravity), "agy");
        assert_eq!(executable_name(AgentKind::Claude), "claude");
        assert_eq!(executable_name(AgentKind::Codex), "codex");
        assert_eq!(executable_name(AgentKind::Gemini), "gemini");
    }

    #[test]
    /// Ensures the production probe reports only agent kinds whose
    /// executables are present on the current `PATH`.
    fn test_real_agent_availability_probe_filters_missing_executables() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let antigravity_path = temp_directory.path().join("agy");
        let codex_path = temp_directory.path().join("codex");
        fs::write(&antigravity_path, "").expect("failed to create agy executable");
        fs::write(&codex_path, "").expect("failed to create codex executable");
        fs::set_permissions(&antigravity_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark agy executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");
        let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

        // Act
        let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

        // Assert
        assert_eq!(
            available_agent_kinds,
            vec![AgentKind::Antigravity, AgentKind::Codex]
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
    /// Ensures the startup CLI refresh runs `update` before probing the
    /// visible version.
    fn test_available_agent_clis_from_path_updates_before_version_probe() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        let version_path = temp_directory.path().join("codex-version");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then printf '9.9.9-updated\\n' > \"{}\"; exit \
             0; fi\nif [ \"$1\" = \"--version\" ]; then if [ -f \"{}\" ]; then read version < \
             \"{}\"; else version='1.0.0-old'; fi; printf 'codex-cli %s\\n' \"$version\"; exit 0; \
             fi\nexit 1\n",
            version_path.display(),
            version_path.display(),
            version_path.display(),
        );
        fs::write(&codex_path, script).expect("failed to create codex executable");
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
                Some("9.9.9-updated".to_string())
            )]
        );
        assert!(version_path.exists());
    }

    #[test]
    /// Ensures CLI refreshes start independently so one slow provider does
    /// not delay every following provider.
    fn test_refresh_agent_cli_versions_runs_providers_concurrently() {
        // Arrange
        let codex_started = Arc::new(AtomicBool::new(false));
        let refresh_cli_version = {
            let codex_started = Arc::clone(&codex_started);

            move |_agent_kind: AgentKind, executable_path: &Path| {
                if executable_path.file_name() == Some(OsStr::new("agy")) {
                    let started_at = Instant::now();
                    while !codex_started.load(Ordering::SeqCst)
                        && started_at.elapsed() < Duration::from_millis(200)
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }

                    return if codex_started.load(Ordering::SeqCst) {
                        Some("agy-concurrent".to_string())
                    } else {
                        Some("agy-sequential".to_string())
                    };
                }

                if executable_path.file_name() == Some(OsStr::new("codex")) {
                    codex_started.store(true, Ordering::SeqCst);

                    return Some("codex-current".to_string());
                }

                None
            }
        };
        let executable_agent_clis = vec![
            (AgentKind::Antigravity, PathBuf::from("agy")),
            (AgentKind::Codex, PathBuf::from("codex")),
        ];

        // Act
        let agent_clis = refresh_agent_cli_versions(executable_agent_clis, refresh_cli_version);

        // Assert
        assert_eq!(
            agent_clis,
            vec![
                AgentCliInfo::new(AgentKind::Antigravity, Some("agy-concurrent".to_string())),
                AgentCliInfo::new(AgentKind::Codex, Some("codex-current".to_string())),
            ]
        );
    }

    #[test]
    /// Ensures failed CLI updates do not prevent the post-update version
    /// probe from refreshing the row.
    fn test_refresh_agent_cli_version_probes_version_when_update_fails() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        fs::write(
            &codex_path,
            "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 1; fi\nif [ \"$1\" = \"--version\" \
             ]; then printf 'codex-cli 1.2.3\\n'; exit 0; fi\nexit 1\n",
        )
        .expect("failed to create codex executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");

        // Act
        let detected_version = refresh_agent_cli_version(AgentKind::Codex, &codex_path, None);

        // Assert
        assert_eq!(detected_version, Some("1.2.3".to_string()));
    }

    #[test]
    /// Ensures npm-global Gemini installations update through npm instead of
    /// treating `update` as an interactive Gemini query.
    fn test_available_agent_clis_from_path_updates_npm_global_gemini() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let bin_directory = temp_directory.path().join("bin");
        let gemini_package_directory = temp_directory
            .path()
            .join("lib/node_modules/@google/gemini-cli/bundle");
        let gemini_package_path = gemini_package_directory.join("gemini.js");
        let gemini_path = bin_directory.join("gemini");
        let npm_path = bin_directory.join("npm");
        let version_path = temp_directory.path().join("gemini-version");
        fs::create_dir_all(&bin_directory).expect("failed to create bin directory");
        fs::create_dir_all(&gemini_package_directory)
            .expect("failed to create Gemini package directory");
        fs::write(
            &gemini_package_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 91; fi\nif [ \"$1\" = \
                 \"--version\" ]; then if [ -f \"{}\" ]; then read version < \"{}\"; else \
                 version='1.0.0-old'; fi; printf 'gemini %s\\n' \"$version\"; exit 0; fi\nexit 1\n",
                version_path.display(),
                version_path.display(),
            ),
        )
        .expect("failed to create Gemini executable");
        fs::write(
            &npm_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"install\" ] && [ \"$2\" = \"-g\" ] && [ \"$3\" = \
                 \"@google/gemini-cli@latest\" ]; then printf '9.9.9-updated\\n' > \"{}\"; exit \
                 0; fi\nexit 1\n",
                version_path.display(),
            ),
        )
        .expect("failed to create npm executable");
        fs::set_permissions(&gemini_package_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark Gemini executable");
        fs::set_permissions(&npm_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark npm executable");
        symlink(&gemini_package_path, &gemini_path).expect("failed to link Gemini executable");
        let path_value = env::join_paths([&bin_directory]).expect("valid path");

        // Act
        let available_agent_clis = available_agent_clis_from_path(Some(path_value.as_os_str()));

        // Assert
        assert_eq!(
            available_agent_clis,
            vec![AgentCliInfo::new(
                AgentKind::Gemini,
                Some("9.9.9-updated".to_string())
            )]
        );
        assert_eq!(
            fs::read_to_string(version_path).expect("updated Gemini version"),
            "9.9.9-updated\n"
        );
    }

    #[test]
    /// Ensures Gemini installations with an unknown owner do not launch the
    /// removed native update command.
    fn test_run_agent_cli_update_skips_unknown_gemini_installation() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let gemini_path = temp_directory.path().join("gemini");
        let update_marker_path = temp_directory.path().join("gemini-update");
        fs::write(
            &gemini_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then touch \"{}\"; exit 0; fi\nexit 1\n",
                update_marker_path.display(),
            ),
        )
        .expect("failed to create Gemini executable");
        fs::set_permissions(&gemini_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark Gemini executable");

        // Act
        let did_update = run_agent_cli_update_with_timeout(
            AgentKind::Gemini,
            &gemini_path,
            None,
            Duration::from_millis(100),
        );

        // Assert
        assert!(!did_update);
        assert!(!update_marker_path.exists());
    }

    #[test]
    /// Ensures noisy CLI update commands cannot block on unread pipe buffers.
    fn test_run_agent_cli_update_discards_output_without_pipe_backpressure() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let codex_path = temp_directory.path().join("codex");
        fs::write(
            &codex_path,
            "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then i=0; while [ \"$i\" -lt 4096 ]; do \
             printf \
             '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\\n'; \
             printf \
             'fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\\n' \
             >&2; i=$((i + 1)); done; exit 0; fi\nexit 1\n",
        )
        .expect("failed to create noisy codex executable");
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755))
            .expect("failed to mark codex executable");

        // Act
        let did_finish = run_agent_cli_update_with_timeout(
            AgentKind::Codex,
            &codex_path,
            None,
            Duration::from_secs(10),
        );

        // Assert
        assert!(did_finish);
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
