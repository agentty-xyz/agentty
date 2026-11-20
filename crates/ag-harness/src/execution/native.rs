//! Native resource ownership beneath the shared, cancellation-safe supervisor.

use std::fs::Metadata;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStderr, ChildStdout};

use super::contract::{
    BashExecutor, BashProcess, Enforcement, ExecutionAccess, ExecutionCommand, ExecutionError,
    ExecutionPolicy, MainExit, ProcessEvent,
};
use super::output;
use super::wire::{GitWrites, Launch, Notice};
use crate::bash::{BashConfig, Capability};
use crate::command_journal::CommandCleanupScope;
use crate::sandbox::SandboxUnavailableReason;

pub(super) struct Native {
    pub(super) configuration: BashConfig,
}

impl Native {
    fn bind_for_platform(&self, platform: &str) -> Result<Box<dyn BashProcess>, ExecutionError> {
        if !matches!(platform, "linux" | "macos") {
            return Err(ExecutionError::Unsupported);
        }

        Ok(Box::new(NativeProcess {
            child: None,
            configuration: self.configuration.clone(),
            encoded: Vec::new(),
            finished: false,
            input: None,
            notice: Vec::new(),
            stderr: None,
            stdout: None,
        }))
    }
}

impl BashExecutor for Native {
    fn identity(&self) -> &str {
        crate::bash::NATIVE_EXECUTOR
    }

    fn enforcement(&self) -> Enforcement {
        crate::bash::NATIVE_ENFORCEMENT
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        #[cfg(target_os = "linux")]
        let scope = CommandCleanupScope::PidNamespace;
        #[cfg(not(target_os = "linux"))]
        let scope = CommandCleanupScope::ProcessGroupBestEffort;

        scope
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        self.bind_for_platform(std::env::consts::OS)
    }
}

/// Host facts the native availability check depends on.
#[derive(Clone)]
pub(crate) struct Host {
    /// Whether the kernel enforces Landlock write grants (Linux only).
    pub(crate) landlock: bool,
    /// Operating system, as [`std::env::consts::OS`] names it.
    pub(crate) platform: &'static str,
    /// The Seatbelt front end (macOS only).
    pub(crate) seatbelt: PathBuf,
}

impl Host {
    /// Returns the facts of the host this process runs on.
    pub(crate) fn current() -> Self {
        #[cfg(target_os = "linux")]
        let landlock = super::landlock::validate_write_grants(1, super::landlock::abi()).is_ok();
        #[cfg(not(target_os = "linux"))]
        let landlock = false;

        Self {
            landlock,
            platform: std::env::consts::OS,
            seatbelt: PathBuf::from(SEATBELT),
        }
    }
}

/// Executables a native sandbox launch goes through once checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeLaunch {
    /// Bubblewrap on Linux; `None` on macOS.
    pub(crate) bubblewrap: Option<PathBuf>,
    /// Helper executable the launch starts.
    pub(crate) launcher: PathBuf,
}

