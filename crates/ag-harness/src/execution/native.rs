//! Native resource ownership beneath the shared, cancellation-safe supervisor.

use std::collections::HashSet;
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
use super::wire::{Launch, Notice};
use crate::bash::BashConfig;
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
    if !host.landlock && !configuration.snapshot.workspace_writes.is_empty() {
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

impl NativeProcess {
    fn launch(
        &self,
        command: &ExecutionCommand,
        policy: &ExecutionPolicy,
        git_metadata: Vec<PathBuf>,
        workspace_write_nodes: Vec<(u64, u64)>,
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
            external_reads: policy.external_reads().to_vec(),
            git_metadata,
            host_information: policy.exposes_host_information(),
            launcher: self
                .configuration
                .snapshot
                .launcher
                .clone()
                .ok_or(ExecutionError::Unsupported)?,
            linux_bubblewrap: self.configuration.snapshot.linux_bubblewrap.clone(),
            workspace: policy.workspace().to_path_buf(),
            workspace_write_nodes,
            workspace_writes: policy.workspace_writes().to_vec(),
        })
    }
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
        super::landlock::validate_write_grants(
            policy.workspace_writes().len(),
            super::landlock::abi(),
        )?;
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
        let mut git_metadata = policy.git_metadata().to_vec();
        inspect_tree(
            &LocalInspection,
            policy.workspace(),
            policy.workspace(),
            &mut git_metadata,
            true,
            100_000,
        )
        .await?;
        for read in policy.external_reads() {
            let target = tokio::fs::canonicalize(read)
                .await
                .map_err(|_| ExecutionError::Unsupported)?;
            let parent = tokio::fs::canonicalize(read.parent().ok_or(ExecutionError::Unsupported)?)
                .await
                .map_err(|_| ExecutionError::Unsupported)?;
            if target.starts_with(policy.workspace())
                || policy.workspace().starts_with(&target)
                || parent.starts_with(policy.workspace())
            {
                return Err(ExecutionError::Unsupported);
            }
            if read.starts_with(policy.workspace()) || policy.workspace().starts_with(read) {
                return Err(ExecutionError::Unsupported);
            }
            inspect_tree(
                &LocalInspection,
                read,
                policy.workspace(),
                &mut git_metadata,
                false,
                100_000,
            )
            .await?;
        }
        let mut workspace_write_nodes = Vec::new();
        for write in policy.workspace_writes() {
            let path = policy.workspace().join(write);
            let canonical = tokio::fs::canonicalize(&path)
                .await
                .map_err(|_| ExecutionError::Setup)?;
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|_| ExecutionError::Setup)?;
            if canonical != path || !metadata.is_dir() {
                return Err(ExecutionError::Unsupported);
            }
            workspace_write_nodes.push((metadata.dev(), metadata.ino()));
        }
        let launch = self.launch(command, policy, git_metadata, workspace_write_nodes)?;
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
    async fn read_dir(&self, path: &Path) -> io::Result<Box<dyn Directory>>;
    async fn try_exists(&self, path: &Path) -> io::Result<bool>;
}

#[async_trait]
trait Directory: Send {
    async fn next_entry(&mut self) -> io::Result<Option<PathBuf>>;
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

    async fn read_dir(&self, path: &Path) -> io::Result<Box<dyn Directory>> {
        Ok(Box::new(LocalDirectory(tokio::fs::read_dir(path).await?)))
    }

    async fn try_exists(&self, path: &Path) -> io::Result<bool> {
        tokio::fs::try_exists(path).await
    }
}

struct LocalDirectory(tokio::fs::ReadDir);

#[async_trait]
impl Directory for LocalDirectory {
    async fn next_entry(&mut self) -> io::Result<Option<PathBuf>> {
        Ok(self.0.next_entry().await?.map(|entry| entry.path()))
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

async fn inspect_tree(
    files: &dyn Inspection,
    root: &Path,
    workspace: &Path,
    git: &mut Vec<PathBuf>,
    mutable: bool,
    max_entries: usize,
) -> Result<(), ExecutionError> {
    let mut pending = vec![root.to_path_buf()];
    let mut inspected = 0;
    let mut visited = HashSet::new();
    while let Some(path) = pending.pop() {
        inspected += 1;
        if inspected > max_entries {
            return Err(ExecutionError::Unsupported);
        }
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| ExecutionError::Setup)?;
        if metadata.file_type().is_symlink() {
            if !inspect_alias(files, &path, mutable, &mut pending).await? {
                continue;
            }
        } else if metadata.is_dir() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let mut entries = files
                .read_dir(&path)
                .await
                .map_err(|_| ExecutionError::Setup)?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|_| ExecutionError::Setup)?
            {
                if inspected + pending.len() >= max_entries {
                    return Err(ExecutionError::Unsupported);
                }
                pending.push(entry);
            }
        } else if !metadata.is_file() || (mutable && metadata.nlink() != 1) {
            return Err(ExecutionError::Unsupported);
        }
        inspect_git_metadata(files, &path, workspace, metadata.is_file(), git).await?;
        tokio::task::yield_now().await;
    }

    Ok(())
}

async fn inspect_alias(
    files: &dyn Inspection,
    path: &Path,
    mutable: bool,
    pending: &mut Vec<PathBuf>,
) -> Result<bool, ExecutionError> {
    // Workspace aliases cannot be safely granted by path on both
    // backends.
    if mutable {
        return Err(ExecutionError::Unsupported);
    }
    let target = match files.metadata(path).await {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(ExecutionError::Setup),
    };
    if !target.is_file() && !target.is_dir() {
        return Err(ExecutionError::Unsupported);
    }
    if target.is_dir() {
        pending.push(
            files
                .canonicalize(path)
                .await
                .map_err(|_| ExecutionError::Setup)?,
        );
    }

    Ok(true)
}

async fn inspect_git_metadata(
    files: &dyn Inspection,
    path: &Path,
    workspace: &Path,
    is_file: bool,
    git: &mut Vec<PathBuf>,
) -> Result<(), ExecutionError> {
    if path
        .file_name()
        .is_some_and(|name| name.as_encoded_bytes().eq_ignore_ascii_case(b".git"))
    {
        git.push(path.to_path_buf());
        if is_file {
            let text = bounded_text(path).await?;
            if text.len() > 4096 {
                return Err(ExecutionError::Unsupported);
            }
            let target = text
                .trim()
                .strip_prefix("gitdir: ")
                .ok_or(ExecutionError::Unsupported)?;
            let target = files
                .canonicalize(path.parent().unwrap_or(workspace).join(target).as_path())
                .await
                .map_err(|_| ExecutionError::Setup)?;
            let common = target.join("commondir");
            if files
                .try_exists(&common)
                .await
                .map_err(|_| ExecutionError::Setup)?
            {
                let text = bounded_text(&common).await?;
                if text.len() > 4096 {
                    return Err(ExecutionError::Unsupported);
                }
                git.push(
                    files
                        .canonicalize(&target.join(text.trim()))
                        .await
                        .map_err(|_| ExecutionError::Setup)?,
                );
            }
            git.push(target);
        }
    }

    Ok(())
}

async fn bounded_text(path: &Path) -> Result<String, ExecutionError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ExecutionError::Setup)?;
    let mut text = String::new();
    file.take(4097)
        .read_to_string(&mut text)
        .await
        .map_err(|_| ExecutionError::Setup)?;

    Ok(text)
}

#[cfg(test)]
#[path = "native_test.rs"]
mod tests;
