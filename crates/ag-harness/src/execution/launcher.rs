//! Trusted single-threaded launcher. Untrusted code starts only after
//! isolation.

#[cfg(target_os = "macos")]
use std::fmt::Write as _;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::AsFd;
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, read, write};
use rustix::process::{Pid, WaitOptions, WaitStatus, wait};
use rustix::stdio::stdin;

use super::wire::{Launch, Notice};

pub(crate) fn run() -> ExitCode {
    // This executable has no worker threads and never returns to a host
    // runtime. CLOEXEC preserves Rust's descriptor ownership while closing
    // every extra descriptor at the next exec. Only the private channel and
    // output survive.
    close_fds::set_fds_cloexec(3, &[]);
    let result = launch();
    if result.is_err() {
        let _ = notice(&Notice::Failed);
    }

    if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn launch() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--seatbelt-child")) {
        let configuration = configuration()?;

        return Err(shell(&configuration).stdin(Stdio::null()).exec());
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--namespace-init")) {
        if rustix::process::getpid().as_raw_nonzero().get() != 1 {
            return Err(io::Error::other("not namespace init"));
        }
        notice(&Notice::Configure)?;
        let configuration = configuration()?;

        return supervise(shell(&configuration));
    }
    let configuration = configuration()?;
    #[cfg(target_os = "macos")]
    {
        let profile = seatbelt(&configuration)?;
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .args(["-p", &profile])
            .arg(&configuration.launcher)
            .arg("--seatbelt-child");
        command.current_dir(&configuration.directory).env_clear();

        supervise(command, &configuration)
    }
    #[cfg(target_os = "linux")]
    {
        bubblewrap(&configuration)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = configuration;

        Err(io::Error::other("unsupported native platform"))
    }
}

fn configuration() -> io::Result<Launch> {
    let mut encoded = Vec::new();
    io::stdin()
        .lock()
        .take(2 * 1024 * 1024)
        .read_until(b'\n', &mut encoded)?;

    serde_json::from_slice(&encoded).map_err(io::Error::other)
}

fn shell(configuration: &Launch) -> Command {
    let mut command = Command::new(&configuration.executable);
    command
        .args(&configuration.arguments)
        .current_dir(&configuration.directory)
        .env_clear()
        .envs(&configuration.environment);

    command
}