/// Checks, before any command, that the native sandbox can run
/// `configuration`'s commands in `workspace` on `host` through `helper`.
/// It runs the configured Bash once inside an empty Seatbelt profile on
/// macOS, which fails inside another sandbox, and inside fresh Bubblewrap
/// namespaces on Linux, which fails where unprivileged user namespaces are
/// disabled. It checks whether the sandbox can start, not the policy: a
/// Bash or grant the launch rejects still fails each command closed.
///
/// # Errors
/// Returns what is missing for the native sandbox to run.
pub(crate) async fn probe(
    configuration: &BashConfig,
    helper: Option<&Path>,
    workspace: &Path,
    host: Host,
) -> Result<NativeLaunch, SandboxUnavailableReason> {
    if !matches!(host.platform, "linux" | "macos") {
        return Err(SandboxUnavailableReason::UnsupportedPlatform);
    }
    let launcher = helper.ok_or(SandboxUnavailableReason::HelperMissing)?;
    validate_executable(&LocalInspection, launcher, workspace)
        .await
        .map_err(|_| SandboxUnavailableReason::HelperUntrusted)?;
    let launcher = launcher.to_path_buf();
    let bash = &configuration.snapshot.bash;
    // A Bash the launch rejects cannot tell whether the sandbox starts; its
    // commands fail closed at launch instead of running unsandboxed.
    let trial = validate_executable(&LocalInspection, bash, workspace)
        .await
        .is_ok();
    if host.platform == "macos" {
        validate_executable(&LocalInspection, &host.seatbelt, workspace)
            .await
            .map_err(|_| SandboxUnavailableReason::SeatbeltMissing)?;
        let mut seatbelt = tokio::process::Command::new(&host.seatbelt);
        seatbelt
            .args(["-p", "(version 1)(allow default)"])
            .arg(bash);
        if trial && !runs(seatbelt).await {
            return Err(SandboxUnavailableReason::SeatbeltUnavailable);
        }

        return Ok(NativeLaunch {
            bubblewrap: None,
            launcher,
        });
    }
    let snapshot = &configuration.snapshot;
    let writes = !snapshot.workspace_writes.is_empty()
        || snapshot.temporary_directory.is_some()
        || snapshot.grants(Capability::GitWrites);
    if !host.landlock && writes {
        return Err(SandboxUnavailableReason::LandlockUnavailable);
    }
    let bubblewrap = configuration
        .snapshot
        .linux_bubblewrap
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BUBBLEWRAP));
    validate_executable(&LocalInspection, &bubblewrap, workspace)
        .await
        .map_err(|_| SandboxUnavailableReason::BubblewrapMissing)?;
    let mut namespaces = tokio::process::Command::new(&bubblewrap);
    namespaces
        .args([
            "--unshare-all",
            "--uid",
            "0",
            "--gid",
            "0",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--",
        ])
        .arg(bash);
    if trial && !runs(namespaces).await {
        return Err(SandboxUnavailableReason::UserNamespacesUnavailable);
    }

    Ok(NativeLaunch {
        bubblewrap: Some(bubblewrap),
        launcher,
    })
}

/// Returns whether `command`, which wraps Bash, runs Bash's no-op command
/// successfully within [`PROBE_TIMEOUT`].
async fn runs(mut command: tokio::process::Command) -> bool {
    command
        .args(["--noprofile", "--norc", "-c", ":"])
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Bubblewrap the Linux sandbox uses when the configuration names none.
const DEFAULT_BUBBLEWRAP: &str = "/usr/bin/bwrap";

/// Longest wait for the sandbox's trial run; a stalled sandbox counts as
/// unavailable.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The macOS Seatbelt front end, which the operating system ships.
const SEATBELT: &str = "/usr/bin/sandbox-exec";

struct NativeProcess {
    child: Option<Child>,
    configuration: BashConfig,
    encoded: Vec<u8>,
    finished: bool,
    input: Option<BufReader<UnixStream>>,
    notice: Vec<u8>,
    stderr: Option<ChildStderr>,
    stdout: Option<ChildStdout>,
}

/// Filesystem access one command launches with, resolved by
/// [`NativeProcess::prepare`].
#[derive(Default)]
struct Access {
    external_reads: Vec<PathBuf>,
    git_metadata: Vec<PathBuf>,
    git_writes: Option<GitWrites>,
    write_nodes: Vec<(u64, u64)>,
    writes: Vec<PathBuf>,
}

impl NativeProcess {
    fn launch(
        &self,
        command: &ExecutionCommand,
        policy: &ExecutionPolicy,
        access: Access,
    ) -> Result<Launch, ExecutionError> {
        let arguments = command
            .arguments()
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(ExecutionError::Setup)
            })
            .collect::<Result<_, _>>()?;
        Ok(Launch {
            arguments,
            directory: policy.workspace().join(command.directory()),
            environment: policy
                .environment()
                .iter()
                .map(|(name, value)| {
                    Ok((
                        name.to_str().ok_or(ExecutionError::Setup)?.to_owned(),
                        value.to_str().ok_or(ExecutionError::Setup)?.to_owned(),
                    ))
                })
                .collect::<Result<_, ExecutionError>>()?,
            executable: command.executable().to_path_buf(),
            external_reads: access.external_reads,
            git_metadata: access.git_metadata,
            git_writes: access.git_writes,
            host_information: policy.exposes_host_information(),
            launcher: self
                .configuration
                .snapshot
                .launcher
                .clone()
                .ok_or(ExecutionError::Unsupported)?,
            linux_bubblewrap: self.configuration.snapshot.linux_bubblewrap.clone(),
            network: policy.allows_network(),
            system: self.configuration.snapshot.grants(Capability::SystemReads),
            workspace: policy.workspace().to_path_buf(),
            write_nodes: access.write_nodes,
            writes: access.writes,
        })
    }
}

