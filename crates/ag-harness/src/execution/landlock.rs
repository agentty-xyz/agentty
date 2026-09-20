//! Landlock write enforcement for Linux workspace write grants.
//!
//! Bubblewrap mounts keep the workspace read-only and Git metadata that
//! exists at launch immutable, and mount boundaries reject cross-mount hard
//! links and renames on their own; Landlock additionally confines every
//! write of the shell and its descendants to the granted directories, so the
//! remaining mount-writable paths — the sandbox-private tmpfs root, even
//! through symlinks — stay unreachable and rename and link rights stay
//! scoped to each grant. Enforcement requires ABI 3 (Linux 6.2): earlier
//! kernels cannot restrict `truncate`, so write grants fail closed there.
//! The kernel API has no libc wrapper, so this module owns the workspace's
//! only unsafe blocks: three raw syscalls on plain values and descriptors
//! held in `OwnedFd`.

use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

#[cfg(target_os = "linux")]
use rustix::fs::{Mode, OFlags};

use super::contract::ExecutionError;
#[cfg(target_os = "linux")]
use super::wire::Launch;

/// `truncate` joined the handled rights in ABI 3; without it a command could
/// empty files it cannot otherwise write.
const REQUIRED_ABI: i64 = 3;

const ACCESS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_MAKE_REG: u64 = 1 << 8;
const ACCESS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_MAKE_SYM: u64 = 1 << 12;
const ACCESS_REFER: u64 = 1 << 13;
const ACCESS_TRUNCATE: u64 = 1 << 14;

/// Every write-type right up to the required ABI. Reads and execution stay
/// governed by the launcher mounts.
const GRANT_ACCESS: u64 = ACCESS_WRITE_FILE
    | ACCESS_REMOVE_DIR
    | ACCESS_REMOVE_FILE
    | ACCESS_MAKE_CHAR
    | ACCESS_MAKE_DIR
    | ACCESS_MAKE_REG
    | ACCESS_MAKE_SOCK
    | ACCESS_MAKE_FIFO
    | ACCESS_MAKE_BLOCK
    | ACCESS_MAKE_SYM
    | ACCESS_REFER
    | ACCESS_TRUNCATE;

/// Non-directory rights for the bound `/dev/null` device.
const DEVICE_ACCESS: u64 = ACCESS_WRITE_FILE | ACCESS_TRUNCATE;

#[cfg(target_os = "linux")]
const RULE_PATH_BENEATH: u32 = 1;

#[cfg(target_os = "linux")]
const CREATE_RULESET_VERSION: u32 = 1;

/// Fails closed before execution when write grants require Landlock
/// enforcement this kernel cannot provide.
pub(super) fn validate_write_grants(grants: usize, abi: i64) -> Result<(), ExecutionError> {
    if grants > 0 && !enforceable(abi) {
        return Err(ExecutionError::Unsupported);
    }

    Ok(())
}

/// Restricts the calling launcher thread — and through inheritance the shell
/// and all its descendants — to the granted write directories before any
/// untrusted code runs. Without grants the mounts already enforce the
/// read-only policy and no ruleset is installed.
#[cfg(target_os = "linux")]
pub(super) fn confine(configuration: &Launch) -> io::Result<()> {
    if configuration.workspace_writes.is_empty() {
        return Ok(());
    }
    let ruleset = ruleset(ruleset_access(abi())?)?;
    for (index, write) in configuration.workspace_writes.iter().enumerate() {
        let grant = rustix::fs::open(
            configuration.workspace.join(write),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let stat = rustix::fs::fstat(&grant)?;
        verify_grant_identity(
            configuration.workspace_write_nodes.get(index).copied(),
            (stat.st_dev, stat.st_ino),
        )?;
        add_rule(&ruleset, &grant, GRANT_ACCESS)?;
    }
    let device = rustix::fs::open("/dev/null", OFlags::PATH | OFlags::CLOEXEC, Mode::empty())?;
    add_rule(&ruleset, &device, DEVICE_ACCESS)?;
    rustix::thread::set_no_new_privs(true)?;

    restrict(&ruleset)
}

/// The validated identity of one write grant must match the directory
/// actually mounted at the grant path, so a path swapped for an alias between
/// host validation and launch cannot expose another hierarchy.
fn verify_grant_identity(expected: Option<(u64, u64)>, mounted: (u64, u64)) -> io::Result<()> {
    if expected != Some(mounted) {
        return Err(io::Error::other("write grant identity changed"));
    }

    Ok(())
}

fn ruleset_access(abi: i64) -> io::Result<u64> {
    if !enforceable(abi) {
        return Err(io::Error::other("Landlock enforcement unavailable"));
    }

    Ok(GRANT_ACCESS)
}

fn enforceable(abi: i64) -> bool {
    abi >= REQUIRED_ABI
}

/// Best-available Landlock ABI; a negative value reports unavailability.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "the Landlock kernel API has no libc wrapper")]
pub(super) fn abi() -> i64 {
    // SAFETY: the version probe passes no pointers and creates nothing.
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    }
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct RulesetAttribute {
    handled_access_fs: u64,
}

#[cfg(target_os = "linux")]
#[repr(C, packed)]
struct PathBeneathAttribute {
    allowed_access: u64,
    parent_fd: RawFd,
}

#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "the Landlock kernel API has no libc wrapper")]
fn ruleset(handled_access_fs: u64) -> io::Result<OwnedFd> {
    let attribute = RulesetAttribute { handled_access_fs };
    // SAFETY: the attribute outlives the call and its exact size is passed.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attribute,
            size_of::<RulesetAttribute>(),
            0u32,
        )
    };
    let fd = RawFd::try_from(fd).map_err(io::Error::other)?;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a nonnegative result is a newly created descriptor we own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "the Landlock kernel API has no libc wrapper")]
fn add_rule(ruleset: &OwnedFd, parent: &OwnedFd, allowed_access: u64) -> io::Result<()> {
    let attribute = PathBeneathAttribute {
        allowed_access,
        parent_fd: parent.as_raw_fd(),
    };
    // SAFETY: the attribute outlives the call; both descriptors stay open.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            RULE_PATH_BENEATH,
            &raw const attribute,
            0u32,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "the Landlock kernel API has no libc wrapper")]
fn restrict(ruleset: &OwnedFd) -> io::Result<()> {
    // SAFETY: applies the open ruleset descriptor to the calling thread.
    let result =
        unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(test)]
#[path = "landlock_test.rs"]
mod tests;
