//! Native resource ownership beneath the shared, cancellation-safe supervisor.

use std::collections::HashSet;
use std::fs::Metadata;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStderr, ChildStdout};

use super::contract::{
    BashExecutor, BashProcess, ExecutionAccess, ExecutionCommand, ExecutionError, ExecutionPolicy,
    MainExit, OutputStream, ProcessEvent,
};
use super::supervisor::Clock;
use super::wire::{Launch, Notice};
use crate::bash::BashConfig;
use crate::command_journal::CommandCleanupScope;

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

pub(super) struct Monotonic;

#[async_trait]
impl Clock for Monotonic {
    fn now(&self) -> Instant {
        Instant::now()
    }

    async fn wait_until(&self, deadline: Instant) {
        tokio::time::sleep_until(deadline.into()).await;
    }
}

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
        // Bubblewrap binds cannot deny Git metadata created later beneath a
        // writable directory, so Linux rejects write grants until dedicated
        // filesystem enforcement lands.
        #[cfg(target_os = "linux")]
        if !policy.workspace_writes().is_empty() {
            return Err(ExecutionError::Unsupported);
        }
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
        validate_executable(
            &LocalInspection,
            Path::new("/usr/bin/sandbox-exec"),
            policy.workspace(),
        )
        .await?;
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
        for write in policy.workspace_writes() {
            let path = policy.workspace().join(write);
            let canonical = tokio::fs::canonicalize(&path)
                .await
                .map_err(|_| ExecutionError::Setup)?;
            if canonical != path
                || !tokio::fs::metadata(&path)
                    .await
                    .map_err(|_| ExecutionError::Setup)?
                    .is_dir()
            {
                return Err(ExecutionError::Unsupported);
            }
        }
        let launch = self.launch(command, policy, git_metadata)?;
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
        let mut output = [0; 4096];
        let mut errors = [0; 4096];
        let input = self.input.as_mut().ok_or(ExecutionError::Process)?;
        tokio::select! {
            result = async { self.stdout.as_mut().ok_or(ExecutionError::Process)?.read(&mut output[..capacity]).await.map_err(|_| ExecutionError::Process) }, if self.stdout.is_some() => {
                let length = result?;
                if length == 0 {
                    self.stdout = None;
                    return Ok(ProcessEvent::Eof(OutputStream::Stdout));
                }
                buffer[..length].copy_from_slice(&output[..length]);
                Ok(ProcessEvent::Output(OutputStream::Stdout, length))
            }
            result = async { self.stderr.as_mut().ok_or(ExecutionError::Process)?.read(&mut errors[..capacity]).await.map_err(|_| ExecutionError::Process) }, if self.stderr.is_some() => {
                let length = result?;
                if length == 0 {
                    self.stderr = None;
                    return Ok(ProcessEvent::Eof(OutputStream::Stderr));
                }
                buffer[..length].copy_from_slice(&errors[..length]);
                Ok(ProcessEvent::Output(OutputStream::Stderr, length))
            }
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