/// Resolves what one command may access without inspecting the contents of
/// any granted directory: every check is path-level, so its cost does not
/// grow with the workspace or the grants. The sandboxes resolve links at
/// access time, against the granted paths.
///
/// # Errors
/// Returns [`ExecutionError::Unsupported`] for a grant the sandbox cannot
/// honor: a missing path, a read grant overlapping the workspace, a write
/// grant that is not a directory under its own name, a temporary directory
/// overlapping the workspace or its Git metadata, or Git writes the
/// repository cannot confine.
async fn resolve_access(
    files: &dyn Inspection,
    policy: &ExecutionPolicy,
) -> Result<Access, ExecutionError> {
    let workspace = policy.workspace();
    let repository = git_directories(files, workspace).await?;
    let mut access = Access {
        git_metadata: policy.git_metadata().to_vec(),
        ..Access::default()
    };
    for read in policy.external_reads() {
        let target = files
            .canonicalize(read)
            .await
            .map_err(|_| ExecutionError::Unsupported)?;
        let parent = files
            .canonicalize(read.parent().ok_or(ExecutionError::Unsupported)?)
            .await
            .map_err(|_| ExecutionError::Unsupported)?;
        if overlaps(&target, workspace)
            || parent.starts_with(workspace)
            || overlaps(read, workspace)
        {
            return Err(ExecutionError::Unsupported);
        }
        access.external_reads.push(read.clone());
        if target != *read {
            access.external_reads.push(target);
        }
    }
    if let Some(repository) = &repository {
        for directory in [&repository.admin, &repository.common] {
            access.git_metadata.push(directory.clone());
            if !directory.starts_with(workspace) {
                access.external_reads.push(directory.clone());
            }
        }
    }
    for write in policy.workspace_writes() {
        let path = workspace.join(write);
        let (canonical, node) = directory_identity(files, &path).await?;
        if canonical != path {
            return Err(ExecutionError::Unsupported);
        }
        // Linux protects Git metadata by mounting it read-only, so it
        // protects a repository at the top of each write grant too.
        access.git_metadata.push(canonical.join(".git"));
        access.writes.push(canonical);
        access.write_nodes.push(node);
    }
    if let Some(temporary) = policy.temporary_directory() {
        let (canonical, node) = directory_identity(files, temporary).await?;
        if overlaps(&canonical, workspace)
            || access
                .git_metadata
                .iter()
                .any(|git| overlaps(&canonical, git))
        {
            return Err(ExecutionError::Unsupported);
        }
        access.writes.push(canonical);
        access.write_nodes.push(node);
    }
    if policy.allows_git_writes() {
        let repository = repository.ok_or(ExecutionError::Unsupported)?;
        access.git_writes = Some(repository.writes(files).await?);
    }

    Ok(access)
}

/// Returns whether one of two paths contains the other.
fn overlaps(path: &Path, other: &Path) -> bool {
    path.starts_with(other) || other.starts_with(path)
}

