//! Backend-private process construction, deliberately not an `Executor`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::configuration::Configuration;
use super::filter;
use crate::execution::contract::Access;

/// Owns process termination and temporary resources from the instant of spawn.
/// The future supervisor must retain this owner while awaiting/draining output.
pub(super) struct Launch {
    pub(super) child: Child,
    scratch: TempDir,
}

impl Launch {
    /// `bubblewrap` is a trusted host-installed, non-setuid bubblewrap >=
    /// 0.9.0. `entrypoint` is a trusted static native build of `entrypoint.c`.
    /// Required namespace or filter setup failure never retries unsandboxed.
    pub(super) async fn start(
        configuration: &Configuration,
        bubblewrap: &Path,
        entrypoint: &Path,
    ) -> io::Result<Self> {
        configuration.validate()?;
        if configuration.policy().requested_access(entrypoint) != Ok(Access::ReadOnly) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "entrypoint requires an explicit read-only grant",
            ));
        }
        Self::validate_launcher(bubblewrap)?;
        Self::validate_launcher(entrypoint)?;
        let scratch = tempfile::Builder::new()
            .prefix("ag-isolation-")
            .tempdir_in(configuration.scratch())?;
        configuration.validate_launch_directory(scratch.path())?;
        let payload = scratch.path().join("payload");
        let control = scratch.path().join("control");
        fs::create_dir(&payload)?;
        fs::create_dir(&control)?;
        fs::copy(entrypoint, control.join("entry"))?;
        let filter_path = scratch.path().join("filter");
        File::create(&filter_path)?.write_all(&filter::program()?)?;
        let filter = File::open(&filter_path)?;
        fs::remove_file(filter_path)?;

        let mut command = Command::new(bubblewrap);
        command
            .env_clear()
            .current_dir(configuration.scratch())
            .stdin(filter)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .args([
                "--unshare-user",
                "--unshare-ipc",
                "--unshare-pid",
                "--unshare-net",
                "--unshare-uts",
                "--unshare-cgroup",
                "--uid",
                "0",
                "--gid",
                "0",
                "--cap-drop",
                "ALL",
                "--new-session",
                "--die-with-parent",
                "--hostname",
                "isolated",
                "--clearenv",
                "--seccomp",
                "0",
            ]);
        Self::mount_sources(&mut command, configuration, scratch.path());
        for (name, value) in configuration.policy().environment() {
            command.arg("--setenv").arg(name).arg(value);
        }
        command
            .arg("--chdir")
            .arg(
                configuration
                    .policy()
                    .workspace()
                    .join(configuration.command().directory()),
            )
            .arg("--")
            .arg("/.ag-isolation/entry")
            .arg(configuration.command().executable())
            .args(configuration.command().arguments());
        Self::close_inherited_descriptors(&mut command);
        let child = command.spawn()?;

        let mut launch = Self { child, scratch };
        if let Err(error) = launch.wait_for_setup().await {
            launch.stop().await?;

            return Err(error);
        }

        Ok(launch)
    }

    /// Explicit stop/reap boundary for test owners and future supervision.
    pub(super) async fn stop(&mut self) -> io::Result<()> {
        self.child.kill().await?;

        Ok(())
    }

    fn validate_launcher(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !path.is_absolute() || !metadata.is_file() || metadata.mode() & 0o6000 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "launchers must be trusted non-setuid executables",
            ));
        }
        let mut magic = [0; 4];
        File::open(path)?.read_exact(&mut magic)?;
        if magic != *b"\x7fELF" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "launchers must be ELF executables",
            ));
        }

        Ok(())
    }

    fn mount_sources(command: &mut Command, configuration: &Configuration, scratch: &Path) {
        command
            .arg("--ro-bind")
            .arg(configuration.policy().workspace())
            .arg(configuration.policy().workspace());
        for root in configuration.policy().external_reads() {
            command.arg("--ro-bind").arg(root).arg(root);
        }
        for path in configuration.writable_paths() {
            command.arg("--bind").arg(&path).arg(&path);
        }
        // No procfs, sysfs, host /dev or host /tmp is mounted. This directory
        // is private to this launch and never overlaps a policy source
        // tree.
        command
            .arg("--bind")
            .arg(scratch.join("payload"))
            .arg("/tmp");
        command
            .arg("--ro-bind")
            .arg(scratch.join("control"))
            .arg("/.ag-isolation");
        command.args(["--remount-ro", "/"]);
    }

    async fn wait_for_setup(&mut self) -> io::Result<()> {
        let stdout = self
            .child
            .stdout
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?;
        let mut ready = [0];
        // Only the trusted entrypoint can write before the payload. Consume
        // exactly its byte, leaving every payload output byte in this pipe.
        tokio::time::timeout(Duration::from_secs(10), stdout.read_exact(&mut ready))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "bubblewrap setup timed out"))?
            .map_err(|error| {
                io::Error::other(format!("bubblewrap did not complete setup: {error}"))
            })?;
        if ready != [0] {
            return Err(io::Error::other("invalid setup acknowledgement"));
        }

        Ok(())
    }

    #[expect(
        unsafe_code,
        reason = "the post-fork close_range syscall has no safe Command API"
    )]
    fn close_inherited_descriptors(command: &mut Command) {
        // SAFETY: The callback performs one raw syscall and allocation-free
        // errno handling in the forked child.
        unsafe {
            command.pre_exec(close_descriptors);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "close_range is an integer-only raw Linux syscall"
)]
fn close_descriptors() -> io::Result<()> {
    // SAFETY: There are no pointer arguments. CLOEXEC preserves existing
    // descriptors, including Rust's spawn-error pipe, until exec.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };

    syscall_result(result)
}

fn syscall_result(result: libc::c_long) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
#[path = "launch_test.rs"]
mod tests;
