//! Process-management utilities for agent subprocess lifecycle.

use rustix::process::{self, Pid, Signal};
use tracing::warn;

/// Result of one best-effort process-termination request.
#[derive(Debug, Eq, PartialEq)]
enum TerminationOutcome {
    /// The PID could not be represented by the host process API.
    InvalidPid,
    /// The operating system rejected the termination signal.
    Rejected(rustix::io::Errno),
    /// `SIGTERM` was delivered to the process.
    Sent,
}

/// Capability boundary for delivering one termination signal.
#[cfg_attr(test, mockall::automock)]
trait TerminationClient: Send + Sync {
    /// Sends `SIGTERM` to one validated host PID.
    fn terminate(&self, pid: Pid) -> rustix::io::Result<()>;
}

/// Production termination client backed by a direct host syscall.
struct RealTerminationClient;

impl TerminationClient for RealTerminationClient {
    fn terminate(&self, pid: Pid) -> rustix::io::Result<()> {
        process::kill_process(pid, Signal::TERM)
    }
}

/// Sends `SIGTERM` to the process identified by `pid`.
///
/// Best-effort: the calling workflow remains advisory, but rejected signals
/// are logged with their PID and operating-system error. Uses a direct syscall
/// instead of shelling out to the `kill` binary.
pub(crate) fn send_terminate_signal(pid: u32) {
    let outcome = send_terminate_signal_with(&RealTerminationClient, pid);
    log_rejected_termination(pid, &outcome);
}

/// Logs rejected termination requests and reports whether logging occurred.
fn log_rejected_termination(pid: u32, outcome: &TerminationOutcome) -> bool {
    if let TerminationOutcome::Rejected(error) = outcome {
        warn!(pid, %error, "Failed to send process termination signal");

        return true;
    }

    false
}

/// Sends one termination signal through the injected capability boundary.
fn send_terminate_signal_with(client: &dyn TerminationClient, pid: u32) -> TerminationOutcome {
    let Ok(raw_pid) = i32::try_from(pid) else {
        return TerminationOutcome::InvalidPid;
    };
    let Some(pid) = Pid::from_raw(raw_pid) else {
        return TerminationOutcome::InvalidPid;
    };

    match client.terminate(pid) {
        Ok(()) => TerminationOutcome::Sent,
        Err(error) => TerminationOutcome::Rejected(error),
    }
}

#[cfg(test)]
#[path = "process_test.rs"]
mod tests;
