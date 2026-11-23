//! Copies a link the terminal shows, such as a pairing or sign-in link,
//! through the terminal clipboard.
//!
//! `tmux` copies in the background, because it waits for a `tmux` command,
//! and its result returns as an [`AppEvent`]. When `tmux` did not copy the
//! link, [`LinkCopy::finish`] asks the terminal from the foreground, the
//! only thread that may write to it.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::app::spawn::spawn_blocking_detached;
use crate::infra::terminal_clipboard::TerminalClipboard;

/// Builds the event that reports whether `tmux` copied the link of one
/// request: its identifier, then whether it copied.
pub(crate) type LinkCopiedEvent = fn(u64, bool) -> AppEvent;

/// The latest copy of one kind of link.
pub(crate) struct LinkCopy {
    clipboard: Arc<dyn TerminalClipboard>,
    event: LinkCopiedEvent,
    /// Link of the latest copy while it runs.
    pending: Option<String>,
    /// Identifies the latest copy, so the result of an earlier one, or of
    /// a link that is no longer shown, is ignored.
    request_id: u64,
}

impl LinkCopy {
    /// Creates idle copying through `clipboard` that reports `tmux`
    /// results as `event`.
    pub(crate) fn new(clipboard: Arc<dyn TerminalClipboard>, event: LinkCopiedEvent) -> Self {
        Self {
            clipboard,
            event,
            pending: None,
            request_id: 0,
        }
    }

    /// Starts copying `link` through `tmux` in the background, replacing
    /// any copy still running.
    pub(crate) fn start(&mut self, link: String, event_tx: mpsc::UnboundedSender<AppEvent>) {
        self.forget();
        let request_id = self.request_id;
        let event = self.event;
        let clipboard = Arc::clone(&self.clipboard);
        self.pending = Some(link.clone());
        spawn_blocking_detached(move || {
            let is_copied = clipboard.copy_through_tmux(&link);
            // The app owns the event channel, so a closed one only drops the
            // result.
            let _ = event_tx.send(event(request_id, is_copied));
        });
    }

    /// Disowns any copy still running, because its result would describe
    /// a link that is no longer shown.
    pub(crate) fn forget(&mut self) {
        self.request_id += 1;
        self.pending = None;
    }

    /// Finishes the copy identified by `request_id` and returns whether the
    /// link reached a clipboard, asking the terminal when `tmux` did not
    /// copy it. Returns `None` for a copy that a later one replaced or that
    /// was forgotten.
    pub(crate) fn finish(&mut self, request_id: u64, is_copied: bool) -> Option<bool> {
        if request_id != self.request_id {
            return None;
        }
        let link = self.pending.take()?;

        Some(is_copied || self.clipboard.copy_through_terminal(&link))
    }
}
