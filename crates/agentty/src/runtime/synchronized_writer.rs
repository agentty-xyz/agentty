//! Terminal writer that presents each frame as one synchronized update.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

/// Writer that brackets terminal output in synchronized updates
/// (`CSI ? 2026 h` … `CSI ? 2026 l`).
///
/// Standard output reaches the terminal in buffer-sized chunks, so a large
/// redraw can arrive in several reads. Inside a synchronized update the
/// terminal keeps showing its previous state until the update ends, so a
/// reader never sees a redraw that is only partly applied. Terminals without
/// the extension ignore both sequences.
///
/// The first nonempty write opens an update. A flush closes it unless its
/// [`FrameHold`] is held: Ratatui and crossterm flush several times while
/// clearing and drawing one frame, so the runtime holds the update for the
/// whole frame and flushes once afterwards. Dropping the writer always closes
/// an open update, so the terminal is never left waiting for an end that does
/// not come.
pub(crate) struct SynchronizedWriter<W: Write> {
    frame_hold: FrameHold,
    is_updating: bool,
    writer: W,
}

impl<W: Write> SynchronizedWriter<W> {
    /// Wraps `writer`, keeping updates open while `frame_hold` is held.
    pub(crate) fn new(writer: W, frame_hold: FrameHold) -> Self {
        Self {
            frame_hold,
            is_updating: false,
            writer,
        }
    }

    /// Closes the open synchronized update, if any.
    fn end_update(&mut self) -> io::Result<()> {
        if self.is_updating {
            queue!(self.writer, EndSynchronizedUpdate)?;
            self.is_updating = false;
        }

        Ok(())
    }
}

impl<W: Write> Write for SynchronizedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.is_updating && !buf.is_empty() {
            queue!(self.writer, BeginSynchronizedUpdate)?;
            self.is_updating = true;
        }

        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.frame_hold.is_held() {
            self.end_update()?;
        }

        self.writer.flush()
    }
}

impl<W: Write> Drop for SynchronizedWriter<W> {
    fn drop(&mut self) {
        let _ = self.end_update();
        let _ = self.writer.flush();
    }
}

/// Shared switch that keeps a [`SynchronizedWriter`]'s update open across
/// flushes, so everything written for one frame is presented together.
///
/// Clones share one switch. A hold that no writer shares, as with test
/// backends, has no effect.
#[derive(Clone, Default)]
pub(crate) struct FrameHold {
    is_held: Arc<AtomicBool>,
}

impl FrameHold {
    /// Keeps the update open until the returned guard drops. The writer's
    /// next flush after that presents the frame.
    pub(crate) fn hold(&self) -> HeldFrame<'_> {
        self.is_held.store(true, Ordering::Relaxed);

        HeldFrame { frame_hold: self }
    }

    /// Returns whether a [`HeldFrame`] is alive.
    fn is_held(&self) -> bool {
        self.is_held.load(Ordering::Relaxed)
    }
}

/// Guard that keeps its [`FrameHold`] held until it drops.
pub(crate) struct HeldFrame<'a> {
    frame_hold: &'a FrameHold,
}

impl Drop for HeldFrame<'_> {
    fn drop(&mut self) {
        self.frame_hold.is_held.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "synchronized_writer_test.rs"]
mod tests;
