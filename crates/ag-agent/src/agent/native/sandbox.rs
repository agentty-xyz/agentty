//! Native sandbox grants for `bash` and the sandbox notices a run reports.
//!
//! Commands read the whole disk except credential stores, write only the
//! worktree, its Git metadata, and a per-session temporary directory, and
//! keep network access, like the sandboxes of other coding agents. Where
//! the native sandbox cannot run they run without it, and the run reports
//! why so the session view can tell the user how to fix it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_contracts::TurnNotice;
use ag_harness::RunEventKind;
use ag_harness::bash::{BashConfig, BashError};
use ag_harness::event::Item;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};

use super::config::chatgpt_auth_file;

/// How Agentty runs `bash`: inside the native sandbox, and without it, with
/// a warning, on a host where it cannot run.
pub(crate) const SANDBOX: Sandbox = Sandbox::Native {
    if_unavailable: IfUnavailable::Unsandboxed,
};
/// Tool caches redirected into the temporary directory, by variable and
/// subdirectory: commands cannot write the shared caches in the home
/// directory, which host tools would later trust.
const CACHE_ENVIRONMENT: [(&str, &str); 6] = [
    ("CARGO_HOME", "cargo"),
    ("GOCACHE", "go-build"),
    ("GOMODCACHE", "go-mod"),
    ("PIP_CACHE_DIR", "pip"),
    ("XDG_CACHE_HOME", "cache"),
    ("npm_config_cache", "npm"),
];
/// Variable naming the SSH agent's socket.
#[cfg(target_os = "macos")]
const SSH_AUTH_SOCK: &str = "SSH_AUTH_SOCK";
/// Name the harness's user-facing remedies use for the host application.
const APPLICATION: &str = "the application";

/// The places one run's commands may write and the Agentty files they may
/// not read.
pub(crate) struct CommandSandbox {
    /// Paths hidden from commands: the harness histories and the `ChatGPT`
    /// sign-in.
    hidden: Vec<PathBuf>,
    /// The run's own temporary directory, inside the hidden data root.
    temporary_directory: PathBuf,
}

impl CommandSandbox {
    /// Describes commands that write `temporary_directory` and cannot read
    /// `data_root`, other than that directory, or the `ChatGPT` sign-in
    /// `environment` names.
    pub(crate) fn new(
        data_root: &Path,
        temporary_directory: PathBuf,
        environment: &BTreeMap<String, String>,
    ) -> Self {
        let hidden = std::iter::once(data_root.to_path_buf())
            .chain(chatgpt_auth_file(environment).and_then(|file| std::path::absolute(file).ok()))
            .collect();

        Self {
            hidden,
            temporary_directory,
        }
    }

    /// Adds Agentty's grants to `config`: full-disk reads except credential
    /// stores and the hidden paths; writes to the worktree, its Git
    /// metadata, and the temporary directory, which also holds the tool
    /// caches; network access; and on macOS the SSH agent's socket.
    ///
    /// # Errors
    /// Returns the grant the harness rejects.
    pub(crate) fn grant(
        &self,
        config: BashConfig,
        environment: &BTreeMap<String, String>,
    ) -> Result<BashConfig, BashError> {
        let mut config = config
            .with_full_disk_read()?
            .with_write(PathBuf::from("."))?
            .with_git_writes()
            .with_network()
            .with_temporary_directory(self.temporary_directory.clone())?;
        for path in &self.hidden {
            config = config.with_hidden_path(path.clone())?;
        }
        for (name, directory) in CACHE_ENVIRONMENT {
            let value = self.temporary_directory.join(directory);
            config = config.with_environment(name.to_string(), value.display().to_string())?;
        }

        Ok(with_ssh_agent(config, environment))
    }
}

/// Runs the native sandbox helper when a harness started this process as
/// one and returns its exit code, which `main` must return at once;
/// otherwise returns `None` and makes this executable the helper of every
/// [`super::NativeHarnessConfig`] turn.
///
/// Call it first in `main`, before creating a runtime, threads, or anything
/// else, because a helper process must not run host code. Without the
/// call, commands run without the sandbox and every turn reports it.
#[must_use = "a helper process must return this exit code from `main` at once"]
pub fn dispatch_sandbox_helper() -> Option<ExitCode> {
    ag_harness::sandbox::dispatch()
}

/// Returns what `event` reports about the sandbox, if anything.
pub(crate) fn notice(event: &RunEventKind) -> Option<TurnNotice> {
    match event {
        RunEventKind::SandboxUnavailable(warning) => Some(unavailable(warning)),
        RunEventKind::ItemCompleted {
            item: Item::ToolCall(call),
            ..
        } if call.likely_sandbox_denial => Some(TurnNotice::LikelySandboxDenial),
        _ => None,
    }
}

/// Words `warning` for Agentty's users: whether commands still run
/// sandboxed, what is missing, and the fix, naming Agentty.
fn unavailable(warning: &SandboxWarning) -> TurnNotice {
    let message = match warning.reason {
        SandboxUnavailableReason::UnixSocketsUnsupported => warning.message.clone(),
        _ => format!("Commands run without the sandbox. {}", warning.message),
    };

    TurnNotice::SandboxUnavailable {
        message,
        remedy: warning.remedy.replace(APPLICATION, "Agentty"),
    }
}

/// Grants the SSH agent's socket that `environment` names, so Git over SSH
/// works with agent keys. Only the macOS sandbox can allow one socket by
/// path, so it is granted nowhere else; a socket that does not exist is
/// skipped.
#[cfg(target_os = "macos")]
fn with_ssh_agent(config: BashConfig, environment: &BTreeMap<String, String>) -> BashConfig {
    environment
        .get(SSH_AUTH_SOCK)
        .and_then(|socket| config.clone().with_unix_socket(PathBuf::from(socket)).ok())
        .unwrap_or(config)
}

/// Grants no socket: only the macOS sandbox can allow one by path.
#[cfg(not(target_os = "macos"))]
fn with_ssh_agent(config: BashConfig, _environment: &BTreeMap<String, String>) -> BashConfig {
    config
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;
