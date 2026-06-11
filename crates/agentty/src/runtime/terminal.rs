use std::cell::Cell;
use std::ffi::OsString;
use std::{env, fmt, io};

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{Command, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::runtime::TuiTerminal;

/// Abstraction over terminal transitions so setup/restore paths can be tested
/// without touching real terminal state.
#[cfg_attr(test, mockall::automock)]
trait TerminalOperation {
    /// Enables terminal raw mode before entering the alternate screen.
    fn enable_raw_mode(&self) -> io::Result<()>;

    /// Disables terminal raw mode during cleanup.
    fn disable_raw_mode(&self) -> io::Result<()>;

    /// Returns whether the active terminal supports keyboard enhancement
    /// flags for reporting modified keys like `Alt+Enter`.
    fn supports_keyboard_enhancement(&self) -> io::Result<bool>;

    /// Returns whether the app is running through an SSH transport.
    ///
    /// SSH can hide terminal capability responses even when the outer
    /// terminal will honor keyboard enhancement escape sequences.
    fn is_ssh_session(&self) -> bool;

    /// Returns whether the app is running inside a `tmux` pane.
    ///
    /// `tmux` can hide terminal keyboard capability responses from the pane
    /// while still honoring explicit modified-key reporting requests.
    fn is_tmux_session(&self) -> bool;

    /// Enters the alternate screen and enables bracketed paste, optionally
    /// enabling keyboard enhancement flags first.
    fn enter_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()>;

    /// Leaves the alternate screen, disables bracketed paste, and restores the
    /// terminal cursor, optionally popping keyboard enhancement flags first.
    fn leave_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()>;
}

/// Production terminal operations backed by `crossterm`.
struct CrosstermTerminalOperation;

impl TerminalOperation for CrosstermTerminalOperation {
    fn enable_raw_mode(&self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn supports_keyboard_enhancement(&self) -> io::Result<bool> {
        supports_keyboard_enhancement()
    }

    fn is_ssh_session(&self) -> bool {
        has_ssh_environment(|name| env::var_os(name))
    }

    fn is_tmux_session(&self) -> bool {
        has_tmux_environment(|name| env::var_os(name))
    }

    fn enter_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()> {
        if keyboard_enhancement_enabled {
            execute!(
                stdout,
                EnableXtermCsiUModifiedKeys,
                PushKeyboardEnhancementFlags(keyboard_enhancement_flags()),
                EnterAlternateScreen,
                EnableBracketedPaste
            )
        } else {
            execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
        }
    }

    fn leave_alternate_screen(
        &self,
        stdout: &mut io::Stdout,
        keyboard_enhancement_enabled: bool,
    ) -> io::Result<()> {
        if keyboard_enhancement_enabled {
            execute!(
                stdout,
                PopKeyboardEnhancementFlags,
                DisableXtermCsiUModifiedKeys,
                DisableBracketedPaste,
                LeaveAlternateScreen,
                Show
            )
        } else {
            execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, Show)
        }
    }
}

/// Shared production terminal operation implementation.
static CROSSTERM_TERMINAL_OPERATION: CrosstermTerminalOperation = CrosstermTerminalOperation;

/// Requests xterm/tmux modified-key reporting in CSI-u format.
///
/// `tmux` listens for xterm's `modifyOtherKeys` controls when deciding
/// whether a pane application asked for extended keys. Crossterm's kitty
/// keyboard-protocol push is still used for terminals that support the kitty
/// stack, but the xterm request covers multiplexers that translate modified
/// `Enter` through xterm-compatible controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableXtermCsiUModifiedKeys;

impl Command for EnableXtermCsiUModifiedKeys {
    fn write_ansi(&self, buffer: &mut impl fmt::Write) -> fmt::Result {
        buffer.write_str("\x1B[>4;1f\x1B[>4;2m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Restores xterm/tmux modified-key reporting resources to their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableXtermCsiUModifiedKeys;

impl Command for DisableXtermCsiUModifiedKeys {
    fn write_ansi(&self, buffer: &mut impl fmt::Write) -> fmt::Result {
        buffer.write_str("\x1B[>4f\x1B[>4m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Returns the keyboard enhancement flag set used to disambiguate modified key
/// presses in terminals that support the kitty keyboard protocol without
/// requesting key release/repeat event streams.
///
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` is intentionally omitted: it forces every
/// key (including plain `Enter`) to be reported as a `CSI u` sequence, which
/// has been observed to fail under some `tmux` builds on Linux when the outer
/// terminal is `ghostty`. The resulting Shift+Enter sequence is dropped before
/// reaching the prompt input, while peer TUIs that stay on the legacy mode
/// keep working. `DISAMBIGUATE_ESCAPE_CODES` is sufficient to encode the
/// `Shift` modifier on `Enter` while leaving plain `Enter` on the universally
/// reliable legacy `\r` byte path.
const fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS)
}

/// Compile-time regression check that locks in the omission of
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` from the kitty keyboard enhancement flag
/// set. Re-adding it would break plain `Enter` under some `tmux` builds on
/// Linux when the outer terminal is `ghostty`, dropping `Shift+Enter`
/// sequences before they reach the prompt input. A `const` assertion is used
/// instead of a `#[test]` so the check runs without enlarging the
/// `agentty` libtest descriptor table.
const _: () = {
    assert!(
        keyboard_enhancement_flags().contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    );
    assert!(keyboard_enhancement_flags().contains(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS),);
    assert!(
        !keyboard_enhancement_flags()
            .contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES),
    );
};

/// Restores terminal state on all exit paths after raw mode is enabled.
///
/// The app uses `?` extensively inside the event loop and setup flow. Without
/// this guard, any early return after entering raw mode and the alternate
/// screen can leave the user's shell in a broken state.
///
/// Keeping cleanup in `Drop` guarantees restore runs during normal exit,
/// runtime errors, and unwinding panics. The guard is intentionally
/// thread-affine: setup mutates its state before the runtime loop starts and
/// cleanup runs from the same task via `Drop`.
pub(crate) struct TerminalGuard {
    keyboard_enhancement_enabled: Cell<bool>,
}

impl TerminalGuard {
    /// Creates a guard that restores terminal state for the active TUI session.
    pub(crate) fn new() -> Self {
        Self {
            keyboard_enhancement_enabled: Cell::new(false),
        }
    }

    /// Records whether setup enabled keyboard enhancement flags so cleanup can
    /// pop them symmetrically.
    fn set_keyboard_enhancement_enabled(&self, enabled: bool) {
        self.keyboard_enhancement_enabled.set(enabled);
    }

    /// Returns whether cleanup must pop keyboard enhancement flags before
    /// leaving the alternate screen.
    fn keyboard_enhancement_enabled(&self) -> bool {
        self.keyboard_enhancement_enabled.get()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(
            &CROSSTERM_TERMINAL_OPERATION,
            self.keyboard_enhancement_enabled(),
        );
    }
}

/// Enables raw mode, enters the alternate screen, and turns on bracketed paste
/// so multiline clipboard content arrives as `Event::Paste`.
///
/// When supported, keyboard enhancement flags are also enabled so modified
/// Enter keys remain distinguishable over transports like SSH and `tmux`.
pub(crate) fn setup_terminal(guard: &TerminalGuard) -> io::Result<TuiTerminal> {
    let stdout = prepare_terminal_stdout_with_operation(&CROSSTERM_TERMINAL_OPERATION, guard)?;
    let backend = CrosstermBackend::new(stdout);

    Terminal::new(backend)
}

/// Enables terminal modes with the supplied operation provider and returns the
/// configured stdout handle for later terminal construction.
fn prepare_terminal_stdout_with_operation(
    operation: &dyn TerminalOperation,
    guard: &TerminalGuard,
) -> io::Result<io::Stdout> {
    operation.enable_raw_mode()?;

    let keyboard_enhancement_enabled = should_enable_keyboard_enhancement(operation);
    guard.set_keyboard_enhancement_enabled(keyboard_enhancement_enabled);

    let mut stdout = io::stdout();
    operation.enter_alternate_screen(&mut stdout, keyboard_enhancement_enabled)?;

    Ok(stdout)
}

/// Returns whether setup should push keyboard enhancement flags.
///
/// Crossterm's support query is the preferred signal for local terminals. Over
/// SSH, the query can fail or report unsupported when the outer terminal still
/// honors the enhancement sequence, so remote sessions optimistically enable
/// it to keep modified `Enter` keys distinguishable. `tmux` panes get the same
/// optimistic path because the multiplexer can answer capability probes
/// differently from the terminal attached outside the pane.
fn should_enable_keyboard_enhancement(operation: &dyn TerminalOperation) -> bool {
    match operation.supports_keyboard_enhancement() {
        Ok(true) => true,
        Ok(false) | Err(_) => operation.is_ssh_session() || operation.is_tmux_session(),
    }
}

/// Returns whether common SSH environment variables are present.
fn has_ssh_environment(mut get_var: impl FnMut(&str) -> Option<OsString>) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|name| get_var(name).is_some())
}

/// Returns whether the `TMUX` pane environment variable is present.
fn has_tmux_environment(mut get_var: impl FnMut(&str) -> Option<OsString>) -> bool {
    get_var("TMUX").is_some()
}

/// Restores terminal modes and ignores failures so drop paths do not panic.
fn restore_terminal_state(operation: &dyn TerminalOperation, keyboard_enhancement_enabled: bool) {
    let mut stdout = io::stdout();
    // Best-effort: terminal may already be in normal state.
    let _ = operation.disable_raw_mode();
    let _ = operation.leave_alternate_screen(&mut stdout, keyboard_enhancement_enabled);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

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
            .withf(|_, keyboard_enhancement_enabled| !keyboard_enhancement_enabled)
            .returning(|_, _| Err(io::Error::other("enter failed")));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let error = result.expect_err("setup should fail when alternate screen fails");
        assert_eq!(error.to_string(), "enter failed");
    }

    /// Verifies setup enables keyboard enhancement when the terminal reports
    /// support for the protocol.
    #[test]
    fn setup_terminal_enables_keyboard_enhancement_when_supported() {
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
            .returning(|| Ok(true));
        operation
            .expect_enter_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let _stdout = result.expect("setup should succeed when keyboard enhancement is supported");
        assert!(guard.keyboard_enhancement_enabled());
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
            .withf(|_, keyboard_enhancement_enabled| !keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let _stdout = result.expect("setup should fall back when support query fails");
        assert!(!guard.keyboard_enhancement_enabled());
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
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let _stdout = result.expect("setup should enable keyboard enhancement inside tmux");
        assert!(guard.keyboard_enhancement_enabled());
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
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let _stdout = result.expect("setup should enable keyboard enhancement inside tmux");
        assert!(guard.keyboard_enhancement_enabled());
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
        operation.expect_is_ssh_session().once().returning(|| true);
        operation
            .expect_enter_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let _stdout = result.expect("setup should enable keyboard enhancement over SSH");
        assert!(guard.keyboard_enhancement_enabled());
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
        operation.expect_is_ssh_session().once().returning(|| true);
        operation
            .expect_enter_alternate_screen()
            .once()
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(|_, _| Ok(()));

        // Act
        let result = prepare_terminal_stdout_with_operation(&operation, &guard);

        // Assert
        let _stdout = result.expect("setup should enable keyboard enhancement over SSH");
        assert!(guard.keyboard_enhancement_enabled());
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

    /// Verifies tmux detection accepts the pane environment variable set by
    /// the multiplexer.
    #[test]
    fn tmux_environment_detects_tmux_variable() {
        // Arrange
        let variables = [("TMUX", Some("/tmp/tmux-501/default,123,0"))];

        // Act
        let is_tmux_session = has_tmux_environment(|name| {
            variables
                .iter()
                .find(|(variable_name, _)| *variable_name == name)
                .and_then(|(_, value)| value.map(OsString::from))
        });

        // Assert
        assert!(is_tmux_session);
    }

    /// Verifies tmux detection stays false when the pane marker is absent.
    #[test]
    fn tmux_environment_rejects_without_tmux_variable() {
        // Arrange & Act
        let is_tmux_session = has_tmux_environment(|_| None);

        // Assert
        assert!(!is_tmux_session);
    }

    /// Verifies the xterm modified-key commands request CSI-u encoding and
    /// reset the modified-key resources during terminal restore.
    #[test]
    fn xterm_modified_key_commands_request_and_reset_csi_u_reporting() {
        // Arrange
        let mut enable_sequence = String::new();
        let mut disable_sequence = String::new();

        // Act
        EnableXtermCsiUModifiedKeys
            .write_ansi(&mut enable_sequence)
            .expect("enable sequence should render");
        DisableXtermCsiUModifiedKeys
            .write_ansi(&mut disable_sequence)
            .expect("disable sequence should render");

        // Assert
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
            .withf(|_, keyboard_enhancement_enabled| *keyboard_enhancement_enabled)
            .returning(move |_, _| {
                leave_calls_for_expectation.fetch_add(1, Ordering::Relaxed);
                Ok(())
            });

        // Act
        restore_terminal_state(&operation, true);

        // Assert
        assert_eq!(leave_calls.load(Ordering::Relaxed), 1);
    }
}
