use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use rustix::event;
use rustix::event::Timespec;
use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::errors::ConnectError;
use x11rb::protocol::xproto::{
    Atom, CONVERT_SELECTION_REQUEST, CREATE_WINDOW_REQUEST, ConnectionExt, DELETE_PROPERTY_REQUEST,
    GET_PROPERTY_REQUEST, GetPropertyReply, INTERN_ATOM_REQUEST, InternAtomReply,
    PROPERTY_NOTIFY_EVENT, PropMode, Property, PropertyNotifyEvent, SELECTION_NOTIFY_EVENT, Screen,
    SelectionNotifyEvent, Setup,
};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::x11_utils::Serialize;

use crate::backend::contract::ClipboardBackend;
use crate::backend::x11::{
    AtomCollection, INCR_RESERVATION_BYTE_CAP, INCR_SEGMENT_TIMEOUT, IncrTransfer,
    MAX_CLIPBOARD_BYTE_COUNT, X11Clipboard, checked_clipboard_byte_count,
};
use crate::error::ClipboardError;

const MAX_CLIPBOARD_BYTE_COUNT_U32: u32 = 64 * 1024 * 1024;
const TEST_RESOURCE_ID_BASE: u32 = 0x0100_0000;
const TEST_ROOT_WINDOW_ID: u32 = 1;
const TEST_WINDOW_ID: u32 = 42;
/// Core protocol `BadWindow` error code.
const X11_BAD_WINDOW_ERROR_CODE: u8 = 3;
/// Capacity of the request buffer `x11rb` 0.14 keeps in each
/// `RustConnection`; requests that do not fit force a write to the socket.
const X11RB_WRITE_BUFFER_BYTE_COUNT: usize = 16 * 1024;
const X11_CONSTRUCTOR_CHILD_ENV: &str = "AGENTTY_TEST_X11_CONSTRUCTOR_CHILD";

struct X11TestServer {
    server_thread: thread::JoinHandle<()>,
    writer: UnixStream,
}

impl X11TestServer {
    fn start(steps: Vec<X11ServerStep>) -> (Self, RustConnection) {
        Self::start_with_setup(Self::default_setup(), steps)
    }

    /// Starts a scripted server that closes its socket right after setup.
    fn start_disconnected() -> RustConnection {
        let (server, connection) = Self::start(Vec::new());
        server.finish();

        connection
    }

    fn start_with_setup(setup: Setup, steps: Vec<X11ServerStep>) -> (Self, RustConnection) {
        let (client_stream, server_stream) =
            UnixStream::pair().expect("test X11 socket pair should open");
        let writer = server_stream
            .try_clone()
            .expect("test X11 server socket should clone");
        let server_thread = thread::spawn(move || Self::run(server_stream, &setup, steps));
        let (client_stream, _) = DefaultStream::from_unix_stream(client_stream)
            .expect("test X11 client stream should initialize");
        let connection = RustConnection::connect_to_stream(client_stream, 0)
            .expect("test X11 connection should complete setup");

        (
            Self {
                server_thread,
                writer,
            },
            connection,
        )
    }

    fn run(mut server_stream: UnixStream, setup: &Setup, steps: Vec<X11ServerStep>) {
        let mut setup_request = [0; 12];
        server_stream
            .read_exact(&mut setup_request)
            .expect("test X11 server should receive setup request");
        server_stream
            .write_all(&Self::setup_bytes(setup))
            .expect("test X11 server should send setup response");

        for (step_index, step) in steps.into_iter().enumerate() {
            let opcode = Self::read_request_opcode(&mut server_stream);
            assert_eq!(opcode, step.expected_opcode);
            let sequence =
                u16::try_from(step_index + 1).expect("test X11 request sequence should fit in u16");
            for response in step.responses {
                response.write_to(&mut server_stream, sequence);
            }
        }
    }