/// Returns the resolved path and `(device, inode)` identity of the
/// directory `path` names.
async fn directory_identity(
    files: &dyn Inspection,
    path: &Path,
) -> Result<(PathBuf, (u64, u64)), ExecutionError> {
    let canonical = files
        .canonicalize(path)
        .await
        .map_err(|_| ExecutionError::Unsupported)?;
    let metadata = files
        .metadata(&canonical)
        .await
        .map_err(|_| ExecutionError::Unsupported)?;
    if !metadata.is_dir() {
        return Err(ExecutionError::Unsupported);
    }

    Ok((canonical, (metadata.dev(), metadata.ino())))
}

/// The workspace repository's resolved Git directories.
struct GitDirectories {
    /// The directory the workspace's `.git` entry names: the repository's
    /// own Git directory, or a linked worktree's administration directory.
    admin: PathBuf,
    /// The common directory `admin/commondir` names, or `admin` itself.
    common: PathBuf,
}

impl GitDirectories {
    /// Returns the directories the Git-writes grant opens.
    ///
    /// # Errors
    /// Returns [`ExecutionError::Unsupported`] for a linked worktree whose
    /// repository enables per-worktree configuration while the worktree has
    /// no `config.worktree`: the Linux sandbox could not keep a command from
    /// creating one that Git run later outside the sandbox would read.
    async fn writes(self, files: &dyn Inspection) -> Result<GitWrites, ExecutionError> {
        if self.admin == self.common {
            return Ok(GitWrites {
                admin: None,
                common: self.common,
            });
        }
        let configuration =
            bounded_bytes(&self.common.join("config"), MAX_GIT_CONFIGURATION_BYTES).await?;
        let per_worktree = configuration
            .to_ascii_lowercase()
            .windows(WORKTREE_CONFIGURATION.len())
            .any(|window| window == WORKTREE_CONFIGURATION);
        if per_worktree
            && !files
                .try_exists(&self.admin.join("config.worktree"))
                .await
                .map_err(|_| ExecutionError::Setup)?
        {
            return Err(ExecutionError::Unsupported);
        }

        Ok(GitWrites {
            admin: Some(self.admin),
            common: self.common,
        })
    }
}

/// Lowercase name of the `extensions.worktreeConfig` setting.
const WORKTREE_CONFIGURATION: &[u8] = b"worktreeconfig";

/// Largest Git configuration the Git-writes check reads.
const MAX_GIT_CONFIGURATION_BYTES: u64 = 1024 * 1024;

/// Largest `.git` or `commondir` pointer file read.
const MAX_GIT_POINTER_BYTES: u64 = 4096;

/// Resolves the Git directories of the workspace root's `.git` entry: a
/// directory, or a file naming one, whose `commondir` may name another.
/// Returns `None` when the workspace has no `.git` entry.
async fn git_directories(
    files: &dyn Inspection,
    workspace: &Path,
) -> Result<Option<GitDirectories>, ExecutionError> {
    let entry = workspace.join(".git");
    let metadata = match files.metadata(&entry).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ExecutionError::Setup),
    };
    let admin = if metadata.is_dir() {
        entry
    } else {
        let text = pointer(&entry).await?;
        let target = text
            .trim()
            .strip_prefix("gitdir: ")
            .ok_or(ExecutionError::Unsupported)?;
        workspace.join(target)
    };
    let admin = files
        .canonicalize(&admin)
        .await
        .map_err(|_| ExecutionError::Setup)?;
    let common = admin.join("commondir");
    let common = if files
        .try_exists(&common)
        .await
        .map_err(|_| ExecutionError::Setup)?
    {
        let text = pointer(&common).await?;
        files
            .canonicalize(&admin.join(text.trim()))
            .await
            .map_err(|_| ExecutionError::Setup)?
    } else {
        admin.clone()
    };

    Ok(Some(GitDirectories { admin, common }))
}

