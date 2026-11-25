use std::io::{self, Write};

use super::{Osc52Clipboard, TerminalClipboard, TmuxAwareClipboard};
use crate::infra::tmux;
use crate::test_support::SharedBuffer;

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

#[test]
fn tmux_step_reports_the_tmux_result_without_writing_to_the_terminal() {
    // Arrange
    let buffer = SharedBuffer::default();
    let accepting =
        TmuxAwareClipboard::new(Osc52Clipboard::new(Box::new(buffer.clone())), |_text| true);
    let refusing =
        TmuxAwareClipboard::new(Osc52Clipboard::new(Box::new(buffer.clone())), |_text| false);

    // Act
    let is_accepted = accepting.copy_through_tmux("text");
    let is_refused = !refusing.copy_through_tmux("text");

    // Assert
    assert!(is_accepted);
    assert!(is_refused);
    assert_eq!(buffer.text(), "");
}

#[test]
fn terminal_step_writes_an_osc52_request() {
    // Arrange
    let buffer = SharedBuffer::default();
    let clipboard =
        TmuxAwareClipboard::new(Osc52Clipboard::new(Box::new(buffer.clone())), |_text| false);

    // Act
    let is_copied = clipboard.copy_through_terminal("text");

    // Assert
    assert!(is_copied);
    assert_eq!(buffer.text(), "\x1b]52;c;dGV4dA==\x07");
}

#[test]
fn tmux_aware_stdout_clipboard_copies_through_the_tmux_boundary() {
    // Arrange, Act
    let clipboard = TmuxAwareClipboard::stdout();

    // Assert
    assert!(std::ptr::fn_addr_eq(
        clipboard.tmux_copy,
        tmux::copy_to_clipboard as fn(&str) -> bool
    ));
}
