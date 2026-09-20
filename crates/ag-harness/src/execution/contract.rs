//! Command contracts shared by the harness supervisor and Bash executors.
//!
//! Commands, descendants, and repository contents are hostile. The configuring
//! host and other host processes are trusted. The harness retains policy
//! selection, the separate tool permission, command-journal persistence,
//! deadlines, the combined output budget, and the no-replay rule; a selected
//! executor owns process launch, its documented enforcement, output capture,
//! and cleanup. Enforcing executors apply the complete policy before starting
//! anything, including against path aliases, symlinks, hard links, renames,
//! and races, and reject any policy they cannot enforce. Lexical validation is
//! not enforcement. Git metadata includes `.git` entries, resolved Git
//! directories, common directories, and linked-worktree administration; no
//! grant permits mutation. Applied workspace writes survive cancellation and
//! failure; there is no rollback. Aggregate memory, process-count, and disk
//! quotas are outside this contract.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use thiserror::Error;

use crate::command_journal::CommandCleanupScope;

/// Host-selected Bash execution boundary consumed through `BashConfig`.
///
/// The harness validates policy values, persists command intent before any
/// spawn, supervises the original deadline and the combined output budget,
/// drains excess output, and retains cleanup after caller drop. An executor
/// owns process launch, the isolation its documentation states, bounded
/// output production, and idempotent cleanup. Selection is always explicit:
/// there is no fallback or environment-based executor choice.
pub trait BashExecutor: Send + Sync {
    /// Stable executor identity recorded in durable policy snapshots and
    /// host-request fingerprints. Changing enforcement behavior requires a
    /// new identity or a host policy-revision change.
    fn identity(&self) -> &str;

    /// Cleanup scope this executor acknowledges, carried on every recorded
    /// command outcome.
    fn cleanup_scope(&self) -> CommandCleanupScope;

    /// Inert, synchronous allocation of one command's cleanup owner. No
    /// processes, writes, blocking work, or background preparation may occur
    /// here.
    ///
    /// # Errors
    /// Returns an error when this executor cannot execute on the current
    /// platform or configuration.
    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError>;
}

/// One bound command execution driven by the harness supervisor.
///
/// All acquired resources must be recorded in `self` before an operation can
/// yield. Dropping any operation must leave cleanup authority in `self`, with
/// no detached acquisition still able to create resources afterward. Methods
/// must yield promptly and never block the runtime thread.
#[async_trait]
pub trait BashProcess: Send {
    /// Establish the executor's documented enforcement before `start`, or
    /// fail closed. Preparation must not run the command.
    ///
    /// # Errors
    /// Returns an error when the policy cannot be honored as documented.
    async fn prepare(
        &mut self,
        command: &ExecutionCommand,
        policy: &ExecutionPolicy,
    ) -> Result<(), ExecutionError>;

    /// Launch the prepared command.
    ///
    /// # Errors
    /// Returns an error when the process cannot be spawned. Start failure
    /// still requires retained cleanup.
    async fn start(&mut self) -> Result<(), ExecutionError>;

    /// Read into the supplied bounded buffer, without an unbounded internal
    /// queue. Output lengths cannot exceed the buffer. EOF is per pipe;
    /// completion independently acknowledges the executor's cleanup scope.
    /// Deliver the main exit separately, even if unavailable.
    ///
    /// # Errors
    /// Returns an error when supervision of the running command fails.
    async fn next_event(&mut self, buffer: &mut [u8]) -> Result<ProcessEvent, ExecutionError>;

    /// Idempotently clean up within the documented cleanup scope and release
    /// even partially prepared resources. A best-effort scope cannot confirm
    /// escaped descendants; a dropped attempt must leave the owner usable for
    /// another bounded attempt. This operation owns any remaining pipe
    /// draining/disposal and cannot depend on a consumer.
    ///
    /// # Errors
    /// Returns an error when the cleanup scope could not be confirmed.
    async fn cleanup(&mut self) -> Result<(), ExecutionError>;
}

/// One supervision observation delivered by [`BashProcess::next_event`].
#[derive(Clone, Copy, Debug)]
pub enum ProcessEvent {
    /// Internal progress without output, exit, or scope acknowledgment.
    Progress,
    /// Captured bytes written into the supplied buffer prefix.
    Output(OutputStream, usize),
    /// The named pipe reached end of file.
    Eof(OutputStream),
    /// The main process exited; descendants may still run.
    MainExit(MainExit),
    /// The executor acknowledged its documented cleanup scope.
    Quiescent,
}

/// Owned argv without shell parsing, implicit PATH lookup, or inherited state.
pub struct ExecutionCommand {
    arguments: Vec<OsString>,
    directory: PathBuf,
    executable: PathBuf,
}

impl ExecutionCommand {
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

