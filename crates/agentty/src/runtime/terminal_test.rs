use std::ffi::OsString;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::Command;

use super::{
    DisableXtermCsiUModifiedKeys, EnableXtermCsiUModifiedKeys, MockTerminalOperation,
    TerminalEnhancement, TerminalGuard, has_ssh_environment,
    prepare_terminal_stdout_with_operation, restore_terminal_state,
};

/// Builds one expected terminal enhancement configuration.
fn enhancement_fixture(keyboard: bool, xterm: bool) -> TerminalEnhancement {
    TerminalEnhancement {
        keyboard_enhancement_enabled: keyboard,
        xterm_modified_keys_enabled: xterm,
    }
}

/// Verifies setup returns raw-mode failures directly.
#[test]
fn setup_terminal_returns_error_when_enable_raw_mode_fails() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Err(io::Error::other("enable failed")));
    operation.expect_enter_alternate_screen().times(0);
    operation.expect_supports_keyboard_enhancement().times(0);

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let error = result.expect_err("setup should fail when raw mode fails");
    assert_eq!(error.to_string(), "enable failed");
}

/// Verifies setup returns alternate-screen failures directly.
#[test]
fn setup_terminal_returns_error_when_enter_alternate_screen_fails() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Ok(false));
    operation.expect_is_ssh_session().once().returning(|| false);
    operation
        .expect_is_tmux_session()
        .once()
        .returning(|| false);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(false, false))
        .returning(|_, _| Err(io::Error::other("enter failed")));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let error = result.expect_err("setup should fail when alternate screen fails");
    assert_eq!(error.to_string(), "enter failed");
}

/// Verifies direct-terminal startup omits xterm modified-key reporting,
/// which would swallow shifted punctuation such as `@`.
#[test]
fn setup_terminal_omits_xterm_modified_keys_when_supported_directly() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    let mut xterm_startup_sequence = String::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Ok(true));
    operation
        .expect_is_tmux_session()
        .once()
        .returning(|| false);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(true, false))
        .returning(|_, _| Ok(()));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);
    EnableXtermCsiUModifiedKeys(guard.enhancement())
        .write_ansi(&mut xterm_startup_sequence)
        .expect("xterm startup sequence should render");

    // Assert
    let _stdout = result.expect("setup should use Kitty enhancement directly");
    assert_eq!(guard.enhancement(), enhancement_fixture(true, false));
    assert_eq!(xterm_startup_sequence, "");
}

/// Verifies support-query failures fall back to the legacy key mode so TUI
/// startup still succeeds.
#[test]
fn setup_terminal_ignores_keyboard_enhancement_query_failures() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Err(io::Error::other("unsupported")));
    operation.expect_is_ssh_session().once().returning(|| false);
    operation
        .expect_is_tmux_session()
        .once()
        .returning(|| false);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(false, false))
        .returning(|_, _| Ok(()));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let _stdout = result.expect("setup should fall back when support query fails");
    assert_eq!(guard.enhancement(), enhancement_fixture(false, false));
}

/// Verifies tmux sessions optimistically enable keyboard enhancement when
/// the support query is hidden by the pane transport.
#[test]
fn setup_terminal_enables_keyboard_enhancement_for_tmux_query_failure() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Err(io::Error::other("timeout")));
    operation.expect_is_ssh_session().once().returning(|| false);
    operation.expect_is_tmux_session().once().returning(|| true);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(true, true))
        .returning(|_, _| Ok(()));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let _stdout = result.expect("setup should enable keyboard enhancement inside tmux");
    assert_eq!(guard.enhancement(), enhancement_fixture(true, true));
}

/// Verifies tmux sessions optimistically enable keyboard enhancement even
/// when the support query returns a negative capability signal.
#[test]
fn setup_terminal_enables_keyboard_enhancement_for_tmux_unsupported_query() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Ok(false));
    operation.expect_is_ssh_session().once().returning(|| false);
    operation.expect_is_tmux_session().once().returning(|| true);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(true, true))
        .returning(|_, _| Ok(()));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let _stdout = result.expect("setup should enable keyboard enhancement inside tmux");
    assert_eq!(guard.enhancement(), enhancement_fixture(true, true));
}

/// Verifies SSH sessions optimistically enable keyboard enhancement when
/// the support query is hidden by the remote transport.
#[test]
fn setup_terminal_enables_keyboard_enhancement_for_ssh_query_failure() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Err(io::Error::other("timeout")));
    operation
        .expect_is_tmux_session()
        .once()
        .returning(|| false);
    operation.expect_is_ssh_session().once().returning(|| true);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(true, false))
        .returning(|_, _| Ok(()));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let _stdout = result.expect("setup should enable keyboard enhancement over SSH");
    assert_eq!(guard.enhancement(), enhancement_fixture(true, false));
}

