//! Process-management utilities for agent subprocess lifecycle.

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

/// Sends `SIGTERM` to the process identified by `pid`.
///
/// Best-effort: failures (no such process, permission denied) are silently
/// ignored because the calling code treats process termination as advisory.
/// Uses `nix::sys::signal::kill` for a direct syscall instead of shelling
/// out to the `kill` binary.
pub(crate) fn send_terminate_signal(pid: u32) {
    if let Ok(raw_pid) = i32::try_from(pid) {
        let _ = signal::kill(Pid::from_raw(raw_pid), Signal::SIGTERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_terminate_signal_kills_owned_child() {
        // Arrange — spawn a long-running child whose PID we control.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep");
        let child_pid = child.id();

        // Act
        send_terminate_signal(child_pid);

        // Assert — the child should have been terminated by SIGTERM.
        let exit_status = child.wait().expect("failed to wait on child");
        assert!(
            !exit_status.success(),
            "child should have been killed, not exited normally"
        );
    }

    #[test]
    fn test_send_terminate_signal_ignores_overflow_pid() {
        // Arrange — PID exceeding i32::MAX cannot be converted and is
        // silently skipped.
        let overflow_pid = u32::MAX;

        // Act
        send_terminate_signal(overflow_pid);

        // Assert — no panic, no signal sent.
    }
}
