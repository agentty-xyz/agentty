//! Native sandbox grants for `bash` and the sandbox notices a run reports.
//!
//! Commands read the whole disk except credential stores, write only the
//! worktree, the session branch and the Git metadata committing to it
//! needs, and their session's temporary directory, and
//! keep network access, like the sandboxes of other coding agents. Where
//! the native sandbox cannot run, session commands run without it and the
//! run reports why so the session view can tell the user how to fix it,
//! while utility prompts refuse to run commands at all.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_contracts::TurnNotice;
use ag_harness::RunEventKind;
use ag_harness::bash::{BashConfig, BashError};
use ag_harness::event::Item;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};

use super::cargo_cache::TrustedCargoCache;
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
/// Entry of the temporary directory that a planted directory in place of a
/// trusted Cargo file moves to before it is removed.
const DISCARDED_CARGO_ENTRY: &str = ".agentty-discarded-cargo-entry";
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
/// Directories of Agentty's data directory hidden from commands: the stored
/// provider API keys and sign-ins, the application database, with every
/// session's transcript and the remote-control identity, and the pasted
/// images of every session.
const AGENTTY_PRIVATE_DIRECTORIES: [&str; 3] = ["auth", "db", "tmp"];
/// Name the harness's user-facing remedies use for the host application.
const APPLICATION: &str = "the application";

