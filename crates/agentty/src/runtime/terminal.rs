//! TUI terminal setup and restore: raw mode, alternate screen, and keyboard
//! enhancement modes.

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

use crate::infra::tmux::{self, TmuxKeyReporting};
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

    /// Prepares and returns how an enclosing `tmux` pane reports modified
    /// keys, or [`TmuxKeyReporting::Outside`] when not running in `tmux`.
    ///
    /// `tmux` can hide terminal keyboard capability responses from the pane
    /// while still honoring explicit modified-key reporting requests.
    fn tmux_key_reporting(&self) -> TmuxKeyReporting;

    /// Returns whether the enclosing `tmux` session has its `mouse` option
    /// on, so wheel input over the pane needs explicit mouse reports.
    fn is_tmux_mouse_enabled(&self) -> bool;

    /// Enters the alternate screen, enables its keyboard enhancement flags
    /// when supported, and enables bracketed paste.
    fn enter_alternate_screen(
        &self,
        stdout: &mut dyn io::Write,
        enhancement: TerminalEnhancement,
    ) -> io::Result<()>;

    /// Leaves the alternate screen, disables bracketed paste, and restores the
    /// terminal cursor, optionally popping keyboard enhancement flags first.
    fn leave_alternate_screen(
        &self,
        stdout: &mut dyn io::Write,
        enhancement: TerminalEnhancement,
    ) -> io::Result<()>;
}

/// Terminal keyboard and mouse modes enabled for the active TUI session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalEnhancement {
    keyboard_enhancement_enabled: bool,
    mouse_reporting_enabled: bool,
    xterm_modified_keys_enabled: bool,
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

    fn tmux_key_reporting(&self) -> TmuxKeyReporting {
        tmux::prepare_key_reporting()
    }

    fn is_tmux_mouse_enabled(&self) -> bool {
        tmux::is_mouse_enabled()
    }

    fn enter_alternate_screen(
        &self,
        mut stdout: &mut dyn io::Write,
        enhancement: TerminalEnhancement,
    ) -> io::Result<()> {
        if enhancement.keyboard_enhancement_enabled {
            execute!(
                &mut stdout,
                // Kitty keyboard modes belong to the active screen. Enable
                // them after switching so modified keys reach the TUI.
                EnterAlternateScreen,
                EnableXtermCsiUModifiedKeys(enhancement),
                PushKeyboardEnhancementFlags(keyboard_enhancement_flags()),
                EnableBracketedPaste,
                EnableMouseWheelReporting(enhancement)
            )
        } else {
            execute!(
                &mut stdout,
                EnterAlternateScreen,
                EnableBracketedPaste,
                EnableMouseWheelReporting(enhancement)
            )
        }
    }

    fn leave_alternate_screen(
        &self,
        mut stdout: &mut dyn io::Write,
        enhancement: TerminalEnhancement,
    ) -> io::Result<()> {
        if enhancement.keyboard_enhancement_enabled {
            execute!(
                &mut stdout,
                DisableMouseWheelReporting(enhancement),
                PopKeyboardEnhancementFlags,
                DisableXtermCsiUModifiedKeys(enhancement),
                DisableBracketedPaste,
                LeaveAlternateScreen,
                Show
            )
        } else {
            execute!(
                &mut stdout,
                DisableMouseWheelReporting(enhancement),
                DisableBracketedPaste,
                LeaveAlternateScreen,
                Show
            )
        }
    }
}

/// Shared production terminal operation implementation.
static CROSSTERM_TERMINAL_OPERATION: CrosstermTerminalOperation = CrosstermTerminalOperation;

/// Conditionally requests button and wheel mouse reports in SGR encoding.
///
/// Only `tmux` panes with the `mouse` option on enable this: there, wheel
/// input over an alternate-screen pane is discarded unless the pane requested
/// mouse reports. Direct terminals and `tmux` with `mouse off` keep native
/// text selection and link clicks, and the outer terminal already translates
/// the wheel into arrow keys. Motion tracking stays off to avoid event floods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableMouseWheelReporting(TerminalEnhancement);