    fn default_setup() -> Setup {
        Setup {
            maximum_request_length: u16::MAX,
            protocol_major_version: 11,
            resource_id_base: TEST_RESOURCE_ID_BASE,
            resource_id_mask: 0x00FF_FFFF,
            roots: vec![Screen {
                root: TEST_ROOT_WINDOW_ID,
                ..Screen::default()
            }],
            status: 1,
            ..Setup::default()
        }
    }

    fn setup_bytes(setup: &Setup) -> Vec<u8> {
        let mut setup = setup.clone();
        setup.length = u16::try_from((setup.serialize().len() - 8) / 4)
            .expect("test X11 setup length should fit in u16");

        setup.serialize()
    }

    fn read_request_opcode(server_stream: &mut UnixStream) -> u8 {
        let mut header = [0; 4];
        server_stream
            .read_exact(&mut header)
            .expect("test X11 server should receive request header");
        let request_byte_count = usize::from(u16::from_ne_bytes([header[2], header[3]])) * 4;
        let body_byte_count = request_byte_count
            .checked_sub(header.len())
            .expect("test X11 request should include its header");
        let mut body = vec![0; body_byte_count];
        server_stream
            .read_exact(&mut body)
            .expect("test X11 server should receive request body");

        header[0]
    }

    fn send_response(&mut self, response: X11ServerResponse) {
        response.write_to(&mut self.writer, 0);
    }

    fn finish(self) {
        drop(self.writer);
        self.server_thread
            .join()
            .expect("test X11 server thread should finish");
    }
}

struct X11ServerStep {
    expected_opcode: u8,
    responses: Vec<X11ServerResponse>,
}

enum X11ServerResponse {
    /// Protocol error packet carrying the given error code.
    Error(u8),
    GetProperty(GetPropertyReply),
    InternAtom(Atom),
    PropertyNotify(PropertyNotifyEvent),
    SelectionNotify(SelectionNotifyEvent),
}

impl X11ServerResponse {
    fn write_to(self, server_stream: &mut UnixStream, sequence: u16) {
        let bytes = match self {
            Self::Error(error_code) => {
                let mut bytes = vec![0; 32];
                bytes[1] = error_code;
                bytes[2..4].copy_from_slice(&sequence.to_ne_bytes());

                bytes
            }
            Self::InternAtom(atom) => {
                let reply = InternAtomReply {
                    atom,
                    length: 0,
                    sequence,
                };
                let mut bytes = reply.serialize().to_vec();
                bytes.resize(32, 0);

                bytes
            }
            Self::GetProperty(mut reply) => {
                reply.sequence = sequence;
                let reply_byte_count = 32 + reply.length as usize * 4;
                let mut bytes = reply.serialize();
                bytes.resize(reply_byte_count, 0);

                bytes
            }
            Self::PropertyNotify(mut event) => {
                event.sequence = sequence;
                let mut bytes = event.serialize().to_vec();
                bytes.resize(32, 0);

                bytes
            }
            Self::SelectionNotify(mut event) => {
                event.sequence = sequence;
                let mut bytes = event.serialize().to_vec();
                bytes.resize(32, 0);

                bytes
            }
        };
        server_stream
            .write_all(&bytes)
            .expect("test X11 server should send scripted response");
    }
}

#[test]
fn test_read_target_waits_for_delayed_selection_event() {
    // Arrange
    let atoms = test_atoms();
    let target_format = atoms.UTF8_STRING;
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(CONVERT_SELECTION_REQUEST, Vec::new()),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, target_format, b"delayed".to_vec())],
        ),
    ];
    let (mut server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };
    let mut wait_call_count = 0;

    // Act
    let bytes = clipboard
        .read_target_with_event_waiter(target_format, |_, _| {
            wait_call_count += 1;
            server.send_response(selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            ));

            Ok(true)
        })
        .expect("delayed X11 selection should be read");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(bytes, b"delayed");
    assert_eq!(wait_call_count, 1);
}

