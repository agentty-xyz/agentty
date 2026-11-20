//! Shell commands run through a host-selected executor.
//!
//! Bash is the only tool that needs per-turn configuration: add `bash` to
//! the turn's `ToolSet` and attach a [`BashConfig`] with
//! `TurnOptions::with_bash`. `BashConfig::native` and `BashConfig::new`
//! select the native sandbox with the `native-sandbox` feature, subject to
//! the harness's `crate::sandbox::Sandbox` choice where it cannot run;
//! [`BashConfig::for_executor`] selects a host [`BashExecutor`] such as
//! [`UnsandboxedExecutor`], which enforces no isolation. Each executor
//! reports its [`Enforcement`], and the model-facing
//! tool description states it. Command intents are journaled before spawning
//! and surface as [`CommandRecord`]s.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

pub use crate::command_journal::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandRecord, CommandTermination,
};
pub use crate::execution::{
    BashExecutor, BashProcess, Enforcement, ExecutionAccess, ExecutionCommand, ExecutionError,
    ExecutionPolicy, MainExit, OutputStream, ProcessEvent, UnsandboxedExecutor,
};

/// Identity recorded for the default native sandbox executor.
pub(crate) const NATIVE_EXECUTOR: &str = "native";

/// Isolation the default native sandbox enforces on every supported platform
/// unless the configuration grants networking; where it cannot run, the
/// harness's sandbox choice decides instead.
pub(crate) const NATIVE_ENFORCEMENT: Enforcement = Enforcement::FileSystemAndNetwork;

/// Environment variable naming the granted temporary directory.
const TEMPORARY_DIRECTORY_VARIABLE: &str = "TMPDIR";

/// Immutable host capabilities for Bash. No external reads or environment
/// values are inherited. Runtime libraries and executables require read
/// grants: [`Self::with_system_reads`] grants the platform's system
/// directories and [`Self::with_read`] anything else, such as toolchains.
/// Grants and denials state policy; their enforcement is the selected
/// executor's documented scope, and an unenforcing executor such as
/// [`crate::bash::UnsandboxedExecutor`] applies none of them.
/// The default native executor requires a helper of the matching version
/// at a trusted location outside the command workspace: the host executable
/// calling `crate::sandbox::dispatch`, or the `ag-harness-sandbox` binary;
/// [`BashConfig::for_executor`] selects an explicit host executor instead.
#[derive(Clone)]
pub struct BashConfig {
    pub(crate) enforcement: Enforcement,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) executor: Option<Arc<dyn BashExecutor>>,
    pub(crate) snapshot: BashPolicySnapshot,
}

impl BashConfig {
    /// Suggested `capture_bytes`: the combined stdout and stderr bytes one
    /// command result keeps, split between the start and end of each stream.
    pub const DEFAULT_CAPTURE_BYTES: usize = 8 * 1024;
    /// Largest accepted `capture_bytes`, the same size as the bound on one
    /// encoded `read` result. It caps captured output bytes only: a `bash`
    /// result's encoding, with escaping and outcome fields, can exceed it, and
    /// `bash` results are not deferred by the per-round result share, so a
    /// larger capture spends more of the context window on each command.
    pub const MAX_CAPTURE_BYTES: usize = crate::tool::MAX_TOOL_RESULT_BYTES;

