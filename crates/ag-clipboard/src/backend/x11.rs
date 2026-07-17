use std::path::PathBuf;
use std::time::{Duration, Instant};

use image::ImageFormat;
use rustix::event::{self, PollFd, PollFlags, Timespec};
use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, ConnectionExt, GetPropertyReply, Property, PropertyNotifyEvent, SelectionNotifyEvent,
    Time,
};
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{CreateWindowAux, EventMask, WindowClass};
use x11rb::rust_connection::RustConnection;
#[cfg(target_os = "linux")]
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

use super::ClipboardBackend;
use crate::{ClipboardError, RgbaImageData, format, uri};

const INCR_RESERVATION_BYTE_CAP: usize = 16 * 1024 * 1024;
const INCR_SEGMENT_TIMEOUT: Duration = Duration::from_secs(1);
const INCR_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const INCR_HEADER_LONG_LENGTH: u32 = 1;
const MAX_CLIPBOARD_BYTE_COUNT: usize = 64 * 1024 * 1024;
const MAX_CLIPBOARD_PROPERTY_LONG_LENGTH: u32 = 16 * 1024 * 1024;
const SELECTION_TIMEOUT: Duration = Duration::from_secs(4);

x11rb::atom_manager! {
    AtomCollection: AtomCollectionCookie {
        CLIPBOARD,
        TARGETS,
        INCR,
        UTF8_STRING,
        UTF8_MIME_LOWER: b"text/plain;charset=utf-8",
        UTF8_MIME_UPPER: b"text/plain;charset=UTF-8",
        STRING,
        TEXT,
        TEXT_MIME: b"text/plain",
        URI_LIST: b"text/uri-list",
        PNG_MIME: b"image/png",
        AGENTTY_CLIPBOARD,
    }
}

pub(crate) struct X11Clipboard {
    atoms: AtomCollection,
    connection: RustConnection,
    window_id: u32,
}

impl X11Clipboard {
    #[cfg(target_os = "linux")]
    pub(crate) fn new() -> Result<Self, ClipboardError> {
        let (connection, screen_number) =
            RustConnection::connect(None).map_err(|error| ClipboardError::Unavailable {
                reason: format!("X11 clipboard connection failed: {error}"),
            })?;
        let screen =
            connection
                .setup()
                .roots
                .get(screen_number)
                .ok_or_else(|| ClipboardError::Backend {
                    reason: "X11 screen was not found".to_string(),
                })?;
        let window_id = connection
            .generate_id()
            .map_err(|error| ClipboardError::backend("failed to allocate X11 window id", error))?;
        let event_mask = EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY;

        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window_id,
                screen.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::COPY_FROM_PARENT,
                COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(event_mask),
            )
            .map_err(|error| {
                ClipboardError::backend("failed to create X11 clipboard window", error)
            })?;
        connection.flush().map_err(|error| {
            ClipboardError::backend("failed to flush X11 clipboard window", error)
        })?;
        let atoms = AtomCollection::new(&connection)
            .map_err(|error| {
                ClipboardError::backend("failed to request X11 clipboard atoms", error)
            })?
            .reply()
            .map_err(|error| {
                ClipboardError::backend("failed to load X11 clipboard atoms", error)
            })?;