#[test]
fn test_read_target_reassembles_incremental_transfer() {
    // Arrange
    let atoms = test_atoms();
    let target_format = atoms.UTF8_STRING;
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            )],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, atoms.INCR, Vec::new())],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![
                property_response(32, atoms.INCR, 8_u32.to_ne_bytes().to_vec()),
                property_notify_response(atoms),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![
                property_response(8, target_format, b"incremental".to_vec()),
                property_notify_response(atoms),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, target_format, Vec::new())],
        ),
    ];
    let (server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let bytes = clipboard
        .read_target(target_format)
        .expect("scripted X11 INCR transfer should complete");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(bytes, b"incremental");
}

#[test]
fn test_wait_for_x11_event_reports_ready_connection() {
    // Arrange
    let atoms = test_atoms();
    let (mut server, connection) = X11TestServer::start(Vec::new());
    server.send_response(selection_response(
        atoms,
        atoms.UTF8_STRING,
        atoms.AGENTTY_CLIPBOARD,
    ));
    let timeout_end = Instant::now() + Duration::from_secs(1);

    // Act
    let is_ready = X11Clipboard::wait_for_x11_event(&connection, timeout_end)
        .expect("readable test connection should poll successfully");
    drop(connection);
    server.finish();

    // Assert
    assert!(is_ready);
}

#[test]
fn test_wait_for_x11_event_returns_false_without_ready_events() {
    // Arrange
    let timeout_end = Instant::now() + Duration::from_secs(1);

    // Act
    let is_ready = X11Clipboard::wait_for_x11_event_with_poller(timeout_end, empty_poller)
        .expect("empty poll result should not fail");

    // Assert
    assert!(!is_ready);
}

#[test]
fn test_wait_for_x11_event_returns_false_after_deadline() {
    // Arrange
    let timeout_end = Instant::now();

    // Act
    let is_ready = X11Clipboard::wait_for_x11_event_with_poller(timeout_end, empty_poller)
        .expect("expired deadline should not fail");

    // Assert
    assert!(!is_ready);
}

#[test]
fn test_wait_for_x11_event_reports_poll_failure() {
    // Arrange
    let timeout_end = Instant::now() + Duration::from_secs(1);

    // Act
    let result = X11Clipboard::wait_for_x11_event_with_poller(timeout_end, |_| {
        Err(rustix::io::Errno::INVAL)
    });

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to wait for X11 clipboard events")
    ));
}

#[test]
fn test_poll_timeout_until_returns_remaining_duration() {
    // Arrange
    let now = Instant::now();
    let timeout_end = now + Duration::from_millis(1500);

    // Act
    let timeout =
        X11Clipboard::poll_timeout_until(now, timeout_end).expect("deadline should be in future");

    // Assert
    assert_eq!(timeout.tv_sec, 1);
    assert_eq!(timeout.tv_nsec, 500_000_000);
}

#[test]
fn test_poll_timeout_until_returns_none_after_deadline() {
    // Arrange
    let now = Instant::now();
    let timeout_end = now;

    // Act
    let timeout = X11Clipboard::poll_timeout_until(now, timeout_end);

    // Assert
    assert_eq!(timeout, None);
}

#[test]
fn test_next_incr_timeout_uses_segment_timeout_without_transfer_cap() {
    // Arrange
    let now = Instant::now();

    // Act
    let timeout_end = X11Clipboard::next_incr_timeout(now, None);

    // Assert
    assert_eq!(timeout_end, now + INCR_SEGMENT_TIMEOUT);
}

#[test]
fn test_next_incr_timeout_caps_segment_timeout_to_transfer_deadline() {
    // Arrange
    let now = Instant::now();
    let transfer_timeout_end = now + Duration::from_millis(50);

    // Act
    let timeout_end = X11Clipboard::next_incr_timeout(now, Some(transfer_timeout_end));

    // Assert
    assert_eq!(timeout_end, transfer_timeout_end);
}