    /// Configures the default native sandbox executor with trusted launcher
    /// and Bash executables and a nonsecret policy revision. Change the
    /// revision whenever executable contents or granted environment values
    /// change. Paths must be absolute and outside the workspace; launch
    /// performs filesystem validation. `capture_bytes` bounds the output
    /// each command returns, from 1 to [`Self::MAX_CAPTURE_BYTES`]; use
    /// [`Self::DEFAULT_CAPTURE_BYTES`] unless the host needs another bound.
    ///
    /// Requires the `native-sandbox` feature; without it, select an executor
    /// with [`Self::for_executor`].
    ///
    /// # Errors
    /// Rejects invalid identities, paths, deadlines, and capture bounds.
    #[cfg(feature = "native-sandbox")]
    pub fn new(
        launcher: PathBuf,
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        if !valid_path(&launcher, true) {
            return Err(BashError::InvalidPolicy);
        }

        Self::with_snapshot(
            Some(launcher),
            None,
            NATIVE_EXECUTOR.to_string(),
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    /// Configures the default native sandbox executor as [`Self::new`] does,
    /// with this host executable as the launcher: the host must call
    /// [`crate::sandbox::dispatch`] first in `main`. Without that call, or
    /// when the executable is inside the workspace, the harness's
    /// [`crate::sandbox::Sandbox`] choice decides what runs instead.
    ///
    /// Requires the `native-sandbox` feature.
    ///
    /// # Errors
    /// Rejects invalid paths, revisions, deadlines, and capture bounds.
    #[cfg(feature = "native-sandbox")]
    pub fn native(
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        Self::with_snapshot(
            None,
            None,
            NATIVE_EXECUTOR.to_string(),
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    /// Configures an explicit host-selected executor instead of the default
    /// native sandbox launcher. Selection is always explicit: the harness
    /// never falls back to another executor or consults the environment. The
    /// executor identity is recorded in durable policy snapshots and
    /// host-request fingerprints; hosts must supply a new identity or
    /// revision when executor behavior changes. `capture_bytes` is bounded as
    /// for [`Self::new`].
    ///
    /// # Errors
    /// Rejects invalid executor identities, paths, deadlines, and capture
    /// bounds.
    pub fn for_executor(
        executor: Arc<dyn BashExecutor>,
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        let identity = executor.identity().to_string();
        if identity.trim().is_empty()
            || identity.len() > 256
            || identity.contains('\0')
            || identity == NATIVE_EXECUTOR
        {
            return Err(BashError::InvalidPolicy);
        }

        Self::with_snapshot(
            None,
            Some(executor),
            identity,
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    /// Validates the shared policy fields; `executor` is `None` only for the
    /// native sandbox, whose `launcher` is `None` when the dispatching host
    /// executable launches it.
    fn with_snapshot(
        launcher: Option<PathBuf>,
        executor: Option<Arc<dyn BashExecutor>>,
        identity: String,
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        if !valid_path(&bash, true)
            || revision.trim().is_empty()
            || revision.len() > 256
            || timeout.is_zero()
            || timeout > Duration::from_secs(3600)
            || capture_bytes == 0
            || capture_bytes > Self::MAX_CAPTURE_BYTES
        {
            return Err(BashError::InvalidPolicy);
        }
        let enforcement = executor
            .as_ref()
            .map_or(NATIVE_ENFORCEMENT, |executor| executor.enforcement());

        Ok(Self {
            enforcement,
            environment: BTreeMap::new(),
            executor,
            snapshot: BashPolicySnapshot {
                bash,
                capabilities: BTreeSet::new(),
                capture_bytes,
                environment_names: Vec::new(),
                executor: identity,
                external_reads: Vec::new(),
                host_information: false,
                launcher,
                linux_bubblewrap: None,
                revision,
                temporary_directory: None,
                timeout,
                workspace_writes: Vec::new(),
            },
        })
    }

    /// Grants recursive external read access, including runtime resources.
    /// The native sandbox grants the directory by path, without inspecting
    /// its contents, so a grant as large as a toolchain costs nothing per
    /// command; links inside it resolve to their targets, which stay subject
    /// to the policy. The path must exist, and it must neither contain the
    /// workspace nor resolve inside it.
    ///
    /// # Errors
    /// Rejects relative paths. Native preparation checks actual filesystem
    /// state.
    pub fn with_read(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true) {
            return Err(BashError::InvalidPolicy);
        }
        if self.snapshot.external_reads.len() >= 64 {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.external_reads.push(path);

        Ok(self)
    }

    /// Grants read access to the platform's system directories, which Bash,
    /// Git, and developer toolchains need to start: on macOS `/bin`, `/sbin`,
    /// `/usr`, `/System`, `/Library/Developer`, the selected Xcode and its
    /// license record,
    /// `/private/etc`, and the shell and time zone selections under
    /// `/private/var`; on Linux `/bin`, `/sbin`, `/usr`, `/lib*`, and `/etc`,
    /// with a private `/proc` and a minimal `/dev`. Paths missing on the host
    /// are skipped. Toolchains outside them, such as `~/.rustup` or
    /// `/opt/homebrew`, still need [`Self::with_read`].
    #[must_use]
    pub fn with_system_reads(mut self) -> Self {
        self.snapshot.capabilities.insert(Capability::SystemReads);

        self
    }

    /// Grants writes beneath an existing workspace-relative directory. Git
    /// metadata existing at launch remains protected unless
    /// [`Self::with_git_writes`] grants it; a repository the command itself
    /// creates inside a grant is the command's own output. Links inside the
    /// grant resolve to their targets, which stay subject to the policy:
    /// one that leaves the granted directories cannot be written through.
    /// Hard links that already exist inside the grant are files of the
    /// grant, and commands cannot create new ones to files outside it.
    /// Writes are never rolled back. Native Linux enforcement adds Landlock
    /// rules inside the launcher and fails closed before execution on kernels
    /// without the required ABI (Linux 6.2); macOS additionally denies
    /// metadata by name pattern, including names created after launch.
    ///
    /// # Errors
    /// Rejects absolute paths, traversal, and Git metadata components.
    pub fn with_write(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, false)
            || path.components().any(|part| {
                matches!(part, std::path::Component::ParentDir)
                    || part
                        .as_os_str()
                        .as_encoded_bytes()
                        .eq_ignore_ascii_case(b".git")
            })
        {
            return Err(BashError::InvalidPolicy);
        }
        if self.snapshot.workspace_writes.len() >= 64 {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.workspace_writes.push(path);

        Ok(self)
    }

    /// Grants reads and writes beneath an existing absolute directory outside
    /// the workspace and exports it to commands as `TMPDIR`. Choose one
    /// directory per session, such as one the host creates under the system
    /// temporary directory, so commands never share temporary files with
    /// other sessions. It must not overlap the workspace or its Git metadata.
    /// The path is part of the policy fingerprint that identifies host
    /// requests and command records, so derive it from a stable session
    /// identity rather than choosing a fresh random name at each start.
    ///
    /// # Errors
    /// Rejects relative paths, a second temporary directory, and a
    /// configuration that already grants a `TMPDIR` value.
    pub fn with_temporary_directory(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true)
            || self.snapshot.temporary_directory.is_some()
            || self.environment.contains_key(TEMPORARY_DIRECTORY_VARIABLE)
        {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.temporary_directory = Some(path);

        Ok(self)
    }

    /// Grants writes to the workspace repository's Git directory and, for a
    /// linked worktree, its common directory, so `git add` and `git commit`
    /// work. Configuration, hooks, worktree links, submodule metadata, and
    /// rebase and cherry-pick state stay read-only, because Git run later
    /// outside the sandbox would otherwise execute or follow what a command
    /// wrote there. Commands can still move any reference, including other
    /// branches. On Linux only a linked worktree can stage and commit:
    /// Landlock cannot keep a main worktree's Git directory from gaining a
    /// `commondir` link, so there only its object, reference, and log stores
    /// are writable, and commands are refused while a write grant contains a
    /// Git directory, such as `with_write(".")` in a main worktree. A linked
    /// worktree whose repository enables `extensions.worktreeConfig` needs
    /// its `config.worktree` to exist.
    ///
    /// An executor other than the native sandbox that cannot keep these
    /// entries read-only must refuse a policy granting Git writes.
    #[must_use]
    pub fn with_git_writes(mut self) -> Self {
        self.snapshot.capabilities.insert(Capability::GitWrites);

        self
    }

    /// Grants one environment value. The policy revision identifies its value
    /// for durable recovery; snapshots retain names and revision, never values.
    /// Hosts must change the revision when a value changes.
    ///
    /// # Errors
    /// Rejects empty, duplicate, NUL-containing, or invalid variable names,
    /// and `TMPDIR` once [`Self::with_temporary_directory`] set it.
    pub fn with_environment(mut self, name: String, value: String) -> Result<Self, BashError> {
        if (name == TEMPORARY_DIRECTORY_VARIABLE && self.snapshot.temporary_directory.is_some())
            || name.is_empty()
            || name.contains(['=', '\0'])
            || value.contains('\0')
            || name.len() > 256
            || self.environment.len() >= 64
            || self
                .environment
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>()
                .saturating_add(name.len())
                .saturating_add(value.len())
                > 65536
            || self.environment.contains_key(&name)
        {
            return Err(BashError::InvalidPolicy);
        }
        self.environment.insert(name, value);
        self.snapshot.environment_names = self.environment.keys().cloned().collect();

        Ok(self)
    }

    /// Explicitly grants host-information exposure. Required by every
    /// executor; commands cannot conceal all host details. On macOS the
    /// native executor additionally grants filesystem metadata and
    /// root-directory enumeration needed by the qualified Bash runtime. File
    /// contents still require separate read grants.
    #[must_use]
    pub fn with_host_information(mut self) -> Self {
        self.snapshot.host_information = true;

        self
    }

    /// Selects a trusted native Linux Bubblewrap executable in place of the
    /// default `/usr/bin/bwrap`. No PATH lookup is used.
    ///
    /// Requires the `native-sandbox` feature.
    ///
    /// # Errors
    /// Rejects relative paths; preparation validates the executable.
    #[cfg(feature = "native-sandbox")]
    pub fn with_linux_bubblewrap(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true) {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.linux_bubblewrap = Some(path);

        Ok(self)
    }

    /// Grants network access, which the policy denies by default. The native
    /// sandbox then allows IP connections, listeners, and name resolution:
    /// on macOS through Seatbelt network rules that still deny Unix-domain
    /// sockets other than the system resolver's; on Linux by sharing the
    /// host network namespace while seccomp still refuses every socket
    /// family but IPv4 and IPv6. An executor's [`Enforcement`] describes the
    /// isolation it applies to policies without this grant; with it, the
    /// configuration reports at most [`Enforcement::FileSystem`], so the
    /// model-facing description states that network access is not
    /// restricted. A host executor that cannot open the network it otherwise
    /// denies must refuse a policy granting it.
    #[must_use]
    pub fn with_network(mut self) -> Self {
        self.snapshot.capabilities.insert(Capability::Network);
        if self.enforcement == Enforcement::FileSystemAndNetwork {
            self.enforcement = Enforcement::FileSystem;
        }

        self
    }

    /// Returns the isolation the selected executor reported when this
    /// configuration was built, which the model-facing `bash` tool
    /// description states. Reading it once keeps every advertisement built
    /// from one configuration identical.
    pub(crate) fn enforcement(&self) -> Enforcement {
        self.enforcement
    }

    pub(crate) fn fingerprint(&self) -> Value {
        json!(self.snapshot)
    }

    /// Returns this native configuration launched through the checked
    /// `launch` executables. Host-request and turn-option fingerprints come
    /// from the configuration the host gave; each command intent records
    /// this effective one, naming the executables that ran it.
    #[cfg(feature = "native-sandbox")]
    pub(crate) fn launched_through(&self, launch: crate::execution::NativeLaunch) -> Self {
        let mut configuration = self.clone();
        configuration.snapshot.launcher = Some(launch.launcher);
        configuration.snapshot.linux_bubblewrap = launch.bubblewrap;

        configuration
    }

    /// Returns this native configuration with its commands run without
    /// isolation, which the model-facing `bash` description states and each
    /// command intent records through the unsandboxed executor's identity.
    #[cfg(feature = "native-sandbox")]
    pub(crate) fn unsandboxed(&self) -> Self {
        let executor = UnsandboxedExecutor::without_isolation();
        let mut configuration = self.clone();
        configuration.snapshot.executor = executor.identity().to_string();
        configuration.snapshot.launcher = None;
        configuration.snapshot.linux_bubblewrap = None;
        configuration.enforcement = executor.enforcement();
        configuration.executor = Some(Arc::new(executor));

        configuration
    }
}

impl fmt::Debug for BashConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BashConfig")
            .field("policy", &self.snapshot)
            .finish_non_exhaustive()
    }
}

/// Executor instances compare through their recorded snapshot identity, so
/// equal policies with separately constructed equivalent executors are equal.
impl PartialEq for BashConfig {
    fn eq(&self, other: &Self) -> bool {
        self.enforcement == other.enforcement
            && self.environment == other.environment
            && self.snapshot == other.snapshot
    }
}

impl Eq for BashConfig {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BashPolicySnapshot {
    pub(crate) bash: PathBuf,
    /// Capabilities granted beyond paths and environment values. Grants
    /// added after the first snapshots are skipped on write while unset,
    /// so recorded fingerprints of earlier policies stay stable.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) capabilities: BTreeSet<Capability>,
    pub(crate) capture_bytes: usize,
    pub(crate) environment_names: Vec<String>,
    /// Selected executor identity. Legacy snapshots predate executor
    /// selection and decode as the native default; the native identity is
    /// skipped on write so their recorded fingerprints stay stable.
    #[serde(
        default = "native_executor_identity",
        skip_serializing_if = "is_native_executor"
    )]
    pub(crate) executor: String,
    pub(crate) external_reads: Vec<PathBuf>,
    pub(crate) host_information: bool,
    pub(crate) launcher: Option<PathBuf>,
    pub(crate) linux_bubblewrap: Option<PathBuf>,
    pub(crate) revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) temporary_directory: Option<PathBuf>,
    pub(crate) timeout: Duration,
    pub(crate) workspace_writes: Vec<PathBuf>,
}

impl BashPolicySnapshot {
    /// Returns whether the configuration grants `capability`.
    pub(crate) fn grants(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// A capability a configuration grants beyond paths and environment values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Capability {
    /// [`BashConfig::with_git_writes`].
    GitWrites,
    /// [`BashConfig::with_network`].
    Network,
    /// [`BashConfig::with_system_reads`].
    SystemReads,
}

fn native_executor_identity() -> String {
    NATIVE_EXECUTOR.to_string()
}

fn is_native_executor(identity: &str) -> bool {
    identity == NATIVE_EXECUTOR
}

/// Validated shell source. Working directory, executable, grants, deadline,
/// and output budget are selected exclusively by the host.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "RawArguments")]
pub struct BashArguments {
    command: String,
}

impl BashArguments {
    /// Accepts 1–65536 bytes of nonempty, NUL-free shell source.
    ///
    /// # Errors
    /// Returns an error for invalid or oversized source before spawning.
    pub fn new(command: String) -> Result<Self, BashError> {
        if command.trim().is_empty() || command.len() > 65536 || command.contains('\0') {
            return Err(BashError::InvalidArguments);
        }

        Ok(Self { command })
    }

    /// Returns the validated shell source. Do not include it in telemetry.
    pub fn command(&self) -> &str {
        &self.command
    }
}

impl fmt::Debug for BashArguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BashArguments")
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArguments {
    command: String,
}

impl TryFrom<RawArguments> for BashArguments {
    type Error = BashError;

    fn try_from(value: RawArguments) -> Result<Self, Self::Error> {
        Self::new(value.command)
    }
}

/// Shell execution or policy failure without command content or secrets.
#[derive(Clone, Copy, Debug, Deserialize, Error, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BashError {
    /// Invalid host policy.
    #[error("invalid Bash sandbox policy")]
    InvalidPolicy,
    /// Invalid shell source.
    #[error("invalid Bash arguments")]
    InvalidArguments,
    /// The selected executor cannot execute the requested policy or platform.
    #[error("Bash executor unavailable for this policy or platform")]
    Unavailable,
    /// Preparation, spawning, or supervision failed.
    #[error("sandboxed Bash execution failed")]
    Execution,
    /// Cleanup remains unconfirmed and requires owner-scoped reconciliation.
    #[error("sandboxed Bash cleanup remains unresolved")]
    Cleanup,
}

#[cfg(test)]
#[path = "bash_test.rs"]
mod tests;

fn valid_path(path: &Path, absolute: bool) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();

    !bytes.is_empty()
        && bytes.len() <= 4096
        && !bytes.contains(&0)
        && path.is_absolute() == absolute
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}