#[async_trait]
impl BashProcess for NativeProcess {
    async fn prepare(
        &mut self,
        command: &ExecutionCommand,
        policy: &ExecutionPolicy,
    ) -> Result<(), ExecutionError> {
        let snapshot = &self.configuration.snapshot;
        if !policy.exposes_host_information() {
            return Err(ExecutionError::Unsupported);
        }
        // Linux write grants rely on Landlock rules applied inside the
        // launcher; kernels without the required ABI fail closed here.
        #[cfg(target_os = "linux")]
        super::landlock::validate_write_grants(write_grants(policy), super::landlock::abi())?;
        if policy.requested_access(command.executable()) == ExecutionAccess::Denied {
            return Err(ExecutionError::Unsupported);
        }
        validate_executable(
            &LocalInspection,
            snapshot
                .launcher
                .as_deref()
                .ok_or(ExecutionError::Unsupported)?,
            policy.workspace(),
        )
        .await?;
        validate_executable(&LocalInspection, command.executable(), policy.workspace()).await?;
        if cfg!(target_os = "linux") {
            validate_executable(
                &LocalInspection,
                snapshot
                    .linux_bubblewrap
                    .as_deref()
                    .ok_or(ExecutionError::Unsupported)?,
                policy.workspace(),
            )
            .await?;
        }
        #[cfg(target_os = "macos")]
        validate_executable(&LocalInspection, Path::new(SEATBELT), policy.workspace()).await?;
        let access = resolve_access(&LocalInspection, policy).await?;
        let launch = self.launch(command, policy, access)?;
        self.encoded = serde_json::to_vec(&launch).map_err(|_| ExecutionError::Setup)?;
        if self.encoded.len() > 1024 * 1024 {
            return Err(ExecutionError::Setup);
        }
        self.encoded.push(b'\n');

        Ok(())
    }

    async fn start(&mut self) -> Result<(), ExecutionError> {
        let (parent, child) =
            std::os::unix::net::UnixStream::pair().map_err(|_| ExecutionError::Setup)?;
        parent
            .set_nonblocking(true)
            .map_err(|_| ExecutionError::Setup)?;
        self.input = Some(BufReader::new(
            UnixStream::from_std(parent).map_err(|_| ExecutionError::Setup)?,
        ));
        let child: OwnedFd = child.into();
        let launcher = self
            .configuration
            .snapshot
            .launcher
            .as_deref()
            .ok_or(ExecutionError::Setup)?;
        let mut command = tokio::process::Command::new(launcher);
        command
            .arg(super::launcher::HELPER_ARGUMENT)
            .env_clear()
            .current_dir("/")
            .process_group(0)
            .stdin(Stdio::from(child))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.child = Some(command.spawn().map_err(|_| ExecutionError::Setup)?);
        let child = self.child.as_mut().ok_or(ExecutionError::Setup)?;
        self.stdout = child.stdout.take();
        self.stderr = child.stderr.take();
        self.input
            .as_mut()
            .ok_or(ExecutionError::Setup)?
            .get_mut()
            .write_all(&self.encoded)
            .await
            .map_err(|_| ExecutionError::Setup)?;
        #[cfg(not(target_os = "linux"))]
        self.encoded.clear();

        Ok(())
    }

    async fn next_event(&mut self, buffer: &mut [u8]) -> Result<ProcessEvent, ExecutionError> {
        let capacity = buffer.len().min(4096);
        if capacity == 0 {
            return Err(ExecutionError::Process);
        }
        let input = self.input.as_mut().ok_or(ExecutionError::Process)?;
        tokio::select! {
            result = output::next_event(&mut self.stdout, &mut self.stderr, buffer), if self.stdout.is_some() || self.stderr.is_some() => result,
            result = input.read_until(b'\n', &mut self.notice), if !self.finished => {
                if result.map_err(|_| ExecutionError::Process)? == 0 || self.notice.len() > 256 {
                    return Err(ExecutionError::Process);
                }
                let notice = serde_json::from_slice(&self.notice).map_err(|_| ExecutionError::Process)?;
                self.notice.clear();
                match notice {
                    Notice::Configure if cfg!(target_os = "linux") && !self.encoded.is_empty() => {
                        input.get_mut().write_all(&self.encoded).await.map_err(|_| ExecutionError::Process)?;
                        self.encoded.clear();
                        Ok(ProcessEvent::Progress)
                    }
                    Notice::MainExit { code, signal } => Ok(ProcessEvent::MainExit(code.map(MainExit::Code).or_else(|| signal.map(MainExit::Signal)).unwrap_or(MainExit::Unavailable))),
                    Notice::Finished => {
                        self.finished = true;
                        input.get_mut().write_all(b"x").await.map_err(|_| ExecutionError::Process)?;
                        Ok(ProcessEvent::Quiescent)
                    }
                    Notice::Configure | Notice::Failed => Err(ExecutionError::Process),
                }
            }
        }
    }

