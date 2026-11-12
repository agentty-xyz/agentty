use std::collections::VecDeque;
use std::io::{self, Cursor, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::io::RawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

#[cfg(unix)]
use portable_pty::unix::RawFd;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtyPair, PtySize, PtySystem,
    SlavePty,
};

use crate::assertion::{Expected, match_not_visible};
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

/// Verifies that whole and split primary device attributes requests receive
/// a terminal reply before the child's one-second read timeout.
#[test]
fn primary_device_attributes_requests_receive_terminal_replies() {
    // Arrange — script sends one whole and one split DA1 request, reads each
    // reply without echo, and prints the replies without their escape byte.
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let script_path = temp_dir.path().join("device-attributes.sh");
    std::fs::write(
        &script_path,
        "#!/bin/bash\nstty raw -echo\nprintf '\\033[c'\nIFS= read -r -t 1 -d c whole\nprintf \
         '\\033['\nsleep 0.2\nprintf '0c'\nIFS= read -r -t 1 -d c split\nstty sane\nprintf \
         'whole:%s split:%s\\n' \"${whole#?}\" \"${split#?}\"\nsleep 60\n",
    )
    .expect("failed to write script");
    #[cfg(unix)]
    {
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o750))
            .expect("failed to set permissions");
    }

    let mut session =
        PtySession::spawn(&script_path).expect("failed to spawn device attributes script");

    // Act
    let frame = session
        .wait_for_text("whole:", Duration::from_secs(5))
        .expect("script should report both replies");

    // Assert
    let text = frame.all_text();
    assert!(
        text.contains("whole:[?1;2 split:[?1;2"),
        "both requests should receive the DA1 reply, got: '{text}'"
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

#[test]
fn builder_spawns_with_size_environment_and_working_directory() {
    // Arrange
    let workdir = tempfile::tempdir().expect("failed to create working directory");
    let workdir_name = workdir
        .path()
        .file_name()
        .expect("temporary directory has a name")
        .to_string_lossy()
        .into_owned();
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args([
            "-c",
            "pwd; printf 'env:%s\\n' \"$TESTTY_SESSION_VALUE\"; sleep 60",
        ])
        .env("TESTTY_SESSION_VALUE", "forwarded")
        .workdir(workdir.path())
        .size(160, 6)
        .spawn()
        .expect("failed to spawn configured shell");

    // Act
    let frame = session
        .wait_for_text("env:forwarded", Duration::from_secs(5))
        .expect("child should report the forwarded environment");

    // Assert
    assert!(
        frame.all_text().contains(&workdir_name),
        "child should start in the configured directory, got: '{}'",
        frame.all_text()
    );
    assert_eq!(session.cols(), 160);
    assert_eq!(session.rows(), 6);
    assert_eq!(session.binary_path(), Path::new("/bin/sh"));
}

#[test]
fn spawn_maps_pty_allocation_and_handle_failures() {
    // Arrange
    let cases = [
        (FakePtyFailure::OpenPty, "openpty refused"),
        (FakePtyFailure::Reader, "reader refused"),
        (FakePtyFailure::Writer, "writer refused"),
    ];

    for (failure, expected_message) in cases {
        // Act
        let result = PtySession::spawn_with_pty_system(
            &FakePtySystem { failure },
            Path::new("fake-binary"),
            80,
            24,
            &[],
            &[],
            None,
        );

        // Assert
        assert!(
            matches!(
                &result,
                Err(PtySessionError::PtyCreation(message)) if message == expected_message
            ),
            "{expected_message}: unexpected spawn result"
        );
    }
}

#[test]
fn write_bytes_maps_write_and_flush_failures() {
    // Arrange
    let cases = [
        (FailingOperation::Write, "write refused"),
        (FailingOperation::Flush, "flush refused"),
    ];

    for (operation, expected_message) in cases {
        let mut session = fake_session(
            FakeChild::running(),
            Box::new(FailingWriter { operation }),
            &[],
        );

        // Act
        let result = session.write_bytes(b"input");

        // Assert
        assert!(
            matches!(
                &result,
                Err(PtySessionError::WriteFailed(message)) if message == expected_message
            ),
            "{expected_message}: unexpected write result {result:?}"
        );
    }
}

#[test]
fn execute_steps_runs_key_sleep_pause_and_eventually_steps() {
    // Arrange
    let written = Arc::new(Mutex::new(Vec::new()));
    let mut session = fake_session(
        FakeChild::running(),
        Box::new(RecordingWriter {
            bytes: Arc::clone(&written),
        }),
        &[b"ready"],
    );
    let steps = [
        Step::press_key("Enter"),
        Step::sleep(Duration::ZERO),
        Step::viewing_pause_ms(60_000),
        Step::eventually(Duration::from_secs(5), Duration::from_millis(10), |frame| {
            match_not_visible(frame, "missing")
        }),
    ];

    // Act
    let frame = session
        .execute_steps(&steps)
        .expect("step execution should succeed");

    // Assert
    assert_eq!(
        *written.lock().expect("writer mutex must not be poisoned"),
        b"\r".to_vec()
    );
    assert!(frame.all_text().contains("ready"));
}

#[test]
fn execute_steps_surfaces_eventually_assertion_failure() {
    // Arrange
    let mut session = fake_session(FakeChild::running(), Box::new(io::sink()), &[b"ready"]);
    let steps = [Step::eventually(
        Duration::ZERO,
        Duration::from_millis(10),
        |frame| match_not_visible(frame, "ready"),
    )];

    // Act
    let result = session.execute_steps(&steps);

    // Assert
    let Err(PtySessionError::Assertion(failure)) = result else {
        unreachable!("an expired predicate must return its assertion failure");
    };
    assert!(matches!(
        failure.expected,
        Expected::NotVisible { ref needle } if needle == "ready"
    ));
}

#[test]
fn waits_time_out_when_output_never_satisfies_them() {
    // Arrange
    let mut text_session = fake_session(FakeChild::running(), Box::new(io::sink()), &[b"other"]);
    // The disconnected fake output drains instantly, so this wait polls again
    // after its first miss until the deadline passes.
    let mut polling_text_session =
        fake_session(FakeChild::running(), Box::new(io::sink()), &[b"other"]);
    let mut stable_session = fake_session(FakeChild::running(), Box::new(io::sink()), &[b"busy"]);

    // Act
    let text_result = text_session.wait_for_text("needle", Duration::ZERO);
    let polling_text_result =
        polling_text_session.wait_for_text("needle", Duration::from_millis(100));
    let stable_result =
        stable_session.wait_for_stable_frame(Duration::from_mins(1), Duration::ZERO);

    // Assert
    assert!(
        matches!(&text_result, Err(PtySessionError::Timeout(message))
            if message == "Text 'needle' did not appear within 0ms"),
        "unexpected text wait result"
    );
    assert!(
        matches!(&polling_text_result, Err(PtySessionError::Timeout(message))
            if message == "Text 'needle' did not appear within 100ms"),
        "unexpected polling text wait result"
    );
    assert!(
        matches!(&stable_result, Err(PtySessionError::Timeout(message))
            if message == "Frame did not stabilize within timeout"),
        "unexpected stability wait result"
    );
}

#[test]
fn wait_for_exit_reports_status_timeout_and_wait_errors() {
    // Arrange
    let cases = [
        (
            vec![Ok(None), Ok(Some(ExitStatus::with_exit_code(0)))],
            Duration::from_secs(5),
            "exit:true",
        ),
        (
            vec![Ok(Some(ExitStatus::with_exit_code(3)))],
            Duration::from_secs(5),
            "exit:false",
        ),
        (
            Vec::new(),
            Duration::ZERO,
            "Timeout: Process did not exit within timeout",
        ),
        (
            vec![Err(io::Error::other("wait refused"))],
            Duration::from_secs(5),
            "Timeout: Error waiting for process exit: wait refused",
        ),
    ];

    for (statuses, timeout, expected) in cases {
        let mut session = fake_session(
            FakeChild {
                statuses: statuses.into(),
            },
            Box::new(io::sink()),
            &[],
        );

        // Act
        let outcome = match session.wait_for_exit(timeout) {
            Ok(success) => format!("exit:{success}"),
            Err(error) => error.to_string(),
        };

        // Assert
        assert_eq!(outcome, expected);
    }
}

/// Builds a session around fake process and input handles whose output
/// stream yields `output` and then disconnects.
fn fake_session(child: FakeChild, writer: Box<dyn Write + Send>, output: &[&[u8]]) -> PtySession {
    let (sender, receiver) = mpsc::channel();
    for chunk in output {
        sender
            .send(chunk.to_vec())
            .expect("receiver is alive while the fixture is built");
    }

    PtySession {
        binary_path: PathBuf::from("fake-binary"),
        child: Box::new(child),
        cols: 20,
        output_buffer: Vec::new(),
        output_receiver: receiver,
        query_scan_offset: 0,
        rows: 3,
        writer,
    }
}

/// Child process whose `try_wait` results are scripted; it keeps running
/// once the script is exhausted.
#[derive(Debug)]
struct FakeChild {
    statuses: VecDeque<io::Result<Option<ExitStatus>>>,
}

impl FakeChild {
    fn running() -> Self {
        Self {
            statuses: VecDeque::new(),
        }
    }
}

impl Child for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.statuses.pop_front().unwrap_or(Ok(None))
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        Ok(ExitStatus::with_exit_code(0))
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<RawHandle> {
        None
    }
}

