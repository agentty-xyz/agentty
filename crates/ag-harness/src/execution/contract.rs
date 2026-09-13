//! Private contracts, not an isolation implementation or a callable tool.
//!
//! Commands, descendants, and repository contents are hostile. The configuring
//! host and other host processes are trusted. A future executor must enforce
//! the complete policy before starting anything, including against path
//! aliases, symlinks, hard links, renames, and races. Lexical validation is not
//! enforcement. Git metadata includes `.git` entries, resolved Git directories,
//! common directories, and linked-worktree administration; no grant permits
//! mutation. Applied workspace writes survive cancellation and failure; there
//! is no rollback. Aggregate memory, process-count, and disk quotas are outside
//! this contract.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// Owned argv without shell parsing, implicit PATH lookup, or inherited state.
pub(super) struct Command {
    arguments: Vec<OsString>,
    directory: PathBuf,
    executable: PathBuf,
}

impl Command {
    pub(super) fn new(
        executable: PathBuf,
        arguments: Vec<OsString>,
        directory: PathBuf,
    ) -> Result<Self, ValidationError> {
        validate_path(&executable, true)?;
        validate_path(&directory, false)?;
        if arguments.iter().any(|argument| contains_nul(argument)) {
            return Err(ValidationError::InvalidArgument);
        }

        Ok(Self {
            arguments,
            directory,
            executable,
        })
    }

    pub(super) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(super) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Relative to the policy workspace, including `.` for its root.
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Unvalidated host input, consumed when the immutable policy is constructed.
#[derive(Default)]
pub(super) struct Grants {
    pub(super) environment: Vec<(OsString, OsString)>,
    pub(super) external_reads: Vec<PathBuf>,
    pub(super) host_information: bool,
    pub(super) network: bool,
    pub(super) workspace_writes: Vec<PathBuf>,
}

/// Default-read-only workspace and explicit capabilities for all descendants.
///
/// No inherited environment, descriptors, external filesystem reads, or host
/// information is implicitly authorized, including executable/runtime
/// resources. An executor must reject any policy it cannot enforce, never
/// weaken it.
pub(super) struct Policy {
    environment: BTreeMap<OsString, OsString>,
    external_reads: Vec<PathBuf>,
    git_metadata: Vec<PathBuf>,
    host_information: bool,
    workspace: PathBuf,
    workspace_writes: Vec<PathBuf>,
}

impl Policy {
    /// The trusted host supplies known Git administration roots. The executor
    /// must additionally discover and protect repository-controlled
    /// indirections before launch, or fail closed if complete protection
    /// cannot be established.
    pub(super) fn new(
        workspace: PathBuf,
        git_metadata: Vec<PathBuf>,
        grants: Grants,
    ) -> Result<Self, ValidationError> {
        if grants.network {
            return Err(ValidationError::UnsupportedNetworking);
        }
        validate_path(&workspace, true)?;
        if has_git_component(&workspace) {
            return Err(ValidationError::InvalidWorkspace);
        }
        if git_metadata.is_empty() {
            return Err(ValidationError::MissingGitMetadata);
        }
        for path in &git_metadata {
            validate_path(path, true)?;
            if workspace.starts_with(path) {
                return Err(ValidationError::InvalidWorkspace);
            }
        }
        for path in &grants.external_reads {
            validate_path(path, true)?;
        }
        for path in &grants.workspace_writes {
            validate_path(path, false)?;
            let target = workspace.join(path);
            if has_git_component(path) || git_metadata.iter().any(|git| target.starts_with(git)) {
                return Err(ValidationError::GitMetadataWrite);
            }
        }
        let mut environment = BTreeMap::new();
        for (name, value) in grants.environment {
            if name.is_empty()
                || contains_nul(&name)
                || name.as_encoded_bytes().contains(&b'=')
                || contains_nul(&value)
                || environment.insert(name, value).is_some()
            {
                return Err(ValidationError::InvalidEnvironment);
            }
        }

        Ok(Self {
            environment,
            external_reads: grants.external_reads,
            git_metadata,
            host_information: grants.host_information,
            workspace,
            workspace_writes: grants.workspace_writes,
        })
    }

    pub(super) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(super) fn git_metadata(&self) -> &[PathBuf] {
        &self.git_metadata
    }

    pub(super) fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }

    pub(super) fn external_reads(&self) -> &[PathBuf] {
        &self.external_reads
    }

    pub(super) fn workspace_writes(&self) -> &[PathBuf] {
        &self.workspace_writes
    }

    pub(super) fn exposes_host_information(&self) -> bool {
        self.host_information
    }

