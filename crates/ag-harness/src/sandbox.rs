//! Native sandbox selection, availability warnings, and the helper entry
//! point.
//!
//! A [`crate::bash::BashConfig`] built with `BashConfig::new` or
//! `BashConfig::native` runs commands through the native sandbox: Bubblewrap
//! with Landlock and seccomp on Linux, Seatbelt on macOS. The sandbox needs
//! a helper executable, which a host provides in one of two ways:
//!
//! - call [`dispatch`] first in `main`, so the host's own executable acts as
//!   the helper and `BashConfig::native` needs no path; or
//! - install the standalone helper of the same version with `cargo install
//!   ag-harness --version <version> --features native-sandbox --bin
//!   ag-harness-sandbox` and pass its path to `BashConfig::new`.
//!
//! [`Sandbox`], set with `HarnessBuilder::sandbox` or `Harness::sandbox`,
//! decides what happens on
//! a host where the sandbox cannot run, for example without `bwrap` or with
//! unprivileged user namespaces disabled. Before a turn offering `bash` is
//! admitted, the harness checks the sandbox, until one check succeeds for
//! the configuration. By default, commands then run without isolation and
//! every such turn reports a [`SandboxWarning`] naming what is missing and
//! how to fix it, on its `TurnReport::sandbox_warning` and, for a streamed
//! run, as [`crate::RunEventKind::SandboxUnavailable`].
//! [`IfUnavailable::Refuse`] fails those turns with
//! [`crate::ErrorKind::SandboxUnavailable`] before they are admitted
//! instead.

#[cfg(feature = "native-sandbox")]
use std::collections::HashMap;
use std::fmt;
#[cfg(feature = "native-sandbox")]
use std::path::{Path, PathBuf};
#[cfg(feature = "native-sandbox")]
use std::sync::{Arc, Mutex, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "native-sandbox")]
use crate::Repository;
#[cfg(feature = "native-sandbox")]
use crate::bash::{BashArguments, BashConfig, CommandOutcome};
#[cfg(feature = "native-sandbox")]
use crate::command_settlement::Commands;
#[cfg(feature = "native-sandbox")]
use crate::execution::{self, BashTool, Host, NativeLaunch};
#[cfg(feature = "native-sandbox")]
use crate::turn::TurnError;

/// How Bash commands of a native configuration run.
///
/// Configurations from `BashConfig::for_executor` always run through their
/// own executor; this choice applies only to native ones.
///
/// Requires the `native-sandbox` feature.
#[cfg(feature = "native-sandbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Sandbox {
    /// Run commands inside the native sandbox; `if_unavailable` decides
    /// what happens on a host where it cannot run.
    Native {
        /// What a run does when the sandbox is unavailable.
        if_unavailable: IfUnavailable,
    },
    /// Run commands without isolation, with the harness process's own
    /// access, and report no warning: the host chose this, for example
    /// because it already runs inside a container or VM.
    Unsandboxed,
}

#[cfg(feature = "native-sandbox")]
impl Default for Sandbox {
    /// Returns [`Self::Native`] with [`IfUnavailable::Unsandboxed`].
    fn default() -> Self {
        Self::Native {
            if_unavailable: IfUnavailable::default(),
        }
    }
}

/// What a run does when the native sandbox cannot run on this host.
///
/// Requires the `native-sandbox` feature.
#[cfg(feature = "native-sandbox")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum IfUnavailable {
    /// Run commands without isolation and report a [`SandboxWarning`] in
    /// every turn that offers `bash`.
    #[default]
    Unsandboxed,
    /// Fail every turn that offers `bash` with
    /// [`crate::ErrorKind::SandboxUnavailable`] before it is admitted.
    Refuse,
}

