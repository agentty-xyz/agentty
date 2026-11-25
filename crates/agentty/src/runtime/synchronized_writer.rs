//! Terminal writer that presents each flushed batch as one synchronized update.

use std::io::{self, Write};

use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

/// Writer that brackets everything written between two flushes in a
/// synchronized update (`CSI ? 2026 h` … `CSI ? 2026 l`).
///
/// Standard output reaches the terminal in buffer-sized chunks, so a large
/// redraw can arrive in several reads. Inside a synchronized update the
/// terminal keeps showing its previous state until the update ends, so a
/// reader never sees a redraw that is only partly applied. Ratatui queues a
/// frame's cell changes without flushing, so each frame lands in one update.
/// Terminals without the extension ignore both sequences.
///
/// The first nonempty write after a flush opens the update; the next flush,
/// or dropping the writer, closes it, so the terminal is never left waiting
/// for an end that does not come.
pub(crate) struct SynchronizedWriter<W: Write> {
    is_updating: bool,
    writer: W,
}

impl<W: Write> SynchronizedWriter<W> {
    /// Wraps `writer`.
    pub(crate) fn new(writer: W) -> Self {
        Self {
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
        self.end_update()?;

        self.writer.flush()
    }
}

impl<W: Write> Drop for SynchronizedWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
#[path = "synchronized_writer_test.rs"]
mod tests;