fn supervise(
    mut command: Command,
    #[cfg(target_os = "macos")] configuration: &Launch,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut sender = {
        let (parent, child) = UnixStream::pair()?;
        let child: OwnedFd = child.into();
        command.stdin(Stdio::from(child));

        parent
    };
    #[cfg(not(target_os = "macos"))]
    command.stdin(Stdio::null());
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    let child = command.spawn()?;
    #[cfg(target_os = "macos")]
    {
        serde_json::to_writer(&mut sender, configuration)?;
        sender.write_all(b"\n")?;
    }
    let main_pid = child.id();
    let flags = fcntl_getfl(stdin())?;
    fcntl_setfl(stdin(), flags | OFlags::NONBLOCK)?;
    let mut main_exited = false;
    loop {
        let event = child_event(wait(WaitOptions::NOHANG), main_pid, main_exited)?;
        if let ChildEvent::MainExit(message) = &event {
            notice(message)?;
            main_exited = true;
        }
        // Without a PID namespace, reaping every child does not prove the
        // process group finished; keep polling while members remain.
        #[cfg(target_os = "macos")]
        let event = if matches!(event, ChildEvent::Finished) && !group_finished()? {
            ChildEvent::Pending
        } else {
            event
        };
        match event {
            // Only a PID namespace init reaps other descendants; every reaped
            // child polls again immediately to drain further exits.
            ChildEvent::MainExit(_) | ChildEvent::Reaped => continue,
            ChildEvent::Finished => {
                notice(&Notice::Finished)?;
                // Keep the launcher/group identity alive until acknowledged.
                while !instruction(stdin())? {
                    std::thread::sleep(Duration::from_millis(5));
                }

                return Ok(());
            }
            ChildEvent::Pending => {}
        }
        if instruction(stdin())? {
            // Exiting namespace PID 1 makes the kernel kill and reap all its
            // remaining descendants before Bubblewrap observes its exit.
            #[cfg(not(target_os = "macos"))]
            return Ok(());
            #[cfg(target_os = "macos")]
            return Err(io::Error::other(
                "process group cancellation requires host cleanup",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

enum ChildEvent {
    MainExit(Notice),
    Reaped,
    Finished,
    Pending,
}

fn child_event(
    result: Result<Option<(Pid, WaitStatus)>, Errno>,
    main_pid: u32,
    main_exited: bool,
) -> io::Result<ChildEvent> {
    match result {
        Ok(Some((pid, status)))
            if u32::try_from(pid.as_raw_nonzero().get()).ok() == Some(main_pid) =>
        {
            Ok(ChildEvent::MainExit(Notice::MainExit {
                code: status.exit_status(),
                signal: status.terminating_signal(),
            }))
        }
        Ok(Some(_)) => Ok(ChildEvent::Reaped),
        Err(Errno::CHILD) if main_exited => Ok(ChildEvent::Finished),
        Ok(None) => Ok(ChildEvent::Pending),
        Err(error) => Err(error.into()),
    }
}

fn instruction(input: impl AsFd) -> io::Result<bool> {
    let mut byte = [0];
    match read(input, &mut byte) {
        Ok(_) => Ok(true),
        Err(Errno::AGAIN) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn notice(notice: &Notice) -> io::Result<()> {
    write_notice(notice, |bytes| write(stdin(), bytes).map_err(Into::into))
}

fn write_notice(
    notice: &Notice,
    mut send: impl FnMut(&[u8]) -> io::Result<usize>,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(notice)?;
    bytes.push(b'\n');
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        let written = send(remaining)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        remaining = &remaining[written..];
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn group_finished() -> io::Result<bool> {
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid::pidinfo;
    use libproc::processes::{ProcFilter, pids_by_type};

    let own = std::process::id();
    for pid in pids_by_type(ProcFilter::ByProgramGroup { pgrpid: own })? {
        if pid == own {
            continue;
        }
        let pid = i32::try_from(pid).map_err(io::Error::other)?;
        match pidinfo::<BSDInfo>(pid, 0) {
            // Darwin SZOMB. Zombies cannot execute or create descendants.
            Ok(info) if info.pbi_status == 5 => {}
            // The process may exit between enumeration and inspection. Retry
            // the snapshot; an unreadable live process never proves completion.
            _ => return Ok(false),
        }
    }

    Ok(true)
}

#[cfg(target_os = "macos")]
fn seatbelt(configuration: &Launch) -> io::Result<String> {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process-exec process-fork)\n(allow signal (target \
         same-sandbox))\n(allow file-read* file-write-data (literal \"/dev/null\"))\n(deny \
         file-link)",
    );
    for path in std::iter::once(&configuration.workspace)
        .chain(&configuration.external_reads)
        .chain(std::iter::once(&configuration.launcher))
    {
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF8 grant"))?;
        writeln!(
            profile,
            "(allow file-read* (subpath {}))",
            serde_json::to_string(path)?
        )
        .map_err(io::Error::other)?;
    }
    for path in &configuration.workspace_writes {
        let path: std::path::PathBuf = configuration.workspace.join(path).components().collect();
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF8 grant"))?;
        writeln!(
            profile,
            "(allow file-write* (subpath {}))",
            serde_json::to_string(path)?
        )
        .map_err(io::Error::other)?;
    }
    for path in &configuration.git_metadata {
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF8 metadata"))?;
        writeln!(
            profile,
            "(deny file-write* (subpath {}))",
            serde_json::to_string(path)?
        )
        .map_err(io::Error::other)?;
    }
    if configuration.host_information {
        profile
            .push_str("(allow sysctl-read file-read-metadata)\n(allow file-read* (literal \"/\"))");
    }

    profile.push_str("(deny file-write* (regex #\"(^|/)[.][gG][iI][tT](/|$)\"))");

    Ok(profile)
}

#[cfg(target_os = "linux")]
fn bubblewrap(configuration: &Launch) -> io::Result<()> {
    use std::io::Seek;
    use std::os::fd::AsRawFd;

    use rustix::fs::{MemfdFlags, memfd_create};
    use rustix::io::{FdFlags, fcntl_setfd};

    let bubblewrap = configuration
        .linux_bubblewrap
        .as_ref()
        .ok_or_else(|| io::Error::other("Bubblewrap required"))?;
    let mut filter = std::fs::File::from(memfd_create("ag-harness-seccomp", MemfdFlags::CLOEXEC)?);
    filter.write_all(&super::seccomp::filter(
        configuration.host_information,
        std::env::consts::ARCH,
    )?)?;
    filter.rewind()?;
    fcntl_setfd(&filter, FdFlags::empty())?;
    let mut command = Command::new(bubblewrap);
    command.env_clear().args([
        "--unshare-all",
        "--uid",
        "0",
        "--gid",
        "0",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--as-pid-1",
        "--tmpfs",
        "/",
    ]);
    for path in &configuration.external_reads {
        command.arg("--ro-bind").arg(path).arg(path);
    }
    command
        .arg("--ro-bind")
        .arg(&configuration.launcher)
        .arg(&configuration.launcher);
    command
        .arg("--ro-bind")
        .arg(&configuration.workspace)
        .arg(&configuration.workspace);
    for path in &configuration.workspace_writes {
        let path = configuration.workspace.join(path);
        command.arg("--bind").arg(&path).arg(&path);
    }
    for path in &configuration.git_metadata {
        if path.exists() && path.starts_with(&configuration.workspace) {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    command
        .args(["--dev-bind", "/dev/null", "/dev/null", "--seccomp"])
        .arg(filter.as_raw_fd().to_string());
    command
        .arg("--chdir")
        .arg(&configuration.workspace)
        .arg("--")
        .arg(&configuration.launcher)
        .arg("--namespace-init");

    Err(command.exec())
}

#[cfg(test)]
#[path = "launcher_test.rs"]
mod tests;
