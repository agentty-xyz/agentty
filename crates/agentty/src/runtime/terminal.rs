use std::io;

use crossterm::cursor::Show;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
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

    /// Enters the alternate screen and enables bracketed paste.
    fn enter_alternate_screen(&self, stdout: &mut io::Stdout) -> io::Result<()>;

    /// Leaves the alternate screen, disables bracketed paste, and restores the
    /// terminal cursor.
    fn leave_alternate_screen(&self, stdout: &mut io::Stdout) -> io::Result<()>;
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

    fn enter_alternate_screen(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
    }

    fn leave_alternate_screen(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, Show)
    }
}

/// Shared production terminal operation implementation.
static CROSSTERM_TERMINAL_OPERATION: CrosstermTerminalOperation = CrosstermTerminalOperation;

/// Restores terminal state on all exit paths after raw mode is enabled.
///
/// The app uses `?` extensively inside the event loop and setup flow. Without
/// this guard, any early return after entering raw mode and the alternate
/// screen can leave the user's shell in a broken state.
///
/// Keeping cleanup in `Drop` guarantees restore runs during normal exit,
/// runtime errors, and unwinding panics.
pub(crate) struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(&CROSSTERM_TERMINAL_OPERATION);
    }
}

/// Enables raw mode, enters the alternate screen, and turns on bracketed paste
/// so multiline clipboard content arrives as `Event::Paste`.
pub(crate) fn setup_terminal() -> io::Result<TuiTerminal> {
    setup_terminal_with_operation(&CROSSTERM_TERMINAL_OPERATION)
}

/// Enables terminal modes with the supplied operation provider.
fn setup_terminal_with_operation(operation: &dyn TerminalOperation) -> io::Result<TuiTerminal> {
    operation.enable_raw_mode()?;

    let mut stdout = io::stdout();
    operation.enter_alternate_screen(&mut stdout)?;
    let backend = CrosstermBackend::new(stdout);

    Terminal::new(backend)
}

/// Restores terminal modes and ignores failures so drop paths do not panic.
fn restore_terminal_state(operation: &dyn TerminalOperation) {
    let mut stdout = io::stdout();
    let _ = operation.disable_raw_mode();
    let _ = operation.leave_alternate_screen(&mut stdout);
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
        operation
            .expect_enable_raw_mode()
            .once()
            .returning(|| Err(io::Error::other("enable failed")));
        operation.expect_enter_alternate_screen().times(0);

        // Act
        let result = setup_terminal_with_operation(&operation);

        // Assert
        let error = result.expect_err("setup should fail when raw mode fails");
        assert_eq!(error.to_string(), "enable failed");
    }

    /// Verifies setup returns alternate-screen failures directly.
    #[test]
    fn setup_terminal_returns_error_when_enter_alternate_screen_fails() {
        // Arrange
        let mut operation = MockTerminalOperation::new();
        operation
            .expect_enable_raw_mode()
            .once()
            .returning(|| Ok(()));
        operation
            .expect_enter_alternate_screen()
            .once()
            .returning(|_| Err(io::Error::other("enter failed")));

        // Act
        let result = setup_terminal_with_operation(&operation);

        // Assert
        let error = result.expect_err("setup should fail when alternate screen fails");
        assert_eq!(error.to_string(), "enter failed");
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
            .returning(move |_| {
                leave_calls_for_expectation.fetch_add(1, Ordering::Relaxed);
                Ok(())
            });

        // Act
        restore_terminal_state(&operation);

        // Assert
        assert_eq!(leave_calls.load(Ordering::Relaxed), 1);
    }
}