impl ChildKiller for FakeChild {
    fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self::running())
    }
}

/// PTY handle whose acquisition fails in [`FakePtySystem`].
#[derive(Clone, Copy)]
enum FakePtyFailure {
    OpenPty,
    Reader,
    Writer,
}

/// PTY system that fails at one configured acquisition step.
struct FakePtySystem {
    failure: FakePtyFailure,
}

impl PtySystem for FakePtySystem {
    fn openpty(&self, _size: PtySize) -> anyhow::Result<PtyPair> {
        if matches!(self.failure, FakePtyFailure::OpenPty) {
            return Err(anyhow::anyhow!("openpty refused"));
        }

        Ok(PtyPair {
            slave: Box::new(FakeSlave),
            master: Box::new(FakeMaster {
                failure: self.failure,
            }),
        })
    }
}

/// Slave end that "spawns" a [`FakeChild`] without starting a process.
struct FakeSlave;

impl SlavePty for FakeSlave {
    fn spawn_command(
        &self,
        _command: CommandBuilder,
    ) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        Ok(Box::new(FakeChild::running()))
    }
}

/// Master end whose reader or writer acquisition fails as configured.
struct FakeMaster {
    failure: FakePtyFailure,
}

impl MasterPty for FakeMaster {
    fn resize(&self, _size: PtySize) -> anyhow::Result<()> {
        Ok(())
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        Ok(PtySize::default())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        if matches!(self.failure, FakePtyFailure::Reader) {
            return Err(anyhow::anyhow!("reader refused"));
        }

        Ok(Box::new(io::empty()))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        Err(anyhow::anyhow!("writer refused"))
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<RawFd> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// Writer operation that [`FailingWriter`] rejects.
#[derive(Clone, Copy)]
enum FailingOperation {
    Flush,
    Write,
}

/// Writer that rejects one operation with a descriptive error.
struct FailingWriter {
    operation: FailingOperation,
}

impl Write for FailingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if matches!(self.operation, FailingOperation::Write) {
            return Err(io::Error::other("write refused"));
        }

        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush refused"))
    }
}

/// Writer that records every byte written to the PTY input.
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .expect("writer mutex must not be poisoned")
            .extend_from_slice(buffer);

        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
