use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use super::{Osc52Clipboard, TerminalClipboard};

/// Writer that keeps everything written in a shared buffer.
#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl SharedBuffer {
    /// Returns everything written so far as text.
    fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);

        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buffer);

        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writer that refuses every write.
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn copy_writes_one_osc52_request_with_base64_text() {
    // Arrange
    let buffer = SharedBuffer::default();
    let clipboard = Osc52Clipboard::new(Box::new(buffer.clone()));

    // Act
    let is_written = clipboard.copy("https://relay.example/#code=AB");

    // Assert
    assert!(is_written);
    assert_eq!(
        buffer.text(),
        "\x1b]52;c;aHR0cHM6Ly9yZWxheS5leGFtcGxlLyNjb2RlPUFC\x07"
    );
}

#[test]
fn copy_reports_a_failed_write() {
    // Arrange
    let clipboard = Osc52Clipboard::new(Box::new(FailingWriter));

    // Act
    let is_written = clipboard.copy("text");

    // Assert
    assert!(!is_written);
}

#[test]
fn stdout_clipboard_builds() {
    // Arrange, Act
    let clipboard = Osc52Clipboard::stdout();

    // Assert
    assert!(clipboard.writer.lock().is_ok());
}