#[test]
fn test_property_payload_limit_accepts_payload_within_limit() {
    // Arrange
    let property = GetPropertyReply {
        bytes_after: 4,
        format: 8,
        length: 1,
        sequence: 0,
        type_: 0,
        value: vec![0],
        value_len: 1,
    };

    // Act
    let result = X11Clipboard::ensure_property_payload_within_limit(&property);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_property_payload_limit_rejects_payload_above_limit() {
    // Arrange
    let property = GetPropertyReply {
        bytes_after: MAX_CLIPBOARD_BYTE_COUNT_U32,
        format: 8,
        length: 0,
        sequence: 0,
        type_: 0,
        value: vec![0],
        value_len: 1,
    };

    // Act
    let result = X11Clipboard::ensure_property_payload_within_limit(&property);

    // Assert
    assert!(matches!(result, Err(ClipboardError::Backend { .. })));
}

#[test]
fn test_minimum_incr_byte_count_reads_first_header_value() {
    // Arrange
    let minimum_byte_count = 4096_u32;
    let property = GetPropertyReply {
        bytes_after: 0,
        format: 32,
        length: 1,
        sequence: 0,
        type_: 0,
        value: minimum_byte_count.to_ne_bytes().to_vec(),
        value_len: 1,
    };

    // Act
    let result = X11Clipboard::minimum_incr_byte_count(&property);

    // Assert
    assert_eq!(result, Some(minimum_byte_count));
}

#[test]
fn test_latin1_bytes_to_string_maps_bytes_directly_to_unicode_scalars() {
    // Arrange
    let bytes = vec![b'a', 0xE9, b'z'];

    // Act
    let text = X11Clipboard::latin1_bytes_to_string(bytes);

    // Assert
    assert_eq!(text, "a\u{e9}z");
}

#[test]
fn test_utf8_bytes_to_string_decodes_valid_utf8() {
    // Arrange
    let bytes = "clipboard text".as_bytes().to_vec();

    // Act
    let text = X11Clipboard::utf8_bytes_to_string(bytes)
        .expect("valid UTF-8 clipboard text should decode");

    // Assert
    assert_eq!(text, "clipboard text");
}

#[test]
fn test_utf8_bytes_to_string_reports_backend_failure_for_invalid_utf8() {
    // Arrange
    let bytes = vec![0xFF];

    // Act
    let result = X11Clipboard::utf8_bytes_to_string(bytes);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to decode X11 clipboard text as UTF-8")
    ));
}

#[test]
fn test_read_text_decodes_utf8_target() {
    // Arrange
    let atoms = test_atoms();
    let steps = available_target_steps(
        atoms,
        atoms.UTF8_STRING,
        atoms.UTF8_STRING,
        "clipboard text".as_bytes().to_vec(),
    );
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let text = clipboard
        .read_text()
        .expect("scripted UTF-8 X11 text should decode");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(text, "clipboard text");
}

#[test]
fn test_read_text_decodes_string_target_as_latin1() {
    // Arrange
    let atoms = test_atoms();
    let mut steps = Vec::new();
    for target_format in [
        atoms.UTF8_STRING,
        atoms.UTF8_MIME_LOWER,
        atoms.UTF8_MIME_UPPER,
    ] {
        steps.extend(unavailable_target_steps(atoms, target_format));
    }
    steps.extend(available_target_steps(
        atoms,
        atoms.STRING,
        atoms.STRING,
        vec![0xE9],
    ));
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let text = clipboard
        .read_text()
        .expect("scripted Latin-1 X11 text should decode");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(text, "\u{e9}");
}

#[test]
fn test_incr_transfer_reassembles_chunks_and_resets_on_finish() {
    // Arrange
    let mut transfer = IncrTransfer::default();
    transfer
        .reserve_at_least(8)
        .expect("small reservation should fit");

    // Act
    transfer
        .push_chunk(b"abc".to_vec())
        .expect("first chunk should fit");
    transfer
        .push_chunk(b"def".to_vec())
        .expect("second chunk should fit");
    let bytes = transfer.finish();

    // Assert
    assert_eq!(bytes, b"abcdef");
    assert_eq!(transfer.finish(), Vec::<u8>::new());
}