    async fn cleanup(&mut self) -> Result<(), ExecutionError> {
        self.stdout = None;
        self.stderr = None;
        let Some(child) = &mut self.child else {
            return Ok(());
        };
        if self.finished || cfg!(target_os = "linux") {
            if let Some(input) = &mut self.input {
                let _ = input.get_mut().write_all(b"x").await;
                let _ = input.get_mut().shutdown().await;
            }
        } else if let Some(id) = child.id() {
            let pid = Pid::from_raw(i32::try_from(id).map_err(|_| ExecutionError::Cleanup)?)
                .ok_or(ExecutionError::Cleanup)?;
            match kill_process_group(pid, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(_) => return Err(ExecutionError::Cleanup),
            }
        }
        let status = child.wait().await.map_err(|_| ExecutionError::Cleanup)?;
        if cfg!(target_os = "linux") && !status.success() && !self.finished {
            return Err(ExecutionError::CleanupUnconfirmed);
        }
        self.child = None;
        self.input = None;

        Ok(())
    }
}

#[async_trait]
trait Inspection: Send + Sync {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    async fn metadata(&self, path: &Path) -> io::Result<Metadata>;
    async fn try_exists(&self, path: &Path) -> io::Result<bool>;
}

struct LocalInspection;

#[async_trait]
impl Inspection for LocalInspection {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        tokio::fs::canonicalize(path).await
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        tokio::fs::metadata(path).await
    }

    async fn try_exists(&self, path: &Path) -> io::Result<bool> {
        tokio::fs::try_exists(path).await
    }
}

async fn validate_executable(
    files: &dyn Inspection,
    path: &Path,
    workspace: &Path,
) -> Result<(), ExecutionError> {
    let canonical = files
        .canonicalize(path)
        .await
        .map_err(|_| ExecutionError::Unsupported)?;
    let parent = files
        .canonicalize(path.parent().ok_or(ExecutionError::Unsupported)?)
        .await
        .map_err(|_| ExecutionError::Unsupported)?;
    let metadata = files
        .metadata(path)
        .await
        .map_err(|_| ExecutionError::Unsupported)?;
    if !path.is_absolute()
        || path.starts_with(workspace)
        || canonical.starts_with(workspace)
        || parent.starts_with(workspace)
        || metadata.permissions().mode() & 0o6000 != 0
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(ExecutionError::Unsupported);
    }

    Ok(())
}

/// Returns the number of directories a command can write, which Linux
/// confines with Landlock.
#[cfg(target_os = "linux")]
fn write_grants(policy: &ExecutionPolicy) -> usize {
    policy.workspace_writes().len()
        + usize::from(policy.temporary_directory().is_some())
        + usize::from(policy.allows_git_writes())
}

/// Reads one Git pointer file, such as a `.git` file or `commondir`.
async fn pointer(path: &Path) -> Result<String, ExecutionError> {
    String::from_utf8(bounded_bytes(path, MAX_GIT_POINTER_BYTES).await?)
        .map_err(|_| ExecutionError::Unsupported)
}

/// Reads the file at `path`, refusing one longer than `limit` bytes.
async fn bounded_bytes(path: &Path, limit: u64) -> Result<Vec<u8>, ExecutionError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ExecutionError::Setup)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ExecutionError::Setup)?;
    if bytes.len() as u64 > limit {
        return Err(ExecutionError::Unsupported);
    }

    Ok(bytes)
}

#[cfg(test)]
#[path = "native_test.rs"]
mod tests;
