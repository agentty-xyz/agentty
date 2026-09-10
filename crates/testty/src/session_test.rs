use std::io::{self, Cursor, Read};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::frame::CellColor;
use crate::session::{PtySession, PtySessionBuilder, PtySessionError};
use crate::step::Step;

#[test]
fn pty_session_builder_forwards_args() {
    // Arrange / Act — collect args from a string slice iterator.
    let builder = PtySessionBuilder::new("/bin/echo").args(["--help", "--version"]);

    // Assert — args land in insertion order so the spawned command
    // receives `--help --version`.
    assert_eq!(
        builder.args,
        vec!["--help".to_string(), "--version".to_string()]
    );
}

#[test]
fn pty_session_builder_args_appends_across_calls() {
    // Arrange / Act — multiple args calls accumulate, mirroring how env
    // calls accumulate, so callers can compose argument lists from
    // multiple sources.
    let builder = PtySessionBuilder::new("/bin/echo")
        .args(["one"])
        .args(vec![String::from("two"), String::from("three")]);

    // Assert
    assert_eq!(builder.args, vec!["one", "two", "three"]);
}

#[test]
fn key_to_bytes_returns_ctrl_a() {
    // Arrange / Act
    let bytes = PtySession::key_to_bytes("ctrl+a");

    // Assert — Ctrl+A = 0x01.
    assert_eq!(bytes, vec![0x01]);
}

#[test]
fn key_to_bytes_returns_ctrl_z() {
    // Arrange / Act
    let bytes = PtySession::key_to_bytes("ctrl+z");

    // Assert — Ctrl+Z = 0x1a.
    assert_eq!(bytes, vec![0x1a]);
}

#[test]
fn key_to_bytes_ctrl_multi_char_falls_through() {
    // Arrange / Act — "ctrl+ab" must not silently resolve to Ctrl+A.
    let bytes = PtySession::key_to_bytes("ctrl+ab");

    // Assert
    assert_eq!(bytes, "ctrl+ab".as_bytes());
}

#[test]
fn key_to_bytes_ctrl_non_alpha_falls_through() {
    // Arrange / Act — ctrl+[ is not a valid ctrl+letter combination.
    let bytes = PtySession::key_to_bytes("ctrl+[");

    // Assert — falls through to raw bytes instead of panicking.
    assert_eq!(bytes, "ctrl+[".as_bytes());
}

#[test]
fn key_to_bytes_known_keys() {
    // Arrange
    let cases: &[(&str, &[u8])] = &[
        ("enter", b"\r"),
        ("return", b"\r"),
        ("tab", b"\t"),
        ("escape", b"\x1b"),
        ("esc", b"\x1b"),
        ("backspace", b"\x7f"),
        ("up", b"\x1b[A"),
        ("down", b"\x1b[B"),
        ("right", b"\x1b[C"),
        ("left", b"\x1b[D"),
        ("home", b"\x1b[H"),
        ("end", b"\x1b[F"),
        ("delete", b"\x1b[3~"),
        ("pageup", b"\x1b[5~"),
        ("pagedown", b"\x1b[6~"),
        ("space", b" "),
    ];

    for (key, expected) in cases {
        // Act
        let bytes = PtySession::key_to_bytes(key);

        // Assert
        assert_eq!(bytes, *expected, "key: {key}");
    }
}

#[test]
fn key_to_bytes_backtab_sends_csi_z() {
    // Arrange / Act / Assert — BackTab is ESC [ Z.
    assert_eq!(PtySession::key_to_bytes("backtab"), vec![0x1b, b'[', b'Z']);
    assert_eq!(
        PtySession::key_to_bytes("shift+tab"),
        vec![0x1b, b'[', b'Z']
    );
}

#[test]
fn key_to_bytes_unknown_key_returns_raw_bytes() {
    // Arrange / Act
    let bytes = PtySession::key_to_bytes("x");

    // Assert
    assert_eq!(bytes, vec![b'x']);
}

#[test]
fn execute_steps_captures_input_after_an_earlier_wait() {
    // Arrange
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args([
            "-c",
            "printf 'ready\\n'; read value; printf 'done:%s\\n' \"$value\"; sleep 60",
        ])
        .spawn()
        .expect("failed to spawn interactive shell script");
    let steps = [
        Step::wait_for_text("ready", 3_000),
        Step::write_text("hello\n"),
    ];

    // Act
    let frame = session
        .execute_steps(&steps)
        .expect("step execution should succeed");

    // Assert
    assert!(
        frame.all_text().contains("done:hello"),
        "final frame must include output produced after the wait"
    );
}

#[test]
fn execute_steps_captures_without_a_reusable_frame() {
    // Arrange
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args(["-c", "printf 'ready\\n'; sleep 60"])
        .spawn()
        .expect("failed to spawn shell script");
    let steps = [Step::capture()];

    // Act
    let frame = session
        .execute_steps(&steps)
        .expect("step execution should succeed");

    // Assert
    assert!(frame.all_text().contains("ready"));
}