#[test]
fn test_incr_transfer_caps_advertised_reservation() {
    // Arrange
    let advertised_byte_count = MAX_CLIPBOARD_BYTE_COUNT;

    // Act
    let reservation_byte_count = IncrTransfer::capped_reservation(advertised_byte_count);

    // Assert
    assert_eq!(reservation_byte_count, INCR_RESERVATION_BYTE_CAP);
}

#[test]
fn test_incr_transfer_rejects_advertised_payload_above_limit() {
    // Arrange
    let mut transfer = IncrTransfer::default();
    let advertised_byte_count = MAX_CLIPBOARD_BYTE_COUNT_U32 + 1;

    // Act
    let result = transfer.reserve_at_least(advertised_byte_count);

    // Assert
    assert!(matches!(result, Err(ClipboardError::Backend { .. })));
}

#[test]
fn test_checked_clipboard_byte_count_rejects_payload_above_limit() {
    // Arrange
    let current_byte_count = MAX_CLIPBOARD_BYTE_COUNT;
    let additional_byte_count = 1;

    // Act
    let result = checked_clipboard_byte_count(current_byte_count, additional_byte_count);

    // Assert
    assert!(matches!(result, Err(ClipboardError::Backend { .. })));
}

#[test]
fn test_new_reports_unavailable_without_display_in_child_process() {
    if std::env::var_os(X11_CONSTRUCTOR_CHILD_ENV).is_some() {
        // Act
        let result = X11Clipboard::new();

        // Assert
        assert!(matches!(
            result,
            Err(ClipboardError::Unavailable { reason })
                if reason.starts_with("X11 clipboard connection failed: ")
        ));

        return;
    }

    // Arrange
    let current_test_binary =
        std::env::current_exe().expect("current test binary path should be available");

    // Act
    let output = Command::new(current_test_binary)
        .args([
            "--exact",
            "backend::x11::tests::test_new_reports_unavailable_without_display_in_child_process",
            "--nocapture",
        ])
        .env(X11_CONSTRUCTOR_CHILD_ENV, "1")
        .env_remove("DISPLAY")
        .output()
        .expect("X11 constructor child test should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Assert
    assert!(
        output.status.success(),
        "X11 constructor child test failed: {stderr}"
    );
}

#[test]
fn test_from_connect_result_creates_window_and_loads_atoms() {
    // Arrange
    let mut steps = vec![server_step(CREATE_WINDOW_REQUEST, Vec::new())];
    for atom in 100..112 {
        steps.push(server_step(
            INTERN_ATOM_REQUEST,
            vec![X11ServerResponse::InternAtom(atom)],
        ));
    }
    let (server, connection) = X11TestServer::start(steps);

    // Act
    let clipboard = X11Clipboard::from_connect_result(Ok((connection, 0)))
        .expect("scripted X11 setup should succeed");
    let atoms = clipboard.atoms;
    let window_id = clipboard.window_id;
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(window_id, TEST_RESOURCE_ID_BASE);
    assert_eq!(atoms.CLIPBOARD, 100);
    assert_eq!(atoms.UTF8_STRING, 103);
    assert_eq!(atoms.AGENTTY_CLIPBOARD, 111);
}

#[test]
fn test_from_connect_result_reports_failed_connection_as_unavailable() {
    // Arrange
    let connect_result = Err(ConnectError::InvalidScreen);

    // Act
    let result = X11Clipboard::from_connect_result(connect_result);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason.starts_with("X11 clipboard connection failed: ")
    ));
}

#[test]
fn test_from_connection_reports_missing_screen() {
    // Arrange
    let connection = X11TestServer::start_disconnected();

    // Act
    let result = X11Clipboard::from_connection(connection, 1);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason }) if reason == "X11 screen was not found"
    ));
}

#[test]
fn test_from_connection_reports_exhausted_window_ids() {
    // Arrange
    let setup = Setup {
        resource_id_mask: 1,
        ..X11TestServer::default_setup()
    };
    let (server, connection) = X11TestServer::start_with_setup(setup, Vec::new());
    server.finish();
    for _ in 0..2 {
        connection
            .generate_id()
            .expect("the two advertised test ids should be available");
    }

    // Act
    let result = X11Clipboard::from_connection(connection, 0);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to allocate X11 window id: ")
    ));
}

