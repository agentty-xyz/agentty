//! Terminal clipboard boundary that copies text through `tmux` or with the
//! OSC 52 escape sequence, so copying works over SSH and needs no platform
//! tool.

use std::io::{self, Write};
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::infra::tmux;

/// Copies text to the clipboard of the terminal that shows Agentty.
///
/// A copy has two steps, because only the thread that draws may write to the
/// terminal: [`Self::copy_through_tmux`] off that thread, and
/// [`Self::copy_through_terminal`] on it when `tmux` did not take the text.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait TerminalClipboard: Send + Sync {
    /// Copies `text` through the enclosing `tmux` session and returns whether
    /// `tmux` took it, which it never does outside `tmux`.
    ///
    /// This waits for a `tmux` command and never writes to the terminal, so
    /// it runs off the thread that draws.
    fn copy_through_tmux(&self, text: &str) -> bool;

    /// Asks the terminal to copy `text` and returns whether the request was
    /// handed over. Terminals without clipboard support drop it silently.
    ///
    /// This writes to the terminal, so only the thread that draws may call
    /// it, between frames. A write from another thread can land inside an
    /// escape sequence of a frame.
    fn copy_through_terminal(&self, text: &str) -> bool;
}

/// Writes OSC 52 clipboard requests to a writer.
pub(crate) struct Osc52Clipboard {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl Osc52Clipboard {
    /// Writes requests to the process's standard output, the terminal.
    pub(crate) fn stdout() -> Self {
        Self::new(Box::new(io::stdout()))
    }

    /// Writes requests to `writer`.
    pub(crate) fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }

    /// Writes one request to copy `text` and returns whether it was written.
    fn copy(&self, text: &str) -> bool {
        let request = format!("\x1b]52;c;{}\x07", STANDARD.encode(text));
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);

        writer
            .write_all(request.as_bytes())
            .and_then(|()| writer.flush())
            .is_ok()
    }
}

/// [`TerminalClipboard`] that copies through the enclosing `tmux` session
/// when there is one and with an OSC 52 request otherwise.
///
/// `tmux` drops OSC 52 requests from pane applications unless
/// `set-clipboard` is `on`, so inside `tmux` the copy goes through `tmux`.
pub(crate) struct TmuxAwareClipboard {
    osc52: Osc52Clipboard,
    /// Copies through `tmux` and returns `false` when it cannot.
    tmux_copy: fn(&str) -> bool,
}

impl TmuxAwareClipboard {
    /// Copies through the enclosing `tmux` session, or through the process's
    /// standard output outside `tmux`.
    pub(crate) fn stdout() -> Self {
        Self::new(Osc52Clipboard::stdout(), tmux::copy_to_clipboard)
    }

    /// Copies through `tmux_copy` first and through `osc52` when it fails.
    fn new(osc52: Osc52Clipboard, tmux_copy: fn(&str) -> bool) -> Self {
        Self { osc52, tmux_copy }
    }
}

impl TerminalClipboard for TmuxAwareClipboard {
    fn copy_through_tmux(&self, text: &str) -> bool {
        (self.tmux_copy)(text)
    }

    fn copy_through_terminal(&self, text: &str) -> bool {
        self.osc52.copy(text)
    }
}

#[cfg(test)]
#[path = "terminal_clipboard_test.rs"]
mod tests;
