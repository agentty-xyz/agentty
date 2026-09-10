use super::{
    MockTerminationClient, TerminationOutcome, log_rejected_termination, send_terminate_signal,
    send_terminate_signal_with,
};

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
    // Arrange
    let client = MockTerminationClient::new();
    let overflow_pid = u32::MAX;

    // Act
    let outcome = send_terminate_signal_with(&client, overflow_pid);

    // Assert
    assert_eq!(outcome, TerminationOutcome::InvalidPid);
}

#[test]
fn test_send_terminate_signal_ignores_zero_pid() {
    // Arrange
    let client = MockTerminationClient::new();

    // Act
    let outcome = send_terminate_signal_with(&client, 0);

    // Assert
    assert_eq!(outcome, TerminationOutcome::InvalidPid);
}

#[test]
fn test_send_terminate_signal_reports_rejected_signal() {
    // Arrange
    let mut client = MockTerminationClient::new();
    client
        .expect_terminate()
        .times(1)
        .returning(|_| Err(rustix::io::Errno::PERM));

    // Act
    let outcome = send_terminate_signal_with(&client, 1);

    // Assert
    assert_eq!(
        outcome,
        TerminationOutcome::Rejected(rustix::io::Errno::PERM)
    );
}

#[test]
fn test_rejected_termination_is_logged_without_propagating() {
    // Arrange
    let outcome = TerminationOutcome::Rejected(rustix::io::Errno::PERM);

    // Act
    let was_logged = log_rejected_termination(1, &outcome);

    // Assert
    assert!(was_logged);
}
