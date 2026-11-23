//! Trusted single-threaded launcher. Untrusted code starts only after
//! isolation.

#[cfg(target_os = "macos")]
use std::fmt::Write as _;
#[cfg(target_os = "linux")]
use std::io::Seek;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::AsFd;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

#[cfg(target_os = "macos")]
use libproc::bsd_info::BSDInfo;
#[cfg(target_os = "macos")]
use libproc::proc_pid::pidinfo;
#[cfg(target_os = "macos")]
use libproc::processes::{ProcFilter, pids_by_type};
#[cfg(target_os = "linux")]
use rustix::fs::{MemfdFlags, memfd_create};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, read, write};
#[cfg(target_os = "linux")]
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, WaitOptions, WaitStatus, getegid, geteuid, getgid, getuid, wait};
use rustix::stdio::stdin;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::contract::ReadScope;
use super::wire::{Launch, Notice};

/// First argument of every helper invocation: the harness starts the helper
/// with it, and the helper passes it again whenever it re-executes itself,
/// so a host executable can tell helper starts from its own.
pub(crate) const HELPER_ARGUMENT: &str = "--ag-harness-sandbox";

/// Returns whether `arguments`, a process's arguments including its program
/// name, start the sandbox helper.
pub(crate) fn is_helper(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> bool {
    arguments.nth(1).as_deref() == Some(std::ffi::OsStr::new(HELPER_ARGUMENT))
}

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
    // The configuration and the programs it names come from the caller, so
    // a set-ID helper would run them with privileges the caller lacks.
    (getuid() == geteuid() && getgid() == getegid())
        .then_some(())
        .ok_or(io::ErrorKind::PermissionDenied)?;
    // The phase follows the helper argument.
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(2).as_deref() == Some(std::ffi::OsStr::new("--seatbelt-child")) {
        let configuration = configuration()?;

        return Err(shell(&configuration).stdin(Stdio::null()).exec());
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().nth(2).as_deref() == Some(std::ffi::OsStr::new("--namespace-init")) {
        if rustix::process::getpid().as_raw_nonzero().get() != 1 {
            return Err(io::Error::other("not namespace init"));
        }
        notice(&Notice::Configure)?;
        let configuration = configuration()?;
        super::landlock::confine(&configuration)?;

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
            .args([HELPER_ARGUMENT, "--seatbelt-child"]);
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

/// Builds the Seatbelt profile for `configuration`. Seatbelt checks every
/// access against the resolved path, so links grant nothing beyond their
/// targets, and the last matching rule decides: Git metadata denials follow
/// the write grants, and the Git-writes grant reopens the repository's Git
/// directories after them, minus what Git would execute or follow.
#[cfg(target_os = "macos")]
fn seatbelt(configuration: &Launch) -> io::Result<String> {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process-exec process-fork)\n(allow signal (target \
         same-sandbox))\n(allow file-read* file-write-data (literal \"/dev/null\"))\n(allow \
         file-read* file-write-data (literal \"/dev/stdin\") (literal \"/dev/stdout\") (literal \
         \"/dev/stderr\") (subpath \"/dev/fd\"))\n(deny file-link)\n",
    );
    if configuration.read_scope == ReadScope::FullDisk {
        profile.push_str("(allow file-read*)\n");
    }
    for path in std::iter::once(&configuration.workspace)
        .chain(&configuration.external_reads)
        .chain(std::iter::once(&configuration.launcher))
    {
        rule(&mut profile, "allow file-read*", path)?;
    }
    if configuration.host_information {
        profile.push_str(
            "(allow sysctl-read file-read-metadata)\n(allow file-read* (literal \"/\"))\n",
        );
    }
    // Hidden paths override every read rule above, and the writable
    // directories below stay accessible inside them.
    for path in &configuration.hidden_paths {
        rule(&mut profile, "deny file-read* file-write*", path)?;
    }
    for path in &configuration.writes {
        rule(&mut profile, "allow file-read* file-write*", path)?;
    }
    for path in &configuration.git_metadata {
        rule(&mut profile, "deny file-write*", path)?;
    }
    profile.push_str("(deny file-write* (regex #\"(^|/)[.][gG][iI][tT](/|$)\"))\n");
    if let Some(git) = &configuration.git_writes {
        for directory in git.admin.iter().chain(std::iter::once(&git.common)) {
            rule(&mut profile, "allow file-write*", directory)?;
        }
        writeln!(
            profile,
            "(deny file-write* (regex \
             #\"^{}/((config|hooks|modules|{GIT_HISTORY_PROTECTED}|{GIT_WORKTREE_PROTECTED})|\
             worktrees/[^/]+/({GIT_WORKTREE_PROTECTED}))(/|$)\"))",
            pattern(&git.common)?
        )
        .map_err(io::Error::other)?;
        // Path rules follow names, so the directories holding protected
        // entries must not move either: a command could otherwise move one
        // aside, change what it holds, and move it back.
        writeln!(
            profile,
            "(deny file-write* (regex #\"^{}(/worktrees(/[^/]+)?)?$\"))",
            pattern(&git.common)?
        )
        .map_err(io::Error::other)?;
        if let Some(admin) = &git.admin {
            writeln!(
                profile,
                "(deny file-write* (regex #\"^{admin}/({GIT_WORKTREE_PROTECTED})(/|$)\"))\n(deny \
                 file-write* (regex #\"^{admin}$\"))",
                admin = pattern(admin)?
            )
            .map_err(io::Error::other)?;
        }
    }
    if configuration.network {
        profile.push_str(SEATBELT_NETWORK);
    }
    // Seatbelt matches a socket's resolved path, which preparation supplies.
    for socket in &configuration.unix_sockets {
        let socket = socket
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF8 socket"))?;
        writeln!(
            profile,
            "(allow network-outbound (remote unix-socket (path-literal {})))",
            serde_json::to_string(socket)?
        )
        .map_err(io::Error::other)?;
    }

    Ok(profile)
}

/// Entries of a worktree's Git administration that stay read-only under the
/// Git-writes grant, as a Seatbelt pattern: Git follows `commondir` and
/// `gitdir` to find the repository, reads `config.worktree` as
/// configuration, and resumes a rebase or cherry-pick from its state
/// directories, whose to-do lists can run commands.
#[cfg(target_os = "macos")]
const GIT_WORKTREE_PROTECTED: &str =
    "config[.]worktree|commondir|gitdir|rebase-merge|rebase-apply|sequencer";

/// Entries inside the common Git directory's writable stores that stay
/// read-only under the Git-writes grant on Linux, and whether each is a
/// directory: replacement refs and object alternates change the history Git
/// run later outside the sandbox shows without changing the objects it
/// pushes. A missing one is created empty as the mount point, which Git
/// treats as absent.
#[cfg(target_os = "linux")]
const GIT_HISTORY_PROTECTED: [(&str, bool); 3] = [
    ("refs/replace", true),
    ("objects/info/alternates", false),
    ("objects/info/http-alternates", false),
];

/// Entries of the common Git directory that stay read-only under the
/// Git-writes grant, as a Seatbelt pattern, because they change the history
/// Git run later outside the sandbox shows without changing the objects it
/// pushes: replacement refs, grafts and other `info` files, the shallow
/// boundary, and object alternates.
#[cfg(target_os = "macos")]
const GIT_HISTORY_PROTECTED: &str = "info|shallow|refs/replace|objects/info/(http-)?alternates";

/// Network rules of the network grant: IP connections and listeners, the
/// system resolver, and the services that resolve names and verify TLS
/// certificates, including the trust evaluation agent Apple's `LibreSSL`,
/// and so `/usr/bin/curl`, falls back to. The keychain service
/// (`com.apple.SecurityServer`) stays denied, so commands cannot fetch
/// credentials such as Git's or the GitHub CLI's from the keychain. Other
/// Unix-domain sockets stay denied unless granted by path.
#[cfg(target_os = "macos")]
const SEATBELT_NETWORK: &str =
    "(allow system-socket)\n(allow network-outbound (remote ip))\n(allow network-bind \
     network-inbound (local ip))\n(allow network-outbound (remote unix-socket (path-literal \
     \"/private/var/run/mDNSResponder\")))\n(allow mach-lookup (global-name \
     \"com.apple.dnssd.service\") (global-name \
     \"com.apple.SystemConfiguration.DNSConfiguration\") (global-name \
     \"com.apple.SystemConfiguration.configd\") (global-name \"com.apple.trustd.agent\") \
     (global-name \"com.apple.ocspd\") (global-name \"com.apple.TrustEvaluationAgent\"))\n";

/// Appends `(<action> (subpath "<path>"))` to `profile`.
#[cfg(target_os = "macos")]
fn rule(profile: &mut String, action: &str, path: &std::path::Path) -> io::Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF8 grant"))?;

    writeln!(
        profile,
        "({action} (subpath {}))",
        serde_json::to_string(path)?
    )
    .map_err(io::Error::other)
}

/// Returns `path` as a Seatbelt regular expression matching it literally.
#[cfg(target_os = "macos")]
fn pattern(path: &std::path::Path) -> io::Result<String> {
    let path = path
        .to_str()
        .filter(|path| !path.contains(['"', '\n']))
        .ok_or_else(|| io::Error::other("unsupported Git directory"))?;

    Ok(path
        .chars()
        .map(|character| match character {
            '.' | '*' | '+' | '?' | '(' | ')' | '{' | '}' | '|' | '$' => format!("[{character}]"),
            // `[`, `\`, `]`, and `^`.
            '['..='^' => format!("\\{character}"),
            other => other.to_string(),
        })
        .collect())
}

#[cfg(target_os = "linux")]
fn bubblewrap(configuration: &Launch) -> io::Result<()> {
    let bubblewrap = configuration
        .linux_bubblewrap
        .as_ref()
        .ok_or_else(|| io::Error::other("Bubblewrap required"))?;
    let mut filter = std::fs::File::from(memfd_create("ag-harness-seccomp", MemfdFlags::CLOEXEC)?);
    filter.write_all(&super::seccomp::filter(
        configuration.host_information,
        configuration.network,
        std::env::consts::ARCH,
    )?)?;
    filter.rewind()?;
    fcntl_setfd(&filter, FdFlags::empty())?;
    let mut command = Command::new(bubblewrap);
    command.env_clear().arg("--unshare-all");
    if configuration.network {
        command.arg("--share-net");
    }
    command.args([
        "--uid",
        "0",
        "--gid",
        "0",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--as-pid-1",
    ]);
    let full_disk = configuration.read_scope == ReadScope::FullDisk;
    if full_disk {
        command.args(["--ro-bind", "/", "/"]);
    } else {
        command.args(["--tmpfs", "/"]);
    }
    let devices = configuration.system || full_disk;
    if devices {
        command.args(["--proc", "/proc", "--dev", "/dev"]);
    }
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
    // Hidden directories cover the read mounts above, and the writable
    // mounts below stay accessible inside them: an empty directory, made
    // read-only once those are mounted. Hidden files get the empty device
    // last. A missing path cannot appear, since none lies inside a writable
    // directory.
    let (hidden_directories, hidden_files): (Vec<_>, Vec<_>) = configuration
        .hidden_paths
        .iter()
        .filter(|path| path.exists())
        .partition(|path| path.is_dir());
    for path in &hidden_directories {
        command.arg("--tmpfs").arg(path);
    }
    for path in &configuration.writes {
        command.arg("--bind").arg(path).arg(path);
    }
    for path in &configuration.git_metadata {
        if path.exists() && path.starts_with(&configuration.workspace) {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    if let Some(git) = &configuration.git_writes {
        // Landlock confines these writable mounts to the object, reference,
        // and log stores and to files at the top of the worktree's own
        // administration directory, whose links stay mounted read-only.
        command.arg("--bind").arg(&git.common).arg(&git.common);
        // Replacement refs and object alternates lie inside the writable
        // stores but change the history Git run outside the sandbox shows,
        // so they stay read-only: the existing ones mounted over themselves,
        // missing ones covered by an empty read-only directory or file.
        // Grafts and the shallow boundary lie outside the stores already.
        for (relative, is_directory) in GIT_HISTORY_PROTECTED {
            let path = git.common.join(relative);
            if path.exists() {
                command.arg("--ro-bind").arg(&path).arg(&path);
            } else if is_directory {
                command
                    .arg("--tmpfs")
                    .arg(&path)
                    .arg("--remount-ro")
                    .arg(&path);
            } else {
                command.arg("--ro-bind").arg("/dev/null").arg(&path);
            }
        }
        if let Some(admin) = &git.admin {
            command.arg("--bind").arg(admin).arg(admin);
            for name in super::landlock::GIT_ADMIN_PROTECTED {
                let path = admin.join(name);
                command.arg("--ro-bind-try").arg(&path).arg(&path);
            }
        }
    }
    for path in hidden_directories {
        command.arg("--remount-ro").arg(path);
    }
    for path in hidden_files {
        command.arg("--ro-bind").arg("/dev/null").arg(path);
    }
    if !devices {
        command.args(["--dev-bind", "/dev/null", "/dev/null"]);
    }
    command.arg("--seccomp").arg(filter.as_raw_fd().to_string());
    command
        .arg("--chdir")
        .arg(&configuration.workspace)
        .arg("--")
        .arg(&configuration.launcher)
        .args([HELPER_ARGUMENT, "--namespace-init"]);

    Err(command.exec())
}

#[cfg(test)]
#[path = "launcher_test.rs"]
mod tests;