    /// Reports lexical intent only; never use this as a filesystem access
    /// check. Protected metadata can be read only where a read grant
    /// already exists.
    pub(super) fn requested_access(&self, path: &Path) -> Result<Access, ValidationError> {
        validate_path(path, true)?;
        let in_workspace = path.starts_with(&self.workspace);
        if !in_workspace
            && !self
                .external_reads
                .iter()
                .any(|root| path.starts_with(root))
        {
            return Ok(Access::Denied);
        }
        if has_git_component(path) || self.git_metadata.iter().any(|root| path.starts_with(root)) {
            return Ok(Access::ReadOnly);
        }
        if in_workspace
            && self.workspace_writes.iter().any(|root| {
                let root = self.workspace.join(root);

                path.starts_with(root)
            })
        {
            return Ok(Access::ReadWrite);
        }

        Ok(Access::ReadOnly)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Access {
    Denied,
    ReadOnly,
    ReadWrite,
}

/// An absolute monotonic deadline shared by setup, execution, and output drain.
/// Cleanup remains independently available after this deadline expires.
#[derive(Clone, Copy)]
pub(super) struct Limits {
    capture_bytes: usize,
    deadline: Instant,
}

impl Limits {
    /// `now` is supplied by the host clock; a zero capture budget is valid.
    pub(super) fn new(
        now: Instant,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, ValidationError> {
        if timeout.is_zero() {
            return Err(ValidationError::InvalidDeadline);
        }
        let deadline = now
            .checked_add(timeout)
            .ok_or(ValidationError::InvalidDeadline)?;

        Ok(Self {
            capture_bytes,
            deadline,
        })
    }

    pub(super) fn deadline(self) -> Instant {
        self.deadline
    }

    pub(super) fn capture_bytes(self) -> usize {
        self.capture_bytes
    }
}

/// One shared byte budget, spent in arrival order across both binary streams.
/// After exhaustion callers continue draining; discarded bytes mark truncation.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Output {
    remaining: usize,
    stderr: Vec<u8>,
    stdout: Vec<u8>,
    truncated: bool,
}

impl Output {
    pub(super) fn new(limits: Limits) -> Self {
        Self {
            remaining: limits.capture_bytes(),
            stderr: Vec::new(),
            stdout: Vec::new(),
            truncated: false,
        }
    }

    pub(super) fn capture(&mut self, stream: Stream, bytes: &[u8]) {
        let retained = bytes.len().min(self.remaining);
        let destination = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        destination.extend_from_slice(&bytes[..retained]);
        self.remaining -= retained;
        self.truncated |= retained < bytes.len();
    }

    pub(super) fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub(super) fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Copy)]
pub(super) enum Stream {
    Stdout,
    Stderr,
}

/// Main-process observation, independent of descendants and overall
/// termination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MainExit {
    Code(i32),
    Signal(i32),
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Termination {
    Completed,
    Deadline,
    Cancelled,
    Failed,
}

/// Output truncation never overwrites the exit or termination reason. Cleanup
/// failure never erases a successful main exit, and writes are never rolled
/// back.
pub(super) struct ExecutionResult {
    pub(super) cleanup_failure: Option<ExecutionError>,
    pub(super) main_exit: MainExit,
    pub(super) output: Output,
    pub(super) termination: Termination,
}

/// Control is obtained before starting; dropping a start/wait future must not
/// discard the authority needed to stop descendants and clean up. No production
/// implementation exists. Preparation must be inert (no processes or writes).
pub(super) trait Executor: Send + Sync {
    fn prepare(
        &self,
        command: Command,
        policy: Policy,
        limits: Limits,
    ) -> Result<PreparedExecution, ExecutionError>;
}

pub(super) struct PreparedExecution {
    pub(super) control: Box<dyn ExecutionControl>,
    pub(super) execution: Box<dyn Execution>,
}

#[async_trait]
pub(super) trait Execution: Send {
    /// Enforces the policy for the entire process tree or fails before launch.
    /// Start failure still requires cleanup through the separately retained
    /// control. Completion requires descendant quiescence and bounded draining.
    async fn run(self: Box<Self>) -> ExecutionResult;
}

#[async_trait]
pub(super) trait ExecutionControl: Send + Sync {
    /// Idempotent cancellation, including before launch; covers all
    /// descendants.
    fn cancel(&self);

    /// Idempotently stop/reap all descendants and release resources, even after
    /// cancellation, a dropped run future, or deadline expiry. A failed cleanup
    /// can be retried. The owner retains its failure separately in the result.
    async fn cleanup(&self) -> Result<(), ExecutionError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExecutionError {
    Unsupported,
    Setup,
    Cleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ValidationError {
    InvalidPath,
    InvalidArgument,
    InvalidEnvironment,
    InvalidWorkspace,
    MissingGitMetadata,
    GitMetadataWrite,
    UnsupportedNetworking,
    InvalidDeadline,
}

fn validate_path(path: &Path, absolute: bool) -> Result<(), ValidationError> {
    if path.as_os_str().is_empty()
        || contains_nul(path.as_os_str())
        || path.is_absolute() != absolute
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ValidationError::InvalidPath);
    }

    Ok(())
}

fn contains_nul(value: &OsStr) -> bool {
    value.as_encoded_bytes().contains(&0)
}

fn has_git_component(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(b".git")
    })
}

#[cfg(test)]
#[path = "contract_test.rs"]
mod tests;