/// The places one run's commands may write and the Agentty files they may
/// not read.
pub(crate) struct CommandSandbox {
    /// The session branch commands may commit to; without one they cannot
    /// write Git metadata.
    git_branch: Option<String>,
    /// Paths hidden from commands: the harness histories, the `ChatGPT`
    /// sign-in, and Agentty's private directories.
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
    /// have planted with a link to the host's configuration, copies in what
    /// the project's `cargo_cache`, when there is one, holds for
    /// `workspace`'s `Cargo.lock`, and returns the sandbox, which also hides
    /// every project's cache.
    ///
    /// # Errors
    /// Returns an error when the Cargo directory cannot be made safe.
    pub(crate) async fn prepare(
        data_root: &Path,
        temporary_directory: PathBuf,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
        cargo_cache: Option<TrustedCargoCache>,
    ) -> Result<Self, String> {
        let host_config = Self::host_cargo_config(workspace, environment).await;
        let directory = temporary_directory.clone();
        let reused_cache = cargo_cache.clone();
        let worktree = workspace.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let cache = reused_cache
                .as_ref()
                .map(|cache| (cache, worktree.as_path()));

            Self::reset_cargo_home(&directory, host_config.as_deref(), cache)
        })
        .await
        .map_err(std::io::Error::other)
        .flatten()
        .map_err(|error| format!("Failed to prepare the sandbox Cargo directory: {error}"))?;
        let mut sandbox = Self::new(data_root, temporary_directory, environment);
        if let Some(cargo_cache) = cargo_cache {
            sandbox.hidden.push(cargo_cache.hidden_path().to_path_buf());
        }

        Ok(sandbox)
    }

    /// Describes commands that write `temporary_directory` and cannot read
    /// `data_root`, other than that directory, the `ChatGPT` sign-in
    /// `environment` names, or Agentty's stored API keys, application
    /// database and pasted images, which hold every provider key, every
    /// session's transcript and the remote-control identity.
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
            git_branch: None,
            hidden,
            temporary_directory,
        }
    }

    /// Lets commands commit to `branch`, the session branch checked out in
    /// the worktree, while every other reference stays read-only.
    pub(crate) fn with_git_branch(mut self, branch: Option<String>) -> Self {
        self.git_branch = branch;

        self
    }

    /// Adds Agentty's grants to `config`: full-disk reads except credential
    /// stores and the hidden paths; writes to the worktree, the Git writes
    /// that committing to the session branch needs, when there is one, and
    /// the temporary directory, which also holds the tool caches; and
    /// network access. No Unix-domain socket is granted, not even the SSH
    /// agent's, whose keys would sign for any host the network reaches;
    /// Agentty pushes outside the sandbox itself.
    ///
    /// # Errors
    /// Returns the grant the harness rejects.
    pub(crate) fn grant(&self, config: BashConfig) -> Result<BashConfig, BashError> {
        let mut config = config
            .with_full_disk_read()?
            .with_write(PathBuf::from("."))?;
        if let Some(branch) = &self.git_branch {
            config = config.with_git_writes(branch.clone())?;
        }
        let mut config = config
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

    /// Makes the `CARGO_HOME` of `temporary_directory` a real directory
    /// without the Cargo configuration and credential files commands may
    /// have planted, then links `host_config` into it and copies in what
    /// the cache of `cargo_cache` holds for its worktree's `Cargo.lock`.
    ///
    /// Only `temporary_directory` is resolved by path: commands cannot
    /// replace it, because its parent is hidden from them. Its Cargo
    /// directory is opened once without following links and every later
    /// change is made relative to that handle, so a link that a surviving
    /// command swaps in meanwhile cannot redirect them outside.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being made safe.
    fn reset_cargo_home(
        temporary_directory: &Path,
        host_config: Option<&Path>,
        cargo_cache: Option<(&TrustedCargoCache, &Path)>,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(temporary_directory)?;
        let temporary = fs::open(temporary_directory, Self::directory_flags(), Mode::empty())?;
        let planted = fs::statat(&temporary, CARGO_HOME_DIRECTORY, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| !FileType::from_raw_mode(stat.st_mode).is_dir());
        if planted {
            fs::unlinkat(&temporary, CARGO_HOME_DIRECTORY, AtFlags::empty())?;
        }
        // An existing directory is fine; any other failure fails the open.
        let _ = fs::mkdirat(&temporary, CARGO_HOME_DIRECTORY, Mode::RWXU);
        let cargo = fs::openat(
            &temporary,
            CARGO_HOME_DIRECTORY,
            Self::directory_flags(),
            Mode::empty(),
        )?;

        Self::clear_cargo_home(temporary_directory, &temporary, &cargo, host_config)?;
        if let Some((cargo_cache, worktree)) = cargo_cache {
            cargo_cache.copy_into(&cargo, worktree);
        }

        Ok(())
    }

    /// Returns the flags that open a directory handle without following a
    /// final link.
    pub(super) fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    /// Removes the trusted Cargo files from the open `cargo` directory and
    /// links `host_config` in their place. A planted directory is first
    /// moved out of `cargo` into `temporary`, the handle of
    /// `temporary_directory`, so its removal resolves no command-writable
    /// path but its own name.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps an entry from being removed.
    fn clear_cargo_home(
        temporary_directory: &Path,
        temporary: &OwnedFd,
        cargo: &OwnedFd,
        host_config: Option<&Path>,
    ) -> std::io::Result<()> {
        for name in CARGO_TRUSTED_FILES {
            match fs::statat(cargo, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode).is_dir() => {
                    let discarded = temporary_directory.join(DISCARDED_CARGO_ENTRY);
                    Self::remove_discarded(&discarded)?;
                    fs::renameat(cargo, name, temporary, DISCARDED_CARGO_ENTRY)?;
                    Self::remove_discarded(&discarded)?;
                }
                Ok(_) => fs::unlinkat(cargo, name, AtFlags::empty())?,
                Err(_) => {}
            }
        }
        if let Some(config) = host_config {
            // Best effort: without the link only those settings are lost.
            let _ = fs::symlinkat(config, cargo, CARGO_CONFIG_FILES[0]);
        }

        Ok(())
    }

    /// Removes `path`, an entry of the temporary directory, whatever it is:
    /// a link is removed itself, and the standard library removes a
    /// directory tree without following the links inside it.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps `path` from being removed.
    fn remove_discarded(path: &Path) -> std::io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(_) => Ok(()),
        }
    }

    /// Returns the host's Cargo configuration file, unless Cargo finds it
    /// anyway: it reads `.cargo/config.toml` in every ancestor of its
    /// working directory, which covers the default `~/.cargo` above a
    /// workspace inside the home directory.
    pub(super) async fn host_cargo_config(
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