#[test]
fn test_from_connection_reports_lost_connection_at_each_setup_request() {
    // Arrange
    let cases = [
        (Some(32), "failed to create X11 clipboard window: "),
        (Some(40), "failed to request X11 clipboard atoms: "),
        (None, "failed to load X11 clipboard atoms: "),
    ];

    for (free_byte_count, expected_reason_prefix) in cases {
        let connection = X11TestServer::start_disconnected();
        if let Some(free_byte_count) = free_byte_count {
            fill_write_buffer(&connection, free_byte_count);
        }

        // Act
        let result = X11Clipboard::from_connection(connection, 0);

        // Assert
        assert!(
            matches!(
                &result,
                Err(ClipboardError::Backend { reason })
                    if reason.starts_with(expected_reason_prefix)
            ),
            "expected `{expected_reason_prefix}` failure, got {:?}",
            result.err()
        );
    }
}

#[test]
fn test_read_target_reports_lost_connection_at_each_selection_request() {
    // Arrange
    let atoms = test_atoms();
    let cases = [
        (Some(8), "failed to clear X11 clipboard property: "),
        (Some(16), "failed to request X11 clipboard selection: "),
        (None, "failed to flush X11 clipboard request: "),
    ];

    for (free_byte_count, expected_reason_prefix) in cases {
        let connection = X11TestServer::start_disconnected();
        if let Some(free_byte_count) = free_byte_count {
            fill_write_buffer(&connection, free_byte_count);
        }
        let clipboard = X11Clipboard {
            atoms,
            connection,
            window_id: TEST_WINDOW_ID,
        };

        // Act
        let result = clipboard.read_target_with_event_waiter(atoms.UTF8_STRING, |_, _| Ok(true));

        // Assert
        assert!(
            matches!(
                &result,
                Err(ClipboardError::Backend { reason })
                    if reason.starts_with(expected_reason_prefix)
            ),
            "expected `{expected_reason_prefix}` failure, got {result:?}"
        );
    }
}

#[test]
fn test_read_target_reports_event_poll_failure_after_server_disconnect() {
    // Arrange
    let atoms = test_atoms();
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(CONVERT_SELECTION_REQUEST, Vec::new()),
    ];
    let (server, connection) = X11TestServer::start(steps);
    let mut server = Some(server);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let result = clipboard.read_target_with_event_waiter(atoms.UTF8_STRING, |_, _| {
        if let Some(server) = server.take() {
            server.finish();
        }

        Ok(true)
    });

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to poll X11 clipboard events: ")
    ));
}

#[test]
fn test_read_target_returns_content_unavailable_when_event_wait_expires() {
    // Arrange
    let atoms = test_atoms();
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(CONVERT_SELECTION_REQUEST, Vec::new()),
    ];
    let (server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };
    let mut wait_call_count = 0;

    // Act
    let result = clipboard.read_target_with_event_waiter(atoms.UTF8_STRING, |_, _| {
        wait_call_count += 1;

        Ok(false)
    });
    drop(clipboard);
    server.finish();

    // Assert
    assert!(matches!(result, Err(ClipboardError::ContentUnavailable)));
    assert_eq!(wait_call_count, 1);
}

