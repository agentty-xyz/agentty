//! Native spawn owns only a main process. Fork remains denied until supervision
//! exists.

use std::ffi::{CString, OsString};
use std::io::{self, PipeReader};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command as HostCommand, ExitStatus};
use std::ptr;

use crate::execution::contract::Command;
use crate::execution::macos::configuration::{Configuration, unsupported};

/// Retains scratch and reaping authority from the instant spawn succeeds.
/// This is intentionally inaccessible outside the backend module.
pub(super) struct Child {
    pub(super) stderr: PipeReader,
    pub(super) stdout: PipeReader,
    configuration: Configuration,
    pid: libc::pid_t,
    status: Option<ExitStatus>,
}

impl Child {
    pub(super) fn spawn(configuration: Configuration, command: &Command) -> io::Result<Self> {
        let version = HostCommand::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .env_clear()
            .output()?;
        validate_runtime(
            std::env::consts::ARCH,
            &version.stdout,
            version.status.success(),
        )?;
        let profile = configuration.profile(command)?;
        let mut arguments = vec![
            OsString::from("/usr/bin/sandbox-exec"),
            "-p".into(),
            profile.into(),
            "/usr/bin/env".into(),
            "-i".into(),
            "--".into(),
        ];
        for (name, value) in configuration.environment() {
            let mut assignment = name.clone();
            assignment.push("=");
            assignment.push(value);
            arguments.push(assignment);
        }
        arguments.push(command.executable().as_os_str().to_owned());
        arguments.extend_from_slice(command.arguments());
        let arguments = c_arguments(&arguments)?;
        let directory = CString::new(configuration.directory(command).as_os_str().as_bytes())?;
        let (stdin, input) = io::pipe()?;
        drop(input);
        let (stdout, output) = io::pipe()?;
        let (stderr, error) = io::pipe()?;
        let descriptors = [
            rustix::io::fcntl_dupfd_cloexec(stdin, 3)?,
            rustix::io::fcntl_dupfd_cloexec(output, 3)?,
            rustix::io::fcntl_dupfd_cloexec(error, 3)?,
        ];
        let pid = spawn(&arguments, &directory, &descriptors)?;

        Ok(Self {
            stderr,
            stdout,
            configuration,
            pid,
            status: None,
        })
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = wait(self.pid, libc::WNOHANG)?;
        }

        Ok(self.status)
    }

    pub(super) fn scratch(&self) -> &std::path::Path {
        self.configuration.scratch()
    }

    /// Reports cleanup failures while retaining ownership for a later retry.
    pub(super) fn cleanup(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_none() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }

        self.configuration.cleanup()
    }
}

impl Drop for Child {
    #[expect(
        unsafe_code,
        reason = "the owned PID must be killed and reaped before releasing scratch"
    )]
    fn drop(&mut self) {
        if self.status.is_none() {
            // SAFETY: this unreaped PID remains owned by this child; fork is
            // denied.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            let _ = wait(self.pid, 0);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "macOS descriptor closure requires the native posix_spawn API"
)]
fn spawn(
    arguments: &[CString],
    directory: &CString,
    descriptors: &[OwnedFd; 3],
) -> io::Result<libc::pid_t> {
    // SAFETY: all strings and file descriptors outlive spawn; the OS copies
    // them. Initialized action/attribute objects have exactly one RAII
    // owner.
    unsafe {
        let mut actions = ptr::null_mut();
        check(libc::posix_spawn_file_actions_init(&raw mut actions))?;
        let mut actions = Actions(actions);
        let mut attributes = ptr::null_mut();
        check(libc::posix_spawnattr_init(&raw mut attributes))?;
        let mut attributes = Attributes(attributes);
        const {
            assert!(libc::POSIX_SPAWN_CLOEXEC_DEFAULT == 0x4000);
        }
        check(libc::posix_spawnattr_setflags(
            &raw mut attributes.0,
            0x4000,
        ))?;
        check(posix_spawn_file_actions_addchdir(
            &raw mut actions.0,
            directory.as_ptr(),
        ))?;
        for (descriptor, target) in descriptors.iter().zip(0..3) {
            check(libc::posix_spawn_file_actions_adddup2(
                &raw mut actions.0,
                descriptor.as_raw_fd(),
                target,
            ))?;
        }
        let mut argv: Vec<_> = arguments
            .iter()
            .map(|value| value.as_ptr().cast_mut())
            .collect();
        argv.push(ptr::null_mut());
        let environment = [ptr::null_mut()];
        let mut pid = 0;
        check(libc::posix_spawn(
            &raw mut pid,
            arguments[0].as_ptr(),
            &raw const actions.0,
            &raw const attributes.0,
            argv.as_ptr(),
            environment.as_ptr(),
        ))?;

        Ok(pid)
    }
}

struct Actions(libc::posix_spawn_file_actions_t);

impl Drop for Actions {
    #[expect(
        unsafe_code,
        reason = "release the successfully initialized native spawn actions"
    )]
    fn drop(&mut self) {
        // SAFETY: this is the unique initialized owner, and spawn has returned.
        unsafe {
            libc::posix_spawn_file_actions_destroy(&raw mut self.0);
        }
    }
}

struct Attributes(libc::posix_spawnattr_t);

impl Drop for Attributes {
    #[expect(
        unsafe_code,
        reason = "release the successfully initialized native spawn attributes"
    )]
    fn drop(&mut self) {
        // SAFETY: this is the unique initialized owner, and spawn has returned.
        unsafe {
            libc::posix_spawnattr_destroy(&raw mut self.0);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "libc has not yet bound the macOS 26 standardized chdir action"
)]
unsafe extern "C" {
    fn posix_spawn_file_actions_addchdir(
        actions: *mut libc::posix_spawn_file_actions_t,
        path: *const libc::c_char,
    ) -> libc::c_int;
}

#[expect(
    unsafe_code,
    reason = "waitpid consumes the backend-owned child status"
)]
fn wait(pid: libc::pid_t, flags: libc::c_int) -> io::Result<Option<ExitStatus>> {
    retry_interrupted(|| {
        let mut status = 0;
        // SAFETY: status is writable and pid identifies our unreaped child.
        let result = unsafe { libc::waitpid(pid, &raw mut status, flags) };
        if result == pid {
            return Ok(Some(ExitStatus::from_raw(status)));
        }
        if result == 0 {
            return Ok(None);
        }

        Err(io::Error::last_os_error())
    })
}

fn retry_interrupted<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match operation() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

fn c_arguments(arguments: &[OsString]) -> io::Result<Vec<CString>> {
    Ok(arguments
        .iter()
        .map(|value| CString::new(value.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?)
}

fn validate_runtime(architecture: &str, version: &[u8], success: bool) -> io::Result<()> {
    if architecture != "aarch64" || !success || !version.starts_with(b"26.") {
        return Err(unsupported("native isolation requires arm64 macOS 26"));
    }

    Ok(())
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }

    Ok(())
}

#[cfg(test)]
#[path = "launch_test.rs"]
mod tests;