/// What the native sandbox is missing on this host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SandboxUnavailableReason {
    /// The operating system has no native sandbox; only Linux and macOS do.
    UnsupportedPlatform,
    /// No helper executable: the configuration names none and the host did
    /// not call [`dispatch`].
    HelperMissing,
    /// The helper is missing, not executable, set-ID, or inside the
    /// workspace.
    HelperUntrusted,
    /// Bubblewrap (`bwrap`) is not installed at its configured path,
    /// `/usr/bin/bwrap` by default.
    BubblewrapMissing,
    /// Bubblewrap cannot create unprivileged user namespaces.
    UserNamespacesUnavailable,
    /// The configuration grants writes and the kernel cannot confine them
    /// with Landlock, which needs Linux 6.2 or later.
    LandlockUnavailable,
    /// `/usr/bin/sandbox-exec`, the macOS Seatbelt front end, is missing.
    SeatbeltMissing,
    /// Seatbelt cannot start a sandbox, for example because this process
    /// already runs inside one.
    SeatbeltUnavailable,
    /// The configuration grants Unix-domain sockets, which only the macOS
    /// sandbox can allow by path. Unlike the other reasons, commands still
    /// run inside the sandbox, with every Unix-domain socket denied.
    UnixSocketsUnsupported,
}

impl SandboxUnavailableReason {
    /// Returns a one-sentence description of what is missing.
    pub fn message(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "The native sandbox supports only Linux and macOS.",
            Self::HelperMissing => "No native sandbox helper is configured.",
            Self::HelperUntrusted => {
                "The native sandbox helper is missing, not executable, set-ID, or inside the \
                 workspace."
            }
            Self::BubblewrapMissing => "Bubblewrap (`bwrap`) is not installed.",
            Self::UserNamespacesUnavailable => {
                "Bubblewrap cannot create unprivileged user namespaces on this host."
            }
            Self::LandlockUnavailable => {
                "The kernel cannot confine write grants with Landlock (Linux 6.2 or later)."
            }
            Self::SeatbeltMissing => "`/usr/bin/sandbox-exec` is missing.",
            Self::SeatbeltUnavailable => {
                "Seatbelt cannot start a sandbox here, for example inside another sandbox."
            }
            Self::UnixSocketsUnsupported => {
                "The sandbox on this platform cannot allow Unix-domain sockets, so granted sockets \
                 such as an SSH agent's stay blocked."
            }
        }
    }

    /// Returns a one-sentence fix the person using the host application can
    /// apply, without naming APIs, so a host can show it as is.
    ///
    /// What the embedding host can change instead:
    ///
    /// - [`Self::UnsupportedPlatform`]: select an isolating executor with
    ///   `BashConfig::for_executor`.
    /// - [`Self::HelperMissing`]: call [`dispatch`] first in `main`, or install
    ///   `ag-harness-sandbox` and pass its path to `BashConfig::new`.
    /// - [`Self::HelperUntrusted`]: run from, or name a helper at, a location
    ///   outside the workspace.
    /// - [`Self::BubblewrapMissing`]: name another Bubblewrap with
    ///   `BashConfig::with_linux_bubblewrap`.
    /// - [`Self::LandlockUnavailable`]: drop the write, temporary-directory,
    ///   and Git-write grants, which need Landlock.
    /// - [`Self::SeatbeltUnavailable`]: a host already isolated another way can
    ///   choose `Sandbox::Unsandboxed`.
    /// - [`Self::UnixSocketsUnsupported`]: grant sockets only on macOS.
    pub fn remedy(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "Run the application on Linux or macOS.",
            Self::HelperMissing => {
                "Reinstall or update the application, which is missing its sandbox helper."
            }
            Self::HelperUntrusted => {
                "Install the application outside the project folder as a regular executable \
                 without set-user-ID or set-group-ID permissions."
            }
            Self::BubblewrapMissing => {
                "Install bubblewrap, for example with `sudo apt install bubblewrap` or `sudo dnf \
                 install bubblewrap`."
            }
            Self::UserNamespacesUnavailable => {
                "Allow unprivileged user namespaces for bubblewrap, for example with your \
                 distribution's AppArmor profile for `bwrap` or `sudo sysctl \
                 kernel.unprivileged_userns_clone=1`."
            }
            Self::LandlockUnavailable => "Upgrade to Linux 6.2 or later with Landlock enabled.",
            Self::SeatbeltMissing => {
                "Repair the macOS installation, which includes `/usr/bin/sandbox-exec`."
            }
            Self::SeatbeltUnavailable => {
                "Start the application outside other macOS sandboxes, for example from a regular \
                 Terminal window."
            }
            Self::UnixSocketsUnsupported => {
                "Run commands that need a socket, such as SSH with agent keys, outside the \
                 application, or use it on macOS."
            }
        }
    }
}