    /// Absolute path of the executable to launch.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Arguments passed verbatim, without shell parsing.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Relative to the policy workspace, including `.` for its root.
    pub fn directory(&self) -> &Path {
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
/// resources. An enforcing executor must reject any policy it cannot enforce,
/// never weaken it; an executor documented as unenforcing applies only the
/// launch configuration.
pub struct ExecutionPolicy {
    environment: BTreeMap<OsString, OsString>,
    external_reads: Vec<PathBuf>,
    git_metadata: Vec<PathBuf>,
    host_information: bool,
    workspace: PathBuf,
    workspace_writes: Vec<PathBuf>,
}

impl ExecutionPolicy {
    /// The trusted host supplies known Git administration roots. An enforcing
    /// executor must additionally discover and protect repository-controlled
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

    /// Absolute command workspace root; reads default to this tree.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Known Git administration roots that no grant permits mutating.
    pub fn git_metadata(&self) -> &[PathBuf] {
        &self.git_metadata
    }

    /// Complete environment for the command; nothing else is inherited.
    pub fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }

    /// Absolute external roots granted recursive read access.
    pub fn external_reads(&self) -> &[PathBuf] {
        &self.external_reads
    }

    /// Workspace-relative directories granted recursive write access.
    pub fn workspace_writes(&self) -> &[PathBuf] {
        &self.workspace_writes
    }

    /// Whether the host explicitly granted native host-information exposure.
    pub fn exposes_host_information(&self) -> bool {
        self.host_information
    }

    /// Reports lexical intent only; never use this as a filesystem access
    /// check. Protected metadata can be read only where a read grant
    /// already exists. Relative, empty, traversing, and NUL-containing paths
    /// report [`ExecutionAccess::Denied`].
    pub fn requested_access(&self, path: &Path) -> ExecutionAccess {
        if validate_path(path, true).is_err() {
            return ExecutionAccess::Denied;
        }
        let in_workspace = path.starts_with(&self.workspace);
        if !in_workspace
            && !self
                .external_reads
                .iter()
                .any(|root| path.starts_with(root))
        {
            return ExecutionAccess::Denied;
        }
        if has_git_component(path) || self.git_metadata.iter().any(|root| path.starts_with(root)) {
            return ExecutionAccess::ReadOnly;
        }
        if in_workspace
            && self.workspace_writes.iter().any(|root| {
                let root = self.workspace.join(root);

                path.starts_with(root)
            })
        {
            return ExecutionAccess::ReadWrite;
        }

        ExecutionAccess::ReadOnly
    }
}

/// Lexical policy intent for one absolute path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionAccess {
    /// The path lies outside every granted root.
    Denied,
    /// The path is readable but no grant permits writing it.
    ReadOnly,
    /// The path lies beneath a workspace write grant.
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

    pub(super) fn capture(&mut self, stream: OutputStream, bytes: &[u8]) {
        let retained = bytes.len().min(self.remaining);
        let destination = match stream {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
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

/// One captured binary stream of the main process and its pipe inheritors.
#[derive(Clone, Copy, Debug)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Main-process observation, independent of descendants and overall
/// termination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainExit {
    /// The main process exited with this status code.
    Code(i32),
    /// The main process was terminated by this signal.
    Signal(i32),
    /// No main exit observation is available.
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
/// failure never erases the execution error or main exit, and writes are never
/// rolled back.
pub(super) struct ExecutionResult {
    pub(super) cleanup_failure: Option<ExecutionError>,
    pub(super) execution_failure: Option<ExecutionError>,
    pub(super) main_exit: MainExit,
    pub(super) output: Output,
    pub(super) termination: Termination,
}

/// Control is obtained before starting; dropping a start/wait future must not
/// discard the cleanup authority. Preparation is inert (no processes or
/// writes).
pub(super) trait Executor: Send + Sync {
    fn prepare(
        &self,
        command: ExecutionCommand,
        policy: ExecutionPolicy,
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
    /// Start failure still requires retained cleanup. Completion uses the
    /// selected executor's documented cleanup scope.
    async fn run(self: Box<Self>) -> ExecutionResult;
}

#[async_trait]
pub(crate) trait ExecutionControl: Send + Sync {
    /// Idempotent cancellation, including before launch. Best-effort cleanup
    /// scopes do not establish that escaped descendants stopped.
    fn cancel(&self);

    /// Idempotently settle the executor cleanup scope and release resources
    /// after cancellation, a dropped run future, or deadline expiry. A
    /// failed cleanup can be retried. The owner retains its failure
    /// separately in the result.
    async fn cleanup(&self) -> Result<(), ExecutionError>;
}

/// Content-free executor failure classification.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExecutionError {
    /// The executor cannot execute this policy, platform, or configuration.
    #[error("execution unsupported for this policy or platform")]
    Unsupported,
    /// Preparation failed before the command could run.
    #[error("execution setup failed")]
    Setup,
    /// Launching or observing the process failed.
    #[error("process execution failed")]
    Process,
    /// Harness-side supervision failed.
    #[error("execution supervision failed")]
    Supervision,
    /// Cleanup failed within its documented scope.
    #[error("execution cleanup failed")]
    Cleanup,
    /// Cleanup exhausted its bounds without confirming resource release.
    #[error("execution cleanup remains unconfirmed")]
    CleanupUnconfirmed,
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
