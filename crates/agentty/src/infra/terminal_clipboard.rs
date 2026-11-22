//! Terminal clipboard boundary that copies text with the OSC 52 escape
//! sequence, so copying works over SSH and needs no platform tool.

use std::io::{self, Write};
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Copies text to the clipboard of the terminal that shows Agentty.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait TerminalClipboard: Send + Sync {
    /// Asks the terminal to copy `text` and returns whether the request was
    /// written. Terminals that ignore OSC 52, or `tmux` without
    /// `set-clipboard on`, drop it silently.
    fn copy(&self, text: &str) -> bool;
}

/// [`TerminalClipboard`] that writes OSC 52 requests to a writer.
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
}

impl TerminalClipboard for Osc52Clipboard {
    fn copy(&self, text: &str) -> bool {
        let request = format!("\x1b]52;c;{}\x07", STANDARD.encode(text));
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);

        writer
            .write_all(request.as_bytes())
            .and_then(|()| writer.flush())
            .is_ok()
    }
}

#[cfg(test)]
#[path = "terminal_clipboard_test.rs"]
mod tests;
