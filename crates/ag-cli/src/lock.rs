use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::io::AsRawFd;

const LOCK_DIR: &str = "/var/tmp/.agentty";
const LOCK_PATH: &str = "/var/tmp/.agentty/lock";

pub enum LockError {
    AlreadyRunning { pid: String },
    Io(io::Error),
}

impl From<io::Error> for LockError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { pid } => {
                write!(f, "Another agentty session is already running (PID: {pid})")
            }
            Self::Io(err) => write!(f, "Failed to acquire session lock: {err}"),
        }
    }
}

/// Acquire an exclusive session lock.
///
/// Returns the lock file handle which must be kept alive for the entire process
/// lifetime. The OS releases the `flock` automatically on process exit or
/// crash.
pub fn acquire_lock() -> Result<File, LockError> {
    fs::create_dir_all(LOCK_DIR)?;

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(LOCK_PATH)?;

    // SAFETY: flock is a standard POSIX advisory lock syscall operating on a
    // valid file descriptor. No memory-safety concerns.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };

    if ret != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            let mut pid = String::new();
            let mut reader = &file;
            let _ = reader.read_to_string(&mut pid);
            return Err(LockError::AlreadyRunning {
                pid: pid.trim().to_string(),
            });
        }
        return Err(LockError::Io(err));
    }

    // Write our PID into the lock file
    file.set_len(0)?;
    file.seek(io::SeekFrom::Start(0))?;
    write!(&file, "{}", std::process::id())?;

    Ok(file)
}
