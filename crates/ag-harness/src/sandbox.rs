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
//! - install the standalone helper with `cargo install ag-harness --features
//!   native-sandbox --bin ag-harness-sandbox` and pass its path to
//!   `BashConfig::new`.
//!
//! [`Sandbox`], set with `HarnessBuilder::sandbox`, decides what happens on
//! a host where the sandbox cannot run, for example without `bwrap` or with
//! unprivileged user namespaces disabled. The harness checks once per
//! configuration, at the first run that offers `bash`. By default, commands
//! then run without isolation and every such run reports a
//! [`crate::RunEventKind::SandboxUnavailable`] warning naming what is missing
//! and how to fix it. [`IfUnavailable::Refuse`] fails those runs with
//! [`crate::ErrorKind::SandboxUnavailable`] instead.

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
use crate::bash::BashConfig;
#[cfg(feature = "native-sandbox")]
use crate::execution::{self, Host, NativeLaunch};
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
    /// Run commands without isolation and report a
    /// [`crate::RunEventKind::SandboxUnavailable`] warning in every run
    /// that offers `bash`.
    #[default]
    Unsandboxed,
    /// Fail every run that offers `bash` with
    /// [`crate::ErrorKind::SandboxUnavailable`] before its first model
    /// request.
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
        }
    }

    /// Returns a one-sentence fix a person can apply.
    pub fn remedy(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => {
                "Run on Linux or macOS, or select an isolating executor with \
                 `BashConfig::for_executor`."
            }
            Self::HelperMissing => {
                "Call `ag_harness::sandbox::dispatch()` first in `main`, or install \
                 `ag-harness-sandbox` and pass its path to `BashConfig::new`."
            }
            Self::HelperUntrusted => {
                "Install the helper as an executable outside the workspace without set-ID bits."
            }
            Self::BubblewrapMissing => {
                "Install bubblewrap, for example `apt install bubblewrap` or `dnf install \
                 bubblewrap`, or pass its path with `BashConfig::with_linux_bubblewrap`."
            }
            Self::UserNamespacesUnavailable => {
                "Allow unprivileged user namespaces for `bwrap`, for example with the \
                 distribution's AppArmor profile for it or `sysctl \
                 kernel.unprivileged_userns_clone=1`."
            }
            Self::LandlockUnavailable => {
                "Upgrade to Linux 6.2 or later with Landlock enabled, or drop the write grants."
            }
            Self::SeatbeltMissing => "Restore `/usr/bin/sandbox-exec`, which macOS ships.",
        }
    }
}

impl fmt::Display for SandboxUnavailableReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Warning that a run's Bash commands execute without the native sandbox,
/// with what is missing and how to fix it.
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

/// This executable, once [`dispatch`] returned in it.
#[cfg(feature = "native-sandbox")]
static DISPATCHED_HELPER: OnceLock<PathBuf> = OnceLock::new();

/// A harness's [`Sandbox`] choice with the availability it checked, shared
/// by its clones.
#[cfg(feature = "native-sandbox")]
#[derive(Clone, Default)]
pub(crate) struct SandboxGate {
    /// Checked outcomes by configuration fingerprint, workspace, and helper.
    checked: Arc<Mutex<HashMap<ProbeKey, Result<NativeLaunch, SandboxUnavailableReason>>>>,
    sandbox: Sandbox,
}

#[cfg(feature = "native-sandbox")]
impl SandboxGate {
    /// Returns a gate applying `sandbox`.
    pub(crate) fn new(sandbox: Sandbox) -> Self {
        Self {
            checked: Arc::default(),
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
            (Ok(launch), _) => Ok((configuration.launched_through(launch), None)),
            (Err(reason), IfUnavailable::Unsandboxed) => Ok((
                configuration.unsandboxed(),
                Some(SandboxWarning::new(reason)),
            )),
            (Err(reason), IfUnavailable::Refuse) => Err(TurnError::SandboxUnavailable(reason)),
        }
    }

    /// Returns the availability of the native sandbox for `configuration`
    /// in `workspace` through `helper` on this host, checking it only the
    /// first time.
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
        if let Some(checked) = self.lock().get(&key) {
            return checked.clone();
        }
        let checked = execution::probe(
            configuration,
            key.helper.as_deref(),
            workspace,
            Host::current(),
        )
        .await;
        self.lock().insert(key, checked.clone());

        checked
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ProbeKey, Result<NativeLaunch, SandboxUnavailableReason>>>
    {
        self.checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
