//! Native sandbox grants for `bash` and the sandbox notices a run reports.
//!
//! Commands read the whole disk except credential stores, write only the
//! worktree, its Git metadata, and their project's sandbox directory, and
//! keep network access, like the sandboxes of other coding agents. Where
//! the native sandbox cannot run they run without it, and the run reports
//! why so the session view can tell the user how to fix it.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_contracts::TurnNotice;
use ag_harness::RunEventKind;
use ag_harness::bash::{BashConfig, BashError};
use ag_harness::event::Item;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use sha2::{Digest as _, Sha256};

use super::config::chatgpt_auth_file;

/// How Agentty runs `bash`: inside the native sandbox, and without it, with
/// a warning, on a host where it cannot run.
pub(crate) const SANDBOX: Sandbox = Sandbox::Native {
    if_unavailable: IfUnavailable::Unsandboxed,
};
/// Directory under the data root holding one sandbox directory per project:
/// the commands' temporary directory and tool caches, which host tools
/// never read and which can be deleted between sessions.
const PROJECT_DIRECTORY_PARENT: &str = "sandbox-cache";
/// Subdirectory of the project directory that commands use as `CARGO_HOME`.
const CARGO_HOME_DIRECTORY: &str = "cargo";
/// Host Cargo configuration files, newest name first.
const CARGO_CONFIG_FILES: [&str; 2] = ["config.toml", "config"];
/// Bytes of the repository path digest that name a project directory.
const PROJECT_DIGEST_BYTES: usize = 6;
/// Longest repository name kept in a project directory name.
const PROJECT_NAME_CHARACTERS: usize = 32;
/// Tool caches redirected into the project directory, by variable and
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
    /// The project's sandbox directory, inside the hidden data root: the
    /// commands' temporary directory and tool caches.
    temporary_directory: PathBuf,
}

impl CommandSandbox {
    /// Creates the sandbox directory of the project `workspace` belongs to
    /// under `data_root`, links the host's Cargo configuration into it, and
    /// returns the sandbox of commands run in `workspace`.
    ///
    /// Every session and utility prompt of one repository shares the
    /// directory, so downloaded dependencies stay cached across them, and
    /// its path is stable, as the harness fingerprints it into the policy.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be created.
    pub(crate) async fn prepare(
        data_root: &Path,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let repository = Self::git_common_directory(workspace).await;
        let directory = data_root
            .join(PROJECT_DIRECTORY_PARENT)
            .join(Self::project_name(&repository));
        let cargo_home = directory.join(CARGO_HOME_DIRECTORY);
        tokio::fs::create_dir_all(&cargo_home)
            .await
            .map_err(|error| format!("Failed to create the sandbox cache directory: {error}"))?;
        Self::link_cargo_config(&cargo_home, workspace, environment).await;

        Ok(Self::new(data_root, directory, environment))
    }

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

    /// Returns the Git common directory of the repository `workspace`
    /// belongs to, resolved: the main repository's for a linked worktree,
    /// else `workspace/.git`, or `workspace` itself when neither exists.
    async fn git_common_directory(workspace: &Path) -> PathBuf {
        let dot_git = workspace.join(".git");
        // A linked worktree's `.git` is a file naming its own Git directory,
        // whose `commondir` file leads to the shared one.
        let git_directory = match tokio::fs::read_to_string(&dot_git).await {
            Ok(link) => link
                .strip_prefix("gitdir:")
                .map_or_else(|| dot_git.clone(), |path| workspace.join(path.trim())),
            Err(_) => dot_git,
        };
        let common_directory =
            match tokio::fs::read_to_string(git_directory.join("commondir")).await {
                Ok(relative) => git_directory.join(relative.trim()),
                Err(_) => git_directory,
            };

        tokio::fs::canonicalize(&common_directory)
            .await
            .unwrap_or_else(|_| workspace.to_path_buf())
    }

    /// Names the sandbox directory of the repository whose Git common
    /// directory is `repository`: its directory name, for people looking at
    /// the cache, and a digest of its path, which tells repositories of the
    /// same name apart.
    fn project_name(repository: &Path) -> String {
        let checkout = if repository.file_name() == Some(".git".as_ref()) {
            repository.parent()
        } else {
            Some(repository)
        };
        let name = checkout.and_then(Path::file_name).map_or_default(|name| {
            name.to_string_lossy()
                .chars()
                .filter(|character| character.is_ascii_alphanumeric() || "-_.".contains(*character))
                .take(PROJECT_NAME_CHARACTERS)
                .collect::<String>()
        });
        let digest = Sha256::digest(repository.as_os_str().as_bytes());
        let digest = hex::encode(&digest[..PROJECT_DIGEST_BYTES]);

        if name.is_empty() {
            digest
        } else {
            format!("{name}-{digest}")
        }
    }

    /// Links the host's Cargo configuration into the sandbox `cargo_home`
    /// when Cargo would not find it from `workspace` by itself, so its
    /// registries, mirrors, and build settings keep applying. Only the
    /// configuration is linked, never the credentials, which the sandbox
    /// hides. Linking is best effort: a missing link only loses those
    /// settings.
    async fn link_cargo_config(
        cargo_home: &Path,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) {
        let link = cargo_home.join(CARGO_CONFIG_FILES[0]);
        let is_link = tokio::fs::symlink_metadata(&link)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if is_link {
            let _ = tokio::fs::remove_file(&link).await;
        }
        if let Some(config) = Self::host_cargo_config(workspace, environment).await {
            let _ = tokio::fs::symlink(config, &link).await;
        }
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