#[test]
fn test_read_target_ignores_unrelated_events_during_incremental_transfer() {
    // Arrange
    let atoms = test_atoms();
    let target_format = atoms.UTF8_STRING;
    let other_selection = X11ServerResponse::SelectionNotify(SelectionNotifyEvent {
        property: atoms.AGENTTY_CLIPBOARD,
        requestor: TEST_WINDOW_ID,
        response_type: SELECTION_NOTIFY_EVENT,
        selection: atoms.TARGETS,
        target: target_format,
        ..SelectionNotifyEvent::default()
    });
    let deleted_property = X11ServerResponse::PropertyNotify(PropertyNotifyEvent {
        atom: atoms.AGENTTY_CLIPBOARD,
        response_type: PROPERTY_NOTIFY_EVENT,
        state: Property::DELETE,
        window: TEST_WINDOW_ID,
        ..PropertyNotifyEvent::default()
    });
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![
                property_notify_response(atoms),
                other_selection,
                selection_response(atoms, target_format, atoms.AGENTTY_CLIPBOARD),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, atoms.INCR, Vec::new())],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![
                property_response(8, atoms.INCR, Vec::new()),
                selection_response(atoms, target_format, atoms.AGENTTY_CLIPBOARD),
                deleted_property,
                property_notify_response(atoms),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![
                property_response(8, target_format, b"chunk".to_vec()),
                property_notify_response(atoms),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, target_format, Vec::new())],
        ),
    ];
    let (server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let bytes = clipboard
        .read_target(target_format)
        .expect("scripted X11 INCR transfer should complete");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(bytes, b"chunk");
}

#[test]
fn test_read_target_reports_property_reply_error() {
    // Arrange
    let atoms = test_atoms();
    let target_format = atoms.UTF8_STRING;
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            )],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![X11ServerResponse::Error(X11_BAD_WINDOW_ERROR_CODE)],
        ),
    ];
    let (server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let result = clipboard.read_target(target_format);
    drop(clipboard);
    server.finish();

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to read X11 clipboard property: ")
    ));
}

#[test]
fn test_read_text_propagates_unexpected_property_type() {
    // Arrange
    let atoms = test_atoms();
    let steps = available_target_steps(atoms, atoms.UTF8_STRING, atoms.TEXT, b"text".to_vec());
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let result = clipboard.read_text();
    drop(clipboard);
    server.finish();

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason == "X11 clipboard owner returned an unexpected property type"
    ));
}

#[test]
fn test_read_file_list_decodes_uri_list_target() {
    // Arrange
    let atoms = test_atoms();
    let steps = available_target_steps(
        atoms,
        atoms.URI_LIST,
        atoms.URI_LIST,
        b"file:///tmp/image%201.png\r\n".to_vec(),
    );
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let paths = clipboard
        .read_file_list()
        .expect("scripted X11 URI list should parse");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(paths, vec![PathBuf::from("/tmp/image 1.png")]);
}

#[test]
fn test_read_file_list_reports_content_unavailable_without_paths() {
    // Arrange
    let atoms = test_atoms();
    let mut steps = unavailable_target_steps(atoms, atoms.URI_LIST);
    steps.extend(available_target_steps(
        atoms,
        atoms.URI_LIST,
        atoms.URI_LIST,
        b"# copied from a file manager\r\n".to_vec(),
    ));
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let missing_target = clipboard.read_file_list();
    let empty_uri_list = clipboard.read_file_list();
    drop(clipboard);
    server.finish();

    // Assert
    assert!(matches!(
        missing_target,
        Err(ClipboardError::ContentUnavailable)
    ));
    assert!(matches!(
        empty_uri_list,
        Err(ClipboardError::ContentUnavailable)
    ));
}

#[test]
fn test_read_image_rgba_decodes_png_target() {
    // Arrange
    let atoms = test_atoms();
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&[0, 0, 255, 255], 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG should encode");
    let steps = available_target_steps(atoms, atoms.PNG_MIME, atoms.PNG_MIME, png_bytes);
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let image_data = clipboard
        .read_image_rgba()
        .expect("scripted X11 PNG should decode");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(image_data.width, 1);
    assert_eq!(image_data.height, 1);
    assert_eq!(image_data.rgba_bytes, vec![0, 0, 255, 255]);
}

#[test]
fn test_checked_clipboard_byte_count_rejects_overflowing_payload() {
    // Arrange
    let current_byte_count = usize::MAX;
    let additional_byte_count = 1;

    // Act
    let result = checked_clipboard_byte_count(current_byte_count, additional_byte_count);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason }) if reason.contains(&usize::MAX.to_string())
    ));
}