        Ok(Self {
            atoms,
            connection,
            window_id,
        })
    }

    fn read_clipboard_data(
        &self,
        target_formats: &[Atom],
    ) -> Result<ClipboardData, ClipboardError> {
        for target_format in target_formats {
            match self.read_target(*target_format) {
                Ok(bytes) => {
                    return Ok(ClipboardData {
                        bytes,
                        format: *target_format,
                    });
                }
                Err(ClipboardError::ContentUnavailable) => {}
                Err(error) => return Err(error),
            }
        }

        Err(ClipboardError::ContentUnavailable)
    }

    fn read_target(&self, target_format: Atom) -> Result<Vec<u8>, ClipboardError> {
        self.read_target_with_event_waiter(target_format, Self::wait_for_x11_event)
    }

    fn read_target_with_event_waiter(
        &self,
        target_format: Atom,
        mut wait_for_event: impl FnMut(&RustConnection, Instant) -> Result<bool, ClipboardError>,
    ) -> Result<Vec<u8>, ClipboardError> {
        self.connection
            .delete_property(self.window_id, self.atoms.AGENTTY_CLIPBOARD)
            .map_err(|error| {
                ClipboardError::backend("failed to clear X11 clipboard property", error)
            })?;
        self.connection
            .convert_selection(
                self.window_id,
                self.atoms.CLIPBOARD,
                target_format,
                self.atoms.AGENTTY_CLIPBOARD,
                Time::CURRENT_TIME,
            )
            .map_err(|error| {
                ClipboardError::backend("failed to request X11 clipboard selection", error)
            })?;
        self.connection.flush().map_err(|error| {
            ClipboardError::backend("failed to flush X11 clipboard request", error)
        })?;

        let mut timeout_end = Instant::now() + SELECTION_TIMEOUT;
        let mut is_incr_transfer = false;
        let mut incr_transfer_timeout_end = None;
        let mut incr_transfer = IncrTransfer::default();

        while Instant::now() < timeout_end {
            let Some(event) = self.connection.poll_for_event().map_err(|error| {
                ClipboardError::backend("failed to poll X11 clipboard events", error)
            })?
            else {
                if !wait_for_event(&self.connection, timeout_end)? {
                    break;
                }
                continue;
            };

            match event {
                Event::SelectionNotify(event) => match self.handle_selection_notify(
                    event,
                    target_format,
                    &mut is_incr_transfer,
                    &mut incr_transfer,
                )? {
                    SelectionRead::Complete(bytes) => return Ok(bytes),
                    SelectionRead::IncrStarted => {
                        let now = Instant::now();
                        incr_transfer_timeout_end = Some(now + INCR_TRANSFER_TIMEOUT);
                        timeout_end = Self::next_incr_timeout(now, incr_transfer_timeout_end);
                    }
                    SelectionRead::Ignored => {}
                },
                Event::PropertyNotify(event)
                    if self.handle_property_notify(
                        &event,
                        target_format,
                        is_incr_transfer,
                        &mut incr_transfer,
                        incr_transfer_timeout_end,
                        &mut timeout_end,
                    )? =>
                {
                    return Ok(incr_transfer.finish());
                }
                _ => {}
            }
        }

        Err(ClipboardError::ContentUnavailable)
    }

    fn handle_selection_notify(
        &self,
        event: SelectionNotifyEvent,
        target_format: Atom,
        is_incr_transfer: &mut bool,
        incr_transfer: &mut IncrTransfer,
    ) -> Result<SelectionRead, ClipboardError> {
        if event.property == NONE || event.target != target_format {
            return Err(ClipboardError::ContentUnavailable);
        }
        if event.selection != self.atoms.CLIPBOARD {
            return Ok(SelectionRead::Ignored);
        }
        if *is_incr_transfer {
            return Ok(SelectionRead::Ignored);
        }

        let mut property = self
            .connection
            .get_property(
                true,
                event.requestor,
                event.property,
                event.target,
                0,
                MAX_CLIPBOARD_PROPERTY_LONG_LENGTH,
            )
            .map_err(|error| {
                ClipboardError::backend("failed to read X11 clipboard property", error)
            })?
            .reply()
            .map_err(|error| {
                ClipboardError::backend("failed to receive X11 clipboard property", error)
            })?;
        Self::ensure_property_payload_within_limit(&property)?;

        if property.type_ == target_format {
            return Ok(SelectionRead::Complete(property.value));
        }
        if property.type_ != self.atoms.INCR {
            return Err(ClipboardError::Backend {
                reason: "X11 clipboard owner returned an unexpected property type".to_string(),
            });
        }

        property = self
            .connection
            .get_property(
                true,
                event.requestor,
                event.property,
                self.atoms.INCR,
                0,
                INCR_HEADER_LONG_LENGTH,
            )
            .map_err(|error| ClipboardError::backend("failed to read X11 INCR header", error))?
            .reply()
            .map_err(|error| ClipboardError::backend("failed to receive X11 INCR header", error))?;
        if let Some(minimum_byte_count) = Self::minimum_incr_byte_count(&property) {
            incr_transfer.reserve_at_least(minimum_byte_count)?;
        }
        *is_incr_transfer = true;

        Ok(SelectionRead::IncrStarted)
    }

    fn handle_property_notify(
        &self,
        event: &PropertyNotifyEvent,
        target_format: Atom,
        is_incr_transfer: bool,
        incr_transfer: &mut IncrTransfer,
        incr_transfer_timeout_end: Option<Instant>,
        timeout_end: &mut Instant,
    ) -> Result<bool, ClipboardError> {
        if event.atom != self.atoms.AGENTTY_CLIPBOARD || event.state != Property::NEW_VALUE {
            return Ok(false);
        }
        if !is_incr_transfer {
            return Ok(false);
        }

        let property = self
            .connection
            .get_property(
                true,
                event.window,
                event.atom,
                target_format,
                0,
                MAX_CLIPBOARD_PROPERTY_LONG_LENGTH,
            )
            .map_err(|error| ClipboardError::backend("failed to read X11 INCR segment", error))?
            .reply()
            .map_err(|error| {
                ClipboardError::backend("failed to receive X11 INCR segment", error)
            })?;
        Self::ensure_property_payload_within_limit(&property)?;
        if property.value_len == 0 {
            return Ok(true);
        }

        incr_transfer.push_chunk(property.value)?;
        *timeout_end = Self::next_incr_timeout(Instant::now(), incr_transfer_timeout_end);

        Ok(false)
    }

    fn wait_for_x11_event(
        connection: &RustConnection,
        timeout_end: Instant,
    ) -> Result<bool, ClipboardError> {
        let mut poll_fds = [PollFd::new(connection.stream(), PollFlags::IN)];

        Self::wait_for_x11_event_with_poller(timeout_end, |timeout| {
            event::poll(&mut poll_fds, Some(timeout))
        })
    }

    fn wait_for_x11_event_with_poller(
        timeout_end: Instant,
        poll_events: impl FnOnce(&Timespec) -> rustix::io::Result<usize>,
    ) -> Result<bool, ClipboardError> {
        let Some(timeout) = Self::poll_timeout_until(Instant::now(), timeout_end) else {
            return Ok(false);
        };
        let ready_count = poll_events(&timeout).map_err(|error| {
            ClipboardError::backend("failed to wait for X11 clipboard events", error)
        })?;

        Ok(ready_count > 0)
    }

    fn poll_timeout_until(now: Instant, timeout_end: Instant) -> Option<Timespec> {
        if now >= timeout_end {
            return None;
        }
        let duration = timeout_end.checked_duration_since(now)?;

        Some(Self::duration_to_timespec(duration))
    }

    fn duration_to_timespec(duration: Duration) -> Timespec {
        Timespec::try_from(duration).unwrap_or(Timespec {
            tv_sec: i64::MAX,
            tv_nsec: 999_999_999,
        })
    }

    fn next_incr_timeout(now: Instant, transfer_timeout_end: Option<Instant>) -> Instant {
        let segment_timeout_end = now + INCR_SEGMENT_TIMEOUT;

        transfer_timeout_end.map_or(segment_timeout_end, |deadline| {
            segment_timeout_end.min(deadline)
        })
    }

    fn ensure_property_payload_within_limit(
        property: &GetPropertyReply,
    ) -> Result<(), ClipboardError> {
        checked_clipboard_byte_count(property.value.len(), property.bytes_after as usize)?;

        Ok(())
    }

    fn minimum_incr_byte_count(property: &GetPropertyReply) -> Option<u32> {
        property.value32().and_then(|mut values| values.next())
    }

    fn latin1_bytes_to_string(bytes: Vec<u8>) -> String {
        bytes.into_iter().map(char::from).collect()
    }

    fn utf8_bytes_to_string(bytes: Vec<u8>) -> Result<String, ClipboardError> {
        String::from_utf8(bytes).map_err(|error| {
            ClipboardError::backend("failed to decode X11 clipboard text as UTF-8", error)
        })
    }
}