impl fmt::Display for SandboxUnavailableReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Warning that a run's Bash commands execute without the native sandbox,
/// or, for [`SandboxUnavailableReason::UnixSocketsUnsupported`], inside it
/// without a grant it cannot honor, with what is missing and how to fix it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SandboxWarning {
    /// What is missing, as [`SandboxUnavailableReason::message`] states it.
    pub message: String,
    /// What is missing.
    pub reason: SandboxUnavailableReason,
    /// How to fix it, as [`SandboxUnavailableReason::remedy`] states it.
    pub remedy: String,
}

impl SandboxWarning {
    /// Returns the warning for `reason`.
    pub fn new(reason: SandboxUnavailableReason) -> Self {
        Self {
            message: reason.message().to_string(),
            reason,
            remedy: reason.remedy().to_string(),
        }
    }
}

/// Runs the sandbox helper when the harness started this process as one
/// and returns its exit code, which `main` must return at once. Otherwise
/// returns `None` and registers this executable as the helper for
/// `BashConfig::native`.
///
/// Call it first in `main`, before creating a runtime, threads, or
/// anything else, because a helper process must not run host code:
///
/// ```no_run
/// # fn host() {}
/// fn main() -> std::process::ExitCode {
///     if let Some(helper) = ag_harness::sandbox::dispatch() {
///         return helper;
///     }
///     host();
///
///     std::process::ExitCode::SUCCESS
/// }
/// ```
///
/// Requires the `native-sandbox` feature.
#[cfg(feature = "native-sandbox")]
#[must_use = "a helper process must return this exit code from `main` at once"]
pub fn dispatch() -> Option<std::process::ExitCode> {
    if execution::is_helper(std::env::args_os()) {
        return Some(execution::run_launcher());
    }
    // Without a resolvable executable there is no helper to register, and
    // `BashConfig::native` reports `HelperMissing`.
    let _ = std::env::current_exe().map(|executable| DISPATCHED_HELPER.set(executable));

    None
}

/// What [`run_command`] reports about the one command it ran.
#[cfg(feature = "native-sandbox")]
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CommandRun {
    /// The command's exit, captured output, and termination.
    pub outcome: CommandOutcome,
    /// Why the command ran without the native sandbox its configuration
    /// selected, as a turn would report it; `None` when it ran inside it.
    pub warning: Option<SandboxWarning>,
}

/// Runs `command` once in the root of `repository` with the grants of
/// `configuration`, outside any turn, as a turn's `bash` call would run it:
/// a native configuration runs inside the native sandbox, and where that
/// cannot run, `sandbox` decides as it does for a turn. Hosts use it for
/// commands that run on their own behalf over content commands may have
/// written, such as a repository's Git hooks, so those run in the same
/// sandbox as the commands that wrote it. Nothing is journaled. Like a
/// turn, it takes a validated [`Repository`], whose root is the workspace
/// and whose `.git` the sandbox protects as Git metadata.
///
/// # Errors
/// Returns [`TurnError::SandboxUnavailable`] when the sandbox is unavailable
/// and `sandbox` refuses to run without it, and the error a turn's `bash`
/// call would report when the policy is rejected or execution fails.
#[cfg(feature = "native-sandbox")]
pub async fn run_command(
    configuration: &BashConfig,
    sandbox: Sandbox,
    repository: &Repository,
    command: &BashArguments,
) -> Result<CommandRun, TurnError> {
    let (configuration, warning) = SandboxGate::new(sandbox)
        .resolve(configuration, repository.root())
        .await?;
    let tool = BashTool::new(
        configuration,
        repository.root().to_path_buf(),
        Commands::default(),
        None,
    )?;
    // No tool call: a fixed call id, and no approval to leave the sandbox.
    let outcome = tool.execute(command, "command", false).await?;

    Ok(CommandRun { outcome, warning })
}

/// This executable, once [`dispatch`] returned in it.
#[cfg(feature = "native-sandbox")]
static DISPATCHED_HELPER: OnceLock<PathBuf> = OnceLock::new();

/// A harness's [`Sandbox`] choice with the availability it confirmed,
/// shared by its clones.
#[cfg(feature = "native-sandbox")]
#[derive(Clone)]
pub(crate) struct SandboxGate {
    /// Confirmed launches by configuration fingerprint, workspace, and
    /// helper. Only successes are kept, so a remedy applied while the host
    /// runs takes effect at the next run.
    available: Arc<Mutex<HashMap<ProbeKey, NativeLaunch>>>,
    /// The host the sandbox runs on, read when the harness was built.
    host: Host,
    sandbox: Sandbox,
}