impl Command for EnableMouseWheelReporting {
    fn write_ansi(&self, buffer: &mut impl fmt::Write) -> fmt::Result {
        if self.0.mouse_reporting_enabled {
            buffer.write_str("\x1B[?1000h\x1B[?1006h")?;
        }

        Ok(())
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Conditionally disables the mouse reports enabled at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableMouseWheelReporting(TerminalEnhancement);

impl Command for DisableMouseWheelReporting {
    fn write_ansi(&self, buffer: &mut impl fmt::Write) -> fmt::Result {
        if self.0.mouse_reporting_enabled {
            buffer.write_str("\x1B[?1006l\x1B[?1000l")?;
        }

        Ok(())
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Conditionally requests xterm/tmux modified-key reporting in CSI-u format.
///
/// `tmux` listens for xterm's `modifyOtherKeys` controls when deciding
/// whether a pane application asked for extended keys. Crossterm's kitty
/// keyboard-protocol push is still used for terminals that support the kitty
/// stack. Direct terminals must not receive this request because their xterm
/// encoding for shifted punctuation is not understood by Crossterm 0.29.
/// `tmux` sends CSI-u only when its `extended-keys-format` is `csi-u`, so
/// setup requests this mode only after
/// [`tmux::prepare_key_reporting`] confirms that format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableXtermCsiUModifiedKeys(TerminalEnhancement);

impl Command for EnableXtermCsiUModifiedKeys {
    fn write_ansi(&self, buffer: &mut impl fmt::Write) -> fmt::Result {
        if self.0.xterm_modified_keys_enabled {
            buffer.write_str("\x1B[>4;1f\x1B[>4;2m")?;
        }

        Ok(())
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Conditionally restores xterm/tmux modified-key reporting resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableXtermCsiUModifiedKeys(TerminalEnhancement);

impl Command for DisableXtermCsiUModifiedKeys {
    fn write_ansi(&self, buffer: &mut impl fmt::Write) -> fmt::Result {
        if self.0.xterm_modified_keys_enabled {
            buffer.write_str("\x1B[>4f\x1B[>4m")?;
        }

        Ok(())
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
    enhancement: Cell<TerminalEnhancement>,
}

impl TerminalGuard {
    /// Creates a guard that restores terminal state for the active TUI session.
    pub(crate) fn new() -> Self {
        Self {
            enhancement: Cell::new(TerminalEnhancement {
                keyboard_enhancement_enabled: false,
                mouse_reporting_enabled: false,
                xterm_modified_keys_enabled: false,
            }),
        }
    }

    /// Records the enabled keyboard modes so cleanup can restore them.
    fn set_enhancement(&self, enhancement: TerminalEnhancement) {
        self.enhancement.set(enhancement);
    }

    /// Returns the keyboard modes that cleanup must restore.
    fn enhancement(&self) -> TerminalEnhancement {
        self.enhancement.get()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(&CROSSTERM_TERMINAL_OPERATION, self.enhancement());
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

    let enhancement = terminal_enhancement(operation);
    guard.set_enhancement(enhancement);

    let mut stdout = io::stdout();
    operation.enter_alternate_screen(&mut stdout, enhancement)?;

    Ok(stdout)
}

/// Selects the keyboard and mouse modes appropriate for the current terminal
/// transport.
fn terminal_enhancement(operation: &dyn TerminalOperation) -> TerminalEnhancement {
    let tmux_key_reporting = operation.tmux_key_reporting();
    let is_tmux_session = tmux_key_reporting != TmuxKeyReporting::Outside;
    let keyboard_enhancement_enabled =
        should_enable_keyboard_enhancement(operation, is_tmux_session);

    TerminalEnhancement {
        keyboard_enhancement_enabled,
        mouse_reporting_enabled: is_tmux_session && operation.is_tmux_mouse_enabled(),
        xterm_modified_keys_enabled: keyboard_enhancement_enabled
            && tmux_key_reporting == TmuxKeyReporting::CsiU,
    }
}

/// Returns whether setup should push Kitty keyboard enhancement flags.
///
/// Crossterm's support query is the preferred signal for local terminals. Over
/// SSH, the query can fail or report unsupported when the outer terminal still
/// honors the enhancement sequence, so remote sessions optimistically enable
/// it to keep modified `Enter` keys distinguishable. `tmux` panes get the same
/// optimistic path because the multiplexer can answer capability probes
/// differently from the terminal attached outside the pane.
fn should_enable_keyboard_enhancement(
    operation: &dyn TerminalOperation,
    is_tmux_session: bool,
) -> bool {
    match operation.supports_keyboard_enhancement() {
        Ok(true) => true,
        Ok(false) | Err(_) => operation.is_ssh_session() || is_tmux_session,
    }
}

/// Returns whether common SSH environment variables are present.
fn has_ssh_environment(mut get_var: impl FnMut(&str) -> Option<OsString>) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|name| get_var(name).is_some())
}

/// Restores terminal modes and ignores failures so drop paths do not panic.
fn restore_terminal_state(operation: &dyn TerminalOperation, enhancement: TerminalEnhancement) {
    let mut stdout = io::stdout();
    // Best-effort: terminal may already be in normal state.
    let _ = operation.disable_raw_mode();
    let _ = operation.leave_alternate_screen(&mut stdout, enhancement);
}

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;