impl ClipboardBackend for X11Clipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        let target_formats = [
            self.atoms.UTF8_STRING,
            self.atoms.UTF8_MIME_LOWER,
            self.atoms.UTF8_MIME_UPPER,
            self.atoms.STRING,
            self.atoms.TEXT,
            self.atoms.TEXT_MIME,
        ];
        let clipboard_data = self.read_clipboard_data(&target_formats)?;
        if clipboard_data.format == self.atoms.STRING {
            return Ok(Self::latin1_bytes_to_string(clipboard_data.bytes));
        }

        Self::utf8_bytes_to_string(clipboard_data.bytes)
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        let clipboard_data = self.read_clipboard_data(&[self.atoms.URI_LIST])?;
        let paths = uri::paths_from_uri_list(&clipboard_data.bytes);
        if paths.is_empty() {
            return Err(ClipboardError::ContentUnavailable);
        }

        Ok(paths)
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        let clipboard_data = self.read_clipboard_data(&[self.atoms.PNG_MIME])?;

        format::decode_image_rgba(&clipboard_data.bytes, ImageFormat::Png)
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        if self.connection.destroy_window(self.window_id).is_ok() {
            let _ = self.connection.flush();
        }
    }
}

struct ClipboardData {
    bytes: Vec<u8>,
    format: Atom,
}