#[cfg(feature = "native-sandbox")]
impl SandboxGate {
    /// Returns a gate applying `sandbox` on this host.
    pub(crate) fn new(sandbox: Sandbox) -> Self {
        Self {
            available: Arc::default(),
            host: Host::current(),
            sandbox,
        }
    }

    /// Returns the configuration `configuration`'s commands in `workspace`
    /// run with, and the warning to report when they run without the native
    /// sandbox it selected.
    ///
    /// # Errors
    /// Returns [`TurnError::SandboxUnavailable`] when the sandbox is
    /// unavailable and the harness refuses to run without it.
    pub(crate) async fn resolve(
        &self,
        configuration: &BashConfig,
        workspace: &Path,
    ) -> Result<(BashConfig, Option<SandboxWarning>), TurnError> {
        if configuration.executor.is_some() {
            return Ok((configuration.clone(), None));
        }
        let if_unavailable = match self.sandbox {
            Sandbox::Native { if_unavailable } => if_unavailable,
            Sandbox::Unsandboxed => return Ok((configuration.unsandboxed(), None)),
        };
        let helper = configuration
            .snapshot
            .launcher
            .clone()
            .or_else(|| DISPATCHED_HELPER.get().cloned());
        let checked = self.check(configuration, helper, workspace).await;

        match (checked, if_unavailable) {
            (Ok(launch), _) => {
                self.enforce_grants(configuration.launched_through(launch), if_unavailable)
            }
            (Err(reason), IfUnavailable::Unsandboxed) => Ok((
                configuration.unsandboxed(),
                Some(SandboxWarning::new(reason)),
            )),
            (Err(reason), IfUnavailable::Refuse) => Err(TurnError::SandboxUnavailable(reason)),
        }
    }

    /// Returns `configuration`, which runs inside the native sandbox, with a
    /// warning when this host cannot honor one of its grants: Unix-domain
    /// sockets outside macOS stay denied.
    ///
    /// # Errors
    /// Returns [`TurnError::SandboxUnavailable`] for such a grant when the
    /// harness refuses to run without the full sandbox.
    fn enforce_grants(
        &self,
        configuration: BashConfig,
        if_unavailable: IfUnavailable,
    ) -> Result<(BashConfig, Option<SandboxWarning>), TurnError> {
        let unsupported = (self.host.platform != "macos"
            && !configuration.snapshot.unix_sockets.is_empty())
        .then_some(SandboxUnavailableReason::UnixSocketsUnsupported);

        match (unsupported, if_unavailable) {
            (None, _) => Ok((configuration, None)),
            (Some(reason), IfUnavailable::Unsandboxed) => {
                Ok((configuration, Some(SandboxWarning::new(reason))))
            }
            (Some(reason), IfUnavailable::Refuse) => Err(TurnError::SandboxUnavailable(reason)),
        }
    }

    /// Returns the availability of the native sandbox for `configuration`
    /// in `workspace` through `helper`, checking it until it is confirmed
    /// once.
    async fn check(
        &self,
        configuration: &BashConfig,
        helper: Option<PathBuf>,
        workspace: &Path,
    ) -> Result<NativeLaunch, SandboxUnavailableReason> {
        let key = ProbeKey {
            configuration: configuration.fingerprint().to_string(),
            helper,
            workspace: workspace.to_path_buf(),
        };
        if let Some(launch) = self.lock().get(&key) {
            return Ok(launch.clone());
        }
        let launch = execution::probe(
            configuration,
            key.helper.as_deref(),
            workspace,
            self.host.clone(),
        )
        .await?;
        self.lock().insert(key, launch.clone());

        Ok(launch)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ProbeKey, NativeLaunch>> {
        self.available
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(feature = "native-sandbox")]
impl Default for SandboxGate {
    fn default() -> Self {
        Self::new(Sandbox::default())
    }
}

/// What one availability check depends on.
#[cfg(feature = "native-sandbox")]
#[derive(Eq, Hash, PartialEq)]
struct ProbeKey {
    configuration: String,
    helper: Option<PathBuf>,
    workspace: PathBuf,
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;
