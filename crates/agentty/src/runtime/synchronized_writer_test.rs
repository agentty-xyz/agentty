//! Synchronized terminal writer tests.

use std::io::Write;

use super::{FrameHold, SynchronizedWriter};

/// Starts a synchronized update.
const BEGIN: &[u8] = b"\x1b[?2026h";
/// Ends a synchronized update.
const END: &[u8] = b"\x1b[?2026l";

/// Joins byte slices into one expected output.
fn joined(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn writes_between_flushes_share_one_synchronized_update() {
    // Arrange
    let mut output = Vec::new();
    let mut writer = SynchronizedWriter::new(&mut output, FrameHold::default());

    // Act
    writer.write_all(b"first").expect("write succeeds");
    writer.write_all(b"second").expect("write succeeds");
    writer.flush().expect("flush succeeds");
    drop(writer);

    // Assert
    assert_eq!(output, joined(&[BEGIN, b"first", b"second", END]));
}

#[test]
fn each_flushed_batch_gets_its_own_synchronized_update() {
    // Arrange
    let mut output = Vec::new();
    let mut writer = SynchronizedWriter::new(&mut output, FrameHold::default());

    // Act
    writer.write_all(b"one").expect("write succeeds");
    writer.flush().expect("flush succeeds");
    writer.write_all(b"two").expect("write succeeds");
    writer.flush().expect("flush succeeds");
    drop(writer);

    // Assert
    assert_eq!(output, joined(&[BEGIN, b"one", END, BEGIN, b"two", END]));
}

#[test]
fn flushes_and_empty_writes_without_output_open_no_update() {
    // Arrange
    let mut output = Vec::new();
    let mut writer = SynchronizedWriter::new(&mut output, FrameHold::default());

    // Act
    writer.write_all(b"").expect("empty write succeeds");
    writer.flush().expect("flush succeeds");
    drop(writer);

    // Assert
    assert_eq!(output, Vec::<u8>::new());
}

#[test]
fn held_frame_keeps_one_update_open_across_flushes() {
    // Arrange
    let frame_hold = FrameHold::default();
    let mut output = Vec::new();
    let mut writer = SynchronizedWriter::new(&mut output, frame_hold.clone());

    // Act
    let held_frame = frame_hold.hold();
    writer.write_all(b"clear").expect("write succeeds");
    writer.flush().expect("held flush succeeds");
    writer.write_all(b"frame").expect("write succeeds");
    writer.flush().expect("held flush succeeds");
    drop(held_frame);
    writer.flush().expect("flush succeeds");
    drop(writer);

    // Assert
    assert_eq!(output, joined(&[BEGIN, b"clear", b"frame", END]));
}

#[test]
fn dropping_the_writer_closes_an_open_update_even_while_held() {
    // Arrange
    let frame_hold = FrameHold::default();
    let _held_frame = frame_hold.hold();
    let mut output = Vec::new();
    let mut writer = SynchronizedWriter::new(&mut output, frame_hold.clone());
    writer.write_all(b"unflushed").expect("write succeeds");

    // Act
    drop(writer);

    // Assert
    assert_eq!(output, joined(&[BEGIN, b"unflushed", END]));
}
