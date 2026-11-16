//! Shell commands run through a host-selected executor.
//!
//! Bash is the only tool that needs per-turn configuration: enable
//! `Tool::Bash` in the policy and attach a [`BashConfig`] with
//! `TurnOptions::with_bash`. [`BashConfig::new`] selects the native sandbox;
//! [`BashConfig::for_executor`] selects a host [`BashExecutor`] such as
//! [`UnsandboxedExecutor`], which enforces no isolation. Each executor reports
//! its [`Enforcement`], and the model-facing tool description states it.
//! Command intents are journaled before spawning and surface as
//! [`CommandRecord`]s.

use std::collections::BTreeMap;
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

/// Isolation the default native sandbox enforces on every supported platform;
/// unsupported platforms and kernels fail closed instead.
pub(crate) const NATIVE_ENFORCEMENT: Enforcement = Enforcement::FileSystemAndNetwork;

/// Immutable host capabilities for Bash. No external reads or environment
/// values are inherited. Runtime libraries and executables require read grants.
/// Grants and denials state policy; their enforcement is the selected
/// executor's documented scope, and an unenforcing executor such as
/// [`crate::bash::UnsandboxedExecutor`] applies none of them.
/// The default native executor requires the matching `ag-harness-sandbox`
/// launcher at a trusted location outside the command workspace;
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
    /// # Errors
    /// Rejects invalid identities, paths, deadlines, and capture bounds.
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

        Self::with_snapshot(Some(launcher), None, bash, revision, timeout, capture_bytes)
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
            Some((executor, identity)),
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    fn with_snapshot(
        launcher: Option<PathBuf>,
        executor: Option<(Arc<dyn BashExecutor>, String)>,
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
        let (executor, identity) = match executor {
            Some((executor, identity)) => (Some(executor), identity),
            None => (None, NATIVE_EXECUTOR.to_string()),
        };
        let enforcement = executor
            .as_ref()
            .map_or(NATIVE_ENFORCEMENT, |executor| executor.enforcement());

        Ok(Self {
            enforcement,
            environment: BTreeMap::new(),
            executor,
            snapshot: BashPolicySnapshot {
                bash,
                capture_bytes,
                environment_names: Vec::new(),
                executor: identity,
                external_reads: Vec::new(),
                host_information: false,
                launcher,
                linux_bubblewrap: None,
                revision,
                timeout,
                workspace_writes: Vec::new(),
            },
        })
    }

    /// Grants recursive external read access, including runtime resources.
    /// Workspace-overlapping aliases, devices, and IPC nodes are rejected.
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

    /// Grants writes beneath an existing workspace-relative directory. Git
    /// metadata existing at launch remains protected; a repository the
    /// command itself creates inside a grant is the command's own output.
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

    /// Grants one environment value. The policy revision identifies its value
    /// for durable recovery; snapshots retain names and revision, never values.
    /// Hosts must change the revision when a value changes.
    ///
    /// # Errors
    /// Rejects empty, duplicate, NUL-containing, or invalid variable names.
    pub fn with_environment(mut self, name: String, value: String) -> Result<Self, BashError> {
        if name.is_empty()
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

    /// Selects a trusted native Linux Bubblewrap executable. No PATH lookup or
    /// unsandboxed fallback is used.
    ///
    /// # Errors
    /// Rejects relative paths; preparation validates the executable.
    pub fn with_linux_bubblewrap(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true) {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.linux_bubblewrap = Some(path);

        Ok(self)
    }

    /// Networking cannot be granted: the policy always denies it. Enforcing
    /// the denial is the selected executor's documented scope; an unenforcing
    /// executor such as [`crate::bash::UnsandboxedExecutor`] applies no network
    /// boundary of its own.
    ///
    /// # Errors
    /// Always returns unsupported; this does not modify the configuration.
    pub fn with_network(self) -> Result<Self, BashError> {
        Err(BashError::Unavailable)
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
    pub(crate) timeout: Duration,
    pub(crate) workspace_writes: Vec<PathBuf>,
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
