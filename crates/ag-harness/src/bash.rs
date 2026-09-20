//! Explicit host policy and bounded results for sandboxed Bash commands.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

/// Immutable host capabilities for Bash. No external reads or environment
/// values are inherited. Runtime libraries and executables require read grants.
/// The launcher must be the matching `ag-harness-sandbox` binary at a trusted
/// location outside the command workspace.
#[derive(Clone, Eq, PartialEq)]
pub struct BashConfig {
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) snapshot: BashPolicySnapshot,
}

impl BashConfig {
    /// Configures trusted launcher and Bash executables and a nonsecret policy
    /// revision. Change the revision whenever executable contents or granted
    /// environment values change. Paths must be absolute and outside the
    /// workspace; launch performs filesystem validation.
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
        if !valid_path(&launcher, true)
            || !valid_path(&bash, true)
            || revision.trim().is_empty()
            || revision.len() > 256
            || timeout.is_zero()
            || timeout > Duration::from_secs(3600)
            || capture_bytes == 0
            || capture_bytes > 8192
        {
            return Err(BashError::InvalidPolicy);
        }

        Ok(Self {
            environment: BTreeMap::new(),
            snapshot: BashPolicySnapshot {
                bash,
                capture_bytes,
                environment_names: Vec::new(),
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
    /// metadata remains protected. Writes are never rolled back. Native Linux
    /// execution currently rejects write grants before launch, because its
    /// static mounts cannot protect Git metadata created later beneath a
    /// writable directory.
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

    /// Explicitly grants native host-information exposure. Required by both
    /// backends; native execution cannot conceal all host details. On macOS
    /// this also grants filesystem metadata and root-directory enumeration
    /// needed by the qualified Bash runtime. File contents still require
    /// separate read grants.
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

    /// Networking is deny-only on every backend.
    ///
    /// # Errors
    /// Always returns unsupported; this does not modify the configuration.
    pub fn with_network(self) -> Result<Self, BashError> {
        Err(BashError::Unavailable)
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BashPolicySnapshot {
    pub(crate) bash: PathBuf,
    pub(crate) capture_bytes: usize,
    pub(crate) environment_names: Vec<String>,
    pub(crate) external_reads: Vec<PathBuf>,
    pub(crate) host_information: bool,
    pub(crate) launcher: PathBuf,
    pub(crate) linux_bubblewrap: Option<PathBuf>,
    pub(crate) revision: String,
    pub(crate) timeout: Duration,
    pub(crate) workspace_writes: Vec<PathBuf>,
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
    /// Native isolation cannot enforce the requested policy.
    #[error("native Bash sandbox unavailable for this policy or platform")]
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