#[test]
fn test_drop_tolerates_window_destroy_failure() {
    // Arrange
    let connection = X11TestServer::start_disconnected();
    fill_write_buffer(&connection, 4);
    let clipboard = X11Clipboard {
        atoms: test_atoms(),
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let result = panic::catch_unwind(AssertUnwindSafe(move || drop(clipboard)));

    // Assert
    assert!(result.is_ok());
}

/// Buffers one filler request so exactly `free_byte_count` bytes remain in
/// the connection's request buffer.
///
/// On a disconnected test server, the next request larger than that forces
/// a socket write that fails with a broken pipe.
fn fill_write_buffer(connection: &RustConnection, free_byte_count: usize) {
    const CHANGE_PROPERTY_HEADER_BYTE_COUNT: usize = 24;

    let filler =
        vec![
            0;
            X11RB_WRITE_BUFFER_BYTE_COUNT - CHANGE_PROPERTY_HEADER_BYTE_COUNT - free_byte_count
        ];
    let filler_length = u32::try_from(filler.len()).expect("test filler length should fit in u32");
    let property: Atom = 1;
    connection
        .change_property(
            PropMode::REPLACE,
            TEST_WINDOW_ID,
            property,
            property,
            8,
            filler_length,
            &filler,
        )
        .expect("test filler request should be buffered");
}

fn test_atoms() -> AtomCollection {
    AtomCollection {
        AGENTTY_CLIPBOARD: 12,
        CLIPBOARD: 1,
        INCR: 3,
        PNG_MIME: 11,
        STRING: 7,
        TARGETS: 2,
        TEXT: 8,
        TEXT_MIME: 9,
        URI_LIST: 10,
        UTF8_MIME_LOWER: 5,
        UTF8_MIME_UPPER: 6,
        UTF8_STRING: 4,
    }
}

fn server_step(expected_opcode: u8, responses: Vec<X11ServerResponse>) -> X11ServerStep {
    X11ServerStep {
        expected_opcode,
        responses,
    }
}

fn property_response(format: u8, type_: Atom, value: Vec<u8>) -> X11ServerResponse {
    let bytes_per_value = usize::from(format) / 8;
    assert_eq!(value.len() % bytes_per_value, 0);
    let value_len = u32::try_from(value.len() / bytes_per_value)
        .expect("test X11 property value length should fit in u32");
    let length = u32::try_from(value.len().div_ceil(4))
        .expect("test X11 property reply length should fit in u32");

    X11ServerResponse::GetProperty(GetPropertyReply {
        bytes_after: 0,
        format,
        length,
        sequence: 0,
        type_,
        value,
        value_len,
    })
}

fn selection_response(atoms: AtomCollection, target: Atom, property: Atom) -> X11ServerResponse {
    X11ServerResponse::SelectionNotify(SelectionNotifyEvent {
        property,
        requestor: TEST_WINDOW_ID,
        response_type: SELECTION_NOTIFY_EVENT,
        selection: atoms.CLIPBOARD,
        target,
        ..SelectionNotifyEvent::default()
    })
}

fn property_notify_response(atoms: AtomCollection) -> X11ServerResponse {
    X11ServerResponse::PropertyNotify(PropertyNotifyEvent {
        atom: atoms.AGENTTY_CLIPBOARD,
        response_type: PROPERTY_NOTIFY_EVENT,
        state: Property::NEW_VALUE,
        window: TEST_WINDOW_ID,
        ..PropertyNotifyEvent::default()
    })
}

fn available_target_steps(
    atoms: AtomCollection,
    target_format: Atom,
    property_type: Atom,
    value: Vec<u8>,
) -> Vec<X11ServerStep> {
    vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            )],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, property_type, value)],
        ),
    ]
}

fn unavailable_target_steps(atoms: AtomCollection, target_format: Atom) -> Vec<X11ServerStep> {
    vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(atoms, target_format, NONE)],
        ),
    ]
}

fn empty_poller(_: &Timespec) -> rustix::io::Result<usize> {
    let immediate_timeout = Timespec {
        tv_nsec: 0,
        tv_sec: 0,
    };

    event::poll(&mut [], Some(&immediate_timeout))
}