/// Verifies SSH sessions optimistically enable keyboard enhancement even
/// when the support query returns a negative capability signal.
#[test]
fn setup_terminal_enables_keyboard_enhancement_for_ssh_unsupported_query() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let guard = TerminalGuard::new();
    operation
        .expect_enable_raw_mode()
        .once()
        .returning(|| Ok(()));
    operation
        .expect_supports_keyboard_enhancement()
        .once()
        .returning(|| Ok(false));
    operation
        .expect_is_tmux_session()
        .once()
        .returning(|| false);
    operation.expect_is_ssh_session().once().returning(|| true);
    operation
        .expect_enter_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(true, false))
        .returning(|_, _| Ok(()));

    // Act
    let result = prepare_terminal_stdout_with_operation(&operation, &guard);

    // Assert
    let _stdout = result.expect("setup should enable keyboard enhancement over SSH");
    assert_eq!(guard.enhancement(), enhancement_fixture(true, false));
}

/// Verifies SSH detection accepts the environment variables set by common
/// OpenSSH server configurations.
#[test]
fn ssh_environment_detects_common_ssh_variables() {
    // Arrange
    let variables = [
        ("SSH_CONNECTION", Some("client server")),
        ("SSH_CLIENT", None),
        ("SSH_TTY", None),
    ];

    // Act
    let is_ssh_session = has_ssh_environment(|name| {
        variables
            .iter()
            .find(|(variable_name, _)| *variable_name == name)
            .and_then(|(_, value)| value.map(OsString::from))
    });

    // Assert
    assert!(is_ssh_session);
}

/// Verifies SSH detection stays false when no common SSH variable is set.
#[test]
fn ssh_environment_rejects_local_terminal_without_ssh_variables() {
    // Arrange & Act
    let is_ssh_session = has_ssh_environment(|_| None);

    // Assert
    assert!(!is_ssh_session);
}

/// Verifies the xterm modified-key commands request CSI-u encoding and
/// reset the modified-key resources during terminal restore.
#[test]
fn xterm_modified_key_commands_request_and_reset_csi_u_reporting() {
    // Arrange
    let mut disabled_enable_sequence = String::new();
    let mut disabled_disable_sequence = String::new();
    let mut enable_sequence = String::new();
    let mut disable_sequence = String::new();

    // Act
    EnableXtermCsiUModifiedKeys(enhancement_fixture(true, false))
        .write_ansi(&mut disabled_enable_sequence)
        .expect("disabled enable sequence should render");
    DisableXtermCsiUModifiedKeys(enhancement_fixture(true, false))
        .write_ansi(&mut disabled_disable_sequence)
        .expect("disabled disable sequence should render");
    EnableXtermCsiUModifiedKeys(enhancement_fixture(true, true))
        .write_ansi(&mut enable_sequence)
        .expect("enable sequence should render");
    DisableXtermCsiUModifiedKeys(enhancement_fixture(true, true))
        .write_ansi(&mut disable_sequence)
        .expect("disable sequence should render");

    // Assert
    assert_eq!(disabled_enable_sequence, "");
    assert_eq!(disabled_disable_sequence, "");
    assert_eq!(enable_sequence, "\x1B[>4;1f\x1B[>4;2m");
    assert_eq!(disable_sequence, "\x1B[>4f\x1B[>4m");
}

/// Verifies restore still attempts alternate-screen cleanup when raw-mode
/// cleanup fails.
#[test]
fn restore_terminal_state_attempts_leave_even_when_disable_fails() {
    // Arrange
    let mut operation = MockTerminalOperation::new();
    let leave_calls = Arc::new(AtomicUsize::new(0));
    let leave_calls_for_expectation = leave_calls.clone();
    operation
        .expect_disable_raw_mode()
        .once()
        .returning(|| Err(io::Error::other("disable failed")));
    operation
        .expect_leave_alternate_screen()
        .once()
        .withf(|_, enhancement| *enhancement == enhancement_fixture(true, true))
        .returning(move |_, _| {
            leave_calls_for_expectation.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });

    // Act
    restore_terminal_state(&operation, enhancement_fixture(true, true));

    // Assert
    assert_eq!(leave_calls.load(Ordering::Relaxed), 1);
}