enum SelectionRead {
    Complete(Vec<u8>),
    Ignored,
    IncrStarted,
}

#[derive(Default)]
struct IncrTransfer {
    bytes: Vec<u8>,
}

impl IncrTransfer {
    fn reserve_at_least(&mut self, minimum_byte_count: u32) -> Result<(), ClipboardError> {
        let minimum_byte_count = checked_clipboard_byte_count(0, minimum_byte_count as usize)?;

        self.bytes
            .reserve_exact(Self::capped_reservation(minimum_byte_count));

        Ok(())
    }

    fn push_chunk(&mut self, chunk: Vec<u8>) -> Result<(), ClipboardError> {
        checked_clipboard_byte_count(self.bytes.len(), chunk.len())?;
        self.bytes.extend(chunk);

        Ok(())
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    fn capped_reservation(minimum_byte_count: usize) -> usize {
        minimum_byte_count.min(INCR_RESERVATION_BYTE_CAP)
    }
}

fn checked_clipboard_byte_count(
    current_byte_count: usize,
    additional_byte_count: usize,
) -> Result<usize, ClipboardError> {
    let byte_count = current_byte_count
        .checked_add(additional_byte_count)
        .ok_or_else(|| clipboard_payload_too_large(usize::MAX))?;
    if byte_count > MAX_CLIPBOARD_BYTE_COUNT {
        return Err(clipboard_payload_too_large(byte_count));
    }

    Ok(byte_count)
}

fn clipboard_payload_too_large(byte_count: usize) -> ClipboardError {
    ClipboardError::Backend {
        reason: format!(
            "X11 clipboard payload exceeds {MAX_CLIPBOARD_BYTE_COUNT} byte limit ({byte_count} \
             bytes)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;

    use x11rb::protocol::xproto::{
        CONVERT_SELECTION_REQUEST, DELETE_PROPERTY_REQUEST, GET_PROPERTY_REQUEST,
        PROPERTY_NOTIFY_EVENT, PropertyNotifyEvent, SELECTION_NOTIFY_EVENT, Screen,
        SelectionNotifyEvent, Setup,
    };
    use x11rb::rust_connection::DefaultStream;
    use x11rb::x11_utils::Serialize;

    use super::*;

    const MAX_CLIPBOARD_BYTE_COUNT_U32: u32 = 64 * 1024 * 1024;
    const TEST_WINDOW_ID: u32 = 42;

    struct X11TestServer {
        server_thread: thread::JoinHandle<()>,
        writer: UnixStream,
    }

    impl X11TestServer {
        fn start(steps: Vec<X11ServerStep>) -> (Self, RustConnection) {
            let (client_stream, server_stream) =
                UnixStream::pair().expect("test X11 socket pair should open");
            let writer = server_stream
                .try_clone()
                .expect("test X11 server socket should clone");
            let server_thread = thread::spawn(move || Self::run(server_stream, steps));
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

        fn run(mut server_stream: UnixStream, steps: Vec<X11ServerStep>) {
            let mut setup_request = [0; 12];
            server_stream
                .read_exact(&mut setup_request)
                .expect("test X11 server should receive setup request");
            server_stream
                .write_all(&Self::setup_bytes())
                .expect("test X11 server should send setup response");

            for (step_index, step) in steps.into_iter().enumerate() {
                let opcode = Self::read_request_opcode(&mut server_stream);
                assert_eq!(opcode, step.expected_opcode);
                let sequence = u16::try_from(step_index + 1)
                    .expect("test X11 request sequence should fit in u16");
                for response in step.responses {
                    response.write_to(&mut server_stream, sequence);
                }
            }
        }

        fn setup_bytes() -> Vec<u8> {
            let mut setup = Setup {
                maximum_request_length: u16::MAX,
                protocol_major_version: 11,
                resource_id_base: 0x0100_0000,
                resource_id_mask: 0x00FF_FFFF,
                roots: vec![Screen {
                    root: 1,
                    ..Screen::default()
                }],
                status: 1,
                ..Setup::default()
            };
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
        GetProperty(GetPropertyReply),
        PropertyNotify(PropertyNotifyEvent),
        SelectionNotify(SelectionNotifyEvent),
    }

    impl X11ServerResponse {
        fn write_to(self, server_stream: &mut UnixStream, sequence: u16) {
            let bytes = match self {
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
        let timeout = X11Clipboard::poll_timeout_until(now, timeout_end)
            .expect("deadline should be in future");

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

    fn selection_response(
        atoms: AtomCollection,
        target: Atom,
        property: Atom,
    ) -> X11ServerResponse {
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
}
