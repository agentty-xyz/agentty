//! Native sandbox grants for `bash` and the sandbox notices a run reports.
//!
//! Commands read the whole disk except credential stores, write only the
//! worktree, its Git metadata, and their session's temporary directory, and
//! keep network access, like the sandboxes of other coding agents. Where
//! the native sandbox cannot run, session commands run without it and the
//! run reports why so the session view can tell the user how to fix it,
//! while utility prompts refuse to run commands at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_contracts::TurnNotice;
use ag_harness::RunEventKind;
use ag_harness::bash::{BashConfig, BashError};
use ag_harness::event::Item;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};

use super::config::{agentty_home, chatgpt_auth_file};

/// How session turns run `bash`: inside the native sandbox, and without it,
/// with a warning the session shows, on a host where it cannot run.
pub(crate) const SESSION_SANDBOX: Sandbox = Sandbox::Native {
    if_unavailable: IfUnavailable::Unsandboxed,
};
/// How utility prompts run `bash`: only inside the native sandbox. They
/// report no notices, so where the sandbox cannot run they fail with its
/// remedy instead of running commands unseen.
pub(crate) const UTILITY_SANDBOX: Sandbox = Sandbox::Native {
    if_unavailable: IfUnavailable::Refuse,
};
/// Subdirectory of the temporary directory that commands use as
/// `CARGO_HOME`.
const CARGO_HOME_DIRECTORY: &str = "cargo";
/// Host Cargo configuration files, newest name first.
const CARGO_CONFIG_FILES: [&str; 2] = ["config.toml", "config"];
/// Entries of the sandbox `CARGO_HOME` that Cargo reads as configuration or
/// credentials, replaced before every turn whatever commands left there.
const CARGO_TRUSTED_FILES: [&str; 4] = ["config.toml", "config", "credentials.toml", "credentials"];
/// Tool caches redirected into the temporary directory, by variable and
/// subdirectory: commands cannot write the shared caches in the home
/// directory, which host tools would later trust.
const CACHE_ENVIRONMENT: [(&str, &str); 6] = [
    ("CARGO_HOME", CARGO_HOME_DIRECTORY),
    ("GOCACHE", "go-build"),
    ("GOMODCACHE", "go-mod"),
    ("PIP_CACHE_DIR", "pip"),
    ("XDG_CACHE_HOME", "cache"),
    ("npm_config_cache", "npm"),
];
/// Directories of Agentty's data directory hidden from commands: the
/// application database, with every session's transcript and the
/// remote-control identity, and the pasted images of every session.
const AGENTTY_PRIVATE_DIRECTORIES: [&str; 2] = ["db", "tmp"];
/// Name the harness's user-facing remedies use for the host application.
const APPLICATION: &str = "the application";

/// The places one run's commands may write and the Agentty files they may
/// not read.
pub(crate) struct CommandSandbox {
    /// Paths hidden from commands: the harness histories and the `ChatGPT`
    /// sign-in.
    hidden: Vec<PathBuf>,
    /// The session's own temporary directory, inside the hidden data root:
    /// `TMPDIR` and the tool caches.
    temporary_directory: PathBuf,
}

impl CommandSandbox {
    /// Prepares `temporary_directory`, which the host created inside
    /// `data_root` and commands may have written on earlier turns, for
    /// commands run in `workspace`: makes its `CARGO_HOME` a real directory,
    /// replaces the Cargo configuration and credential files commands could
    /// have planted with a link to the host's configuration, and returns
    /// the sandbox.
    ///
    /// # Errors
    /// Returns an error when the Cargo directory cannot be made safe.
    pub(crate) async fn prepare(
        data_root: &Path,
        temporary_directory: PathBuf,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let cargo_home = temporary_directory.join(CARGO_HOME_DIRECTORY);
        Self::reset_cargo_home(&cargo_home)
            .await
            .map_err(|error| format!("Failed to prepare the sandbox Cargo directory: {error}"))?;
        if let Some(config) = Self::host_cargo_config(workspace, environment).await {
            // Best effort: without the link only those settings are lost.
            let _ = tokio::fs::symlink(config, cargo_home.join(CARGO_CONFIG_FILES[0])).await;
        }

        Ok(Self::new(data_root, temporary_directory, environment))
    }

    /// Describes commands that write `temporary_directory` and cannot read
    /// `data_root`, other than that directory, the `ChatGPT` sign-in
    /// `environment` names, or Agentty's application database and pasted
    /// images, which hold every session's transcript and the remote-control
    /// identity.
    pub(crate) fn new(
        data_root: &Path,
        temporary_directory: PathBuf,
        environment: &BTreeMap<String, String>,
    ) -> Self {
        let agentty_files = agentty_home(environment)
            .into_iter()
            .flat_map(|home| AGENTTY_PRIVATE_DIRECTORIES.map(|directory| home.join(directory)));
        let hidden = std::iter::once(data_root.to_path_buf())
            .chain(chatgpt_auth_file(environment))
            .chain(agentty_files)
            .filter_map(|path| std::path::absolute(path).ok())
            .collect();

        Self {
            hidden,
            temporary_directory,
        }
    }

    /// Adds Agentty's grants to `config`: full-disk reads except credential
    /// stores and the hidden paths; writes to the worktree, its Git
    /// metadata, and the temporary directory, which also holds the tool
    /// caches; and network access. No Unix-domain socket is granted, not
    /// even the SSH agent's, whose keys would sign for any host the network
    /// reaches; Agentty pushes outside the sandbox itself.
    ///
    /// # Errors
    /// Returns the grant the harness rejects.
    pub(crate) fn grant(&self, config: BashConfig) -> Result<BashConfig, BashError> {
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

        Ok(config)
    }

    /// Makes `cargo_home` a real directory without the Cargo configuration
    /// and credential files commands may have planted, never following a
    /// link: a planted `cargo_home` link or file is removed itself, so
    /// nothing outside the temporary directory is touched.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being made safe.
    async fn reset_cargo_home(cargo_home: &Path) -> std::io::Result<()> {
        if let Ok(metadata) = tokio::fs::symlink_metadata(cargo_home).await
            && !metadata.is_dir()
        {
            tokio::fs::remove_file(cargo_home).await?;
        }
        tokio::fs::create_dir_all(cargo_home).await?;
        for name in CARGO_TRUSTED_FILES {
            let path = cargo_home.join(name);
            match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(&path).await?,
                Ok(_) => tokio::fs::remove_file(&path).await?,
                Err(_) => {}
            }
        }

        Ok(())
    }

    /// Returns the host's Cargo configuration file, unless Cargo finds it
    /// anyway: it reads `.cargo/config.toml` in every ancestor of its
    /// working directory, which covers the default `~/.cargo` above a
    /// workspace inside the home directory.
    async fn host_cargo_config(
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        let cargo_home = environment
            .get("CARGO_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                environment
                    .get("HOME")
                    .map(|home| Path::new(home).join(".cargo"))
            })?;
        let found_from_workspace = cargo_home.file_name() == Some(".cargo".as_ref())
            && cargo_home
                .parent()
                .is_some_and(|parent| workspace.starts_with(parent));
        if found_from_workspace {
            return None;
        }
        for name in CARGO_CONFIG_FILES {
            let config = cargo_home.join(name);
            if tokio::fs::metadata(&config)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Some(config);
            }
        }

        None
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

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;