#[test]
fn execute_steps_reuses_wait_frame_for_capture() {
    // Arrange — the second output arrives after the wait's read window but
    // within a fresh capture's read window.
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args([
            "-c",
            "sleep 0.2; printf 'ready\\n'; sleep 0.25; printf 'later\\n'; sleep 60",
        ])
        .spawn()
        .expect("failed to spawn delayed shell script");
    let steps = [Step::wait_for_text("ready", 3_000), Step::capture()];

    // Act
    let frame = session
        .execute_steps(&steps)
        .expect("step execution should succeed");

    // Assert
    assert!(frame.all_text().contains("ready"));
    assert!(
        !frame.all_text().contains("later"),
        "capture must reuse the frame supplied by the preceding wait"
    );
}

/// Verifies that `wait_for_stable_frame` times out when the spawned
/// binary produces no terminal output, instead of returning an empty
/// frame immediately.
#[test]
fn wait_for_stable_frame_times_out_when_no_output() {
    // Arrange — script stays alive but produces nothing.
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let script_path = temp_dir.path().join("silent.sh");
    std::fs::write(&script_path, "#!/bin/sh\nsleep 60\n").expect("failed to write script");
    #[cfg(unix)]
    {
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o750))
            .expect("failed to set permissions");
    }

    let mut session = PtySession::spawn(&script_path).expect("failed to spawn silent script");

    // Act
    let result =
        session.wait_for_stable_frame(Duration::from_millis(200), Duration::from_millis(800));

    // Assert
    assert!(
        matches!(result, Err(PtySessionError::Timeout(_))),
        "should timeout when no frame change is observed"
    );
}

/// Verifies that textless terminal output can stabilize when it changes
/// cursor state without rendering visible text.
#[test]
fn wait_for_stable_frame_returns_after_cursor_only_output() {
    // Arrange — move and hide the cursor without writing visible text.
    let script = "printf '\\033[2;2H\\033[?25l'; sleep 60";
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args(["-c", script])
        .spawn()
        .expect("failed to spawn cursor-only shell script");

    // Act
    let frame = session
        .wait_for_stable_frame(Duration::from_millis(200), Duration::from_secs(5))
        .expect("cursor-only frame should stabilize");

    // Assert
    assert!(
        frame.all_text().is_empty(),
        "cursor-only frame should remain textless"
    );
}

/// Verifies that `wait_for_stable_frame` returns a non-empty frame once
/// the binary has rendered output and the frame stops changing.
#[test]
fn wait_for_stable_frame_returns_after_content_stabilizes() {
    // Arrange — script writes visible text and stays alive so the PTY
    // does not close.
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let script_path = temp_dir.path().join("greet.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho hello\nsleep 60\n")
        .expect("failed to write script");
    #[cfg(unix)]
    {
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o750))
            .expect("failed to set permissions");
    }

    let mut session = PtySession::spawn(&script_path).expect("failed to spawn greet script");

    // Act
    let frame = session
        .wait_for_stable_frame(Duration::from_millis(300), Duration::from_secs(5))
        .expect("frame should stabilize");

    // Assert
    let text = frame.all_text();
    assert!(
        text.contains("hello"),
        "stable frame should contain echoed output, got: '{text}'"
    );
}

/// Verifies style-only redraws reset the stability window even when the
/// visible text does not change.
#[test]
fn wait_for_stable_frame_tracks_style_changes() {
    // Arrange — repaint the same text in alternating colors for longer
    // than the stability window, then leave the final blue frame visible.
    let script = concat!(
        "printf '\\033[31mready'; ",
        "sleep 0.1; printf '\\r\\033[32mready'; ",
        "sleep 0.1; printf '\\r\\033[31mready'; ",
        "sleep 0.1; printf '\\r\\033[32mready'; ",
        "sleep 0.1; printf '\\r\\033[31mready'; ",
        "sleep 0.1; printf '\\r\\033[32mready'; ",
        "sleep 0.1; printf '\\r\\033[31mready'; ",
        "sleep 0.1; printf '\\r\\033[34mready'; ",
        "sleep 60",
    );
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args(["-c", script])
        .spawn()
        .expect("failed to spawn styled shell script");

    // Act
    let frame = session
        .wait_for_stable_frame(Duration::from_millis(250), Duration::from_secs(5))
        .expect("styled frame should stabilize");

    // Assert
    assert_eq!(frame.fg_color(0, 0), Some(CellColor::new(0, 0, 128)));
}

#[test]
fn reader_forwards_output_until_end_of_stream() {
    // Arrange
    let reader = Box::new(Cursor::new(b"terminal output".to_vec()));
    let (sender, receiver) = mpsc::channel();

    // Act
    PtySession::read_pty_output(reader, &sender);
    drop(sender);
    let chunks: Vec<_> = receiver.iter().collect();

    // Assert
    assert_eq!(chunks, vec![b"terminal output".to_vec()]);
}

#[test]
fn reader_stops_when_session_receiver_is_dropped() {
    // Arrange
    let reads = Arc::new(AtomicUsize::new(0));
    let reader = Box::new(CountingReader {
        bytes: Cursor::new(vec![b'x'; 8192]),
        reads: Arc::clone(&reads),
    });
    let (sender, receiver) = mpsc::channel();
    drop(receiver);

    // Act
    PtySession::read_pty_output(reader, &sender);

    // Assert — stop at the first rejected chunk instead of draining the stream.
    assert_eq!(reads.load(Ordering::Relaxed), 1);
}

struct CountingReader {
    bytes: Cursor<Vec<u8>>,
    reads: Arc<AtomicUsize>,
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads.fetch_add(1, Ordering::Relaxed);

        self.bytes.read(buffer)
    }
}
