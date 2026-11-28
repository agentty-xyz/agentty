//! Running-server fixture over the crate's in-memory doubles, linked to an
//! in-process relay, with a device that reaches it through that relay.

use std::error::Error;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use ag_noise::{Handshake, Session};
use ag_relay::{AdmissionPolicy, CONNECT_PATH_PREFIX, RelayConfig, RelayHandle, RelayLimits};
use ag_remote::protocol::{ClientFrame, ServerFrame};
use ag_remote::test_support::{MemoryDeviceStore, RecordingBackend};
use ag_remote::{
    DeviceStore, PROTOCOL_VERSION, RelayStatus, RemoteHost, RemoteRelayConfig, RemoteServerConfig,
    RemoteServerHandle, StaticKeypair,
};
use ag_session::{SessionBackend, SessionService};
use futures_util::{SinkExt, StreamExt};
use jsonschema::Validator;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, client_async};

/// One WebSocket to the relay.
pub(crate) type Socket = WebSocketStream<DeviceStream>;

/// Result type for tests and the helpers they share.
pub(crate) type TestResult<Value = ()> = Result<Value, Box<dyn Error>>;

/// Longest wait for an expected frame before a test fails.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest frame the test device reassembles, in bytes.
const MAX_FRAME_BYTES: usize = 1 << 20;

/// A device's encrypted stream to the server through the relay.
pub(crate) struct Client {
    key: StaticKeypair,
    session: Session,
    socket: Socket,
}

impl Client {
    /// Connects to `instance` through `relay` as the device holding `key`
    /// and runs the device handshake.
    pub(crate) async fn connect(
        relay: &RelayHandle,
        instance: &StaticKeypair,
        key: StaticKeypair,
    ) -> TestResult<Self> {
        let mut socket = open_device_stream(relay, instance).await?;
        let mut handshake = Handshake::device_initiator(&key, instance.public())?;
        socket
            .send(Message::Binary(handshake.write_message()?.into()))
            .await?;
        let second = next_binary(&mut socket).await?;
        handshake.read_message(&second)?;
        let session = handshake.into_session(MAX_FRAME_BYTES)?;

        Ok(Self {
            key,
            session,
            socket,
        })
    }

    /// Returns the key this device proves.
    pub(crate) fn key(&self) -> &StaticKeypair {
        &self.key
    }

    /// Encrypts and sends `text` as one frame.
    pub(crate) async fn send_text(&mut self, text: &str) -> TestResult {
        self.send_bytes(text.as_bytes()).await
    }

    /// Encrypts and sends `bytes` as one frame, text or not.
    pub(crate) async fn send_bytes(&mut self, bytes: &[u8]) -> TestResult {
        for message in self.session.seal(bytes)? {
            self.socket.send(Message::Binary(message.into())).await?;
        }

        Ok(())
    }

    /// Receives and decrypts the next frame without a client-side timeout,
    /// so a paused clock can advance to server deadlines.
    pub(crate) async fn next_untimed_frame(&mut self) -> TestResult<ServerFrame> {
        Ok(serde_json::from_str(&self.next_untimed_text().await?)?)
    }

    /// Receives and decrypts the next frame as the text the server sent,
    /// failing when it breaks the committed protocol schema.
    pub(crate) async fn next_untimed_text(&mut self) -> TestResult<String> {
        loop {
            let message = self.socket.next().await.ok_or("stream ended")??;
            if let Message::Binary(bytes) = message
                && let Some(frame) = self.session.open(&bytes)?
            {
                let text = String::from_utf8(frame)?;
                check_frame(&SERVER_FRAME_SCHEMA, &serde_json::from_str(&text)?)?;

                return Ok(text);
            }
        }
    }

    /// Closes the stream.
    pub(crate) async fn close(&mut self) -> TestResult {
        self.socket.close(None).await?;

        Ok(())
    }
}

/// A started server registered with its own relay, with handles on its
/// doubles.
pub(crate) struct TestServer {
    pub(crate) backend: Arc<RecordingBackend>,
    pub(crate) devices: Arc<MemoryDeviceStore>,
    pub(crate) handle: RemoteServerHandle,
    pub(crate) instance: StaticKeypair,
    pub(crate) relay: RelayHandle,
    pub(crate) revisions_tx: watch::Sender<u64>,
}

impl TestServer {
    /// Starts a relay and a server with revision `1` registered with it.
    pub(crate) async fn start() -> TestResult<Self> {
        Self::start_with(MemoryDeviceStore::default(), RecordingBackend::default()).await
    }

    /// Starts a relay and a server over `devices` and `backend` registered
    /// with it.
    pub(crate) async fn start_with(
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
    ) -> TestResult<Self> {
        Self::start_on(start_relay().await?, devices, backend).await
    }

    /// Starts a server over `devices` and `backend` registered with `relay`.
    pub(crate) async fn start_on(
        relay: RelayHandle,
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
    ) -> TestResult<Self> {
        let instance = StaticKeypair::generate()?;
        let backend = Arc::new(backend);
        let devices = Arc::new(devices);
        let (revisions_tx, revisions) = watch::channel(1);
        let host = RemoteHost {
            devices: Arc::clone(&devices) as Arc<dyn DeviceStore>,
            revisions,
            sessions: SessionService::new(Arc::clone(&backend) as Arc<dyn SessionBackend>),
        };
        let handle = ag_remote::start(
            RemoteServerConfig {
                relay: RemoteRelayConfig {
                    instance_key: instance.clone(),
                    url: relay_url(&relay),
                },
            },
            host,
        );
        wait_for_relay(&handle, RelayStatus::Connected).await?;

        Ok(Self {
            backend,
            devices,
            handle,
            instance,
            relay,
            revisions_tx,
        })
    }

    /// Opens a device stream with a fresh key without sending a frame.
    pub(crate) async fn connect(&self) -> TestResult<Client> {
        self.connect_as(StaticKeypair::generate()?).await
    }

    /// Opens a device stream as the device holding `key`.
    pub(crate) async fn connect_as(&self, key: StaticKeypair) -> TestResult<Client> {
        Client::connect(&self.relay, &self.instance, key).await
    }

    /// Pairs a new device, approving it through the handle, and returns its
    /// open client and device id.
    pub(crate) async fn pair(&self, device_name: &str) -> TestResult<(Client, String)> {
        let (mut client, request_id) = self.request_pairing(device_name).await?;
        if !self.handle.approve_pairing(request_id) {
            return Err("pairing request should be pending".into());
        }
        let ServerFrame::Paired { device_id, .. } = next_frame(&mut client).await? else {
            return Err("pairing should succeed".into());
        };

        Ok((client, device_id))
    }

    /// Redeems a fresh code and returns the waiting client with the id of
    /// its listed pairing request.
    pub(crate) async fn request_pairing(&self, device_name: &str) -> TestResult<(Client, u64)> {
        let code = self.handle.issue_pairing_code()?.code;
        let mut client = self.connect().await?;
        send(
            &mut client,
            &ClientFrame::Pair {
                code,
                device_name: device_name.to_string(),
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await?;
        let ServerFrame::AwaitingApproval { confirmation } = next_frame(&mut client).await? else {
            return Err("pairing should wait for approval".into());
        };
        let request_id = self
            .handle
            .status()
            .wait_for(|status| {
                status
                    .pairing_requests
                    .iter()
                    .any(|request| request.confirmation == confirmation)
            })
            .await?
            .pairing_requests
            .iter()
            .find(|request| request.confirmation == confirmation)
            .map(|request| request.id)
            .ok_or("pairing request should be listed")?;

        Ok((client, request_id))
    }

    /// Connects as the device holding `key`, sends a hello, and returns the
    /// server's first frame.
    pub(crate) async fn hello(&self, key: &StaticKeypair) -> TestResult<(Client, ServerFrame)> {
        let mut client = self.connect_as(key.clone()).await?;
        send(&mut client, &hello_frame()).await?;
        let frame = next_frame(&mut client).await?;

        Ok((client, frame))
    }
}

/// Starts a relay on a free loopback port.
pub(crate) async fn start_relay() -> TestResult<RelayHandle> {
    start_relay_limited(relay_limits()).await
}

/// Starts a relay on a free loopback port with `limits`.
pub(crate) async fn start_relay_limited(limits: RelayLimits) -> TestResult<RelayHandle> {
    let relay = ag_relay::start(relay_config(limits)).await?;

    Ok(relay)
}

/// Starts a relay on a free loopback port that registers only the
/// instances `admission` admits.
pub(crate) async fn start_relay_with_admission(
    admission: Arc<dyn AdmissionPolicy>,
) -> TestResult<RelayHandle> {
    let relay = ag_relay::start_with_admission(relay_config(relay_limits()), admission).await?;

    Ok(relay)
}

/// Settings of a test relay on a free loopback port.
fn relay_config(limits: RelayLimits) -> RelayConfig {
    RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: None,
        limits,
    }
}

/// Relay limits with frequent keepalive pings, so the server's handling of
/// them runs in every test.
pub(crate) fn relay_limits() -> RelayLimits {
    RelayLimits {
        ping_interval: Duration::from_millis(50),
        ..RelayLimits::default()
    }
}

/// Starts a server over `backend` that registers with the relay at
/// `relay_url` as `instance`, without waiting for the relay to register it.
pub(crate) fn start_server_linked_to(
    relay_url: String,
    instance: &StaticKeypair,
    backend: RecordingBackend,
) -> RemoteServerHandle {
    let (_revisions_tx, revisions) = watch::channel(1);
    let host = RemoteHost {
        devices: Arc::new(MemoryDeviceStore::default()) as Arc<dyn DeviceStore>,
        revisions,
        sessions: SessionService::new(Arc::new(backend) as Arc<dyn SessionBackend>),
    };

    ag_remote::start(
        RemoteServerConfig {
            relay: RemoteRelayConfig {
                instance_key: instance.clone(),
                url: relay_url,
            },
        },
        host,
    )
}

/// The base URL a server registers with `relay` under.
pub(crate) fn relay_url(relay: &RelayHandle) -> String {
    format!("ws://{}/", relay.local_addr())
}

/// Waits until the relay link of `handle` reports `expected`.
pub(crate) async fn wait_for_relay(
    handle: &RemoteServerHandle,
    expected: RelayStatus,
) -> TestResult {
    let mut status = handle.status();
    tokio::time::timeout(
        FRAME_TIMEOUT,
        status.wait_for(|status| status.relay == expected),
    )
    .await??;

    Ok(())
}

/// Opens a device stream toward `instance` through `relay`, before any
/// handshake.
pub(crate) async fn open_device_stream(
    relay: &RelayHandle,
    instance: &StaticKeypair,
) -> TestResult<Socket> {
    let url = format!(
        "ws://{}{CONNECT_PATH_PREFIX}{}",
        relay.local_addr(),
        instance.public().fingerprint()
    );
    let tcp = TcpStream::connect(relay.local_addr()).await?;
    let (socket, _) = client_async(url, DeviceStream { tcp }).await?;

    Ok(socket)
}

/// A device's connection to the relay that drops what it writes once the
/// relay has hung up, as writes to a departed peer are lost.
///
/// The relay ends a stream by sending its close frame and dropping the
/// connection without waiting for the device's echo. Before handing over
/// each received message, the WebSocket client writes its automatic
/// replies: a pong for every ping it read and the echo of a close frame.
/// Once the relay's socket has reset the connection, that write fails and
/// the client returns the failure in place of frames that already arrived,
/// such as the rejection sent before the close. Dropping the write lets the
/// device read those frames and then the end of the stream; a device that
/// still waits for an answer sees the stream end instead.
pub(crate) struct DeviceStream {
    tcp: TcpStream,
}

impl DeviceStream {
    /// Reports a write the hung-up relay refused as `written`; any other
    /// result passes through.
    fn drop_if_hung_up<Value>(result: io::Result<Value>, written: Value) -> io::Result<Value> {
        match result {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(written)
            }
            result => result,
        }
    }
}

impl AsyncRead for DeviceStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.tcp).poll_read(cx, buf)
    }
}

impl AsyncWrite for DeviceStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let written = ready!(Pin::new(&mut self.tcp).poll_write(cx, buf));

        Poll::Ready(Self::drop_if_hung_up(written, buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let flushed = ready!(Pin::new(&mut self.tcp).poll_flush(cx));

        Poll::Ready(Self::drop_if_hung_up(flushed, ()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let shut_down = ready!(Pin::new(&mut self.tcp).poll_shutdown(cx));

        Poll::Ready(Self::drop_if_hung_up(shut_down, ()))
    }
}

/// Returns the next binary message, skipping keepalive traffic.
async fn next_binary(socket: &mut Socket) -> TestResult<Vec<u8>> {
    loop {
        let message = tokio::time::timeout(FRAME_TIMEOUT, socket.next())
            .await?
            .ok_or("stream ended")??;
        match message {
            Message::Binary(bytes) => return Ok(bytes.to_vec()),
            Message::Ping(_) | Message::Pong(_) => {}
            other => return Err(format!("expected a binary message, got {other:?}").into()),
        }
    }
}

/// Builds a hello frame for the current protocol.
pub(crate) fn hello_frame() -> ClientFrame {
    ClientFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
    }
}

/// Encrypts and sends one JSON frame, failing when it breaks the committed
/// protocol schema.
pub(crate) async fn send(client: &mut Client, frame: &ClientFrame) -> TestResult {
    check_frame(&CLIENT_FRAME_SCHEMA, &serde_json::to_value(frame)?)?;
    client.send_text(&serde_json::to_string(frame)?).await
}

/// Waits for the next server frame.
pub(crate) async fn next_frame(client: &mut Client) -> TestResult<ServerFrame> {
    tokio::time::timeout(FRAME_TIMEOUT, client.next_untimed_frame()).await?
}

/// Waits until the stream of `socket` is closed or ends, and fails on a
/// timeout or a data message, so a stream the server keeps open cannot pass
/// for a closed one.
pub(crate) async fn stream_ends(socket: &mut Socket) -> TestResult<bool> {
    loop {
        match tokio::time::timeout(FRAME_TIMEOUT, socket.next()).await? {
            Some(Ok(Message::Close(_)) | Err(_)) | None => return Ok(true),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(other)) => return Err(format!("unexpected message {other:?}").into()),
        }
    }
}

/// Waits until the server ends the stream of `client`.
pub(crate) async fn closed(client: &mut Client) -> TestResult<bool> {
    stream_ends(&mut client.socket).await
}

/// Asserts that the server rejects with `reason` and then ends the stream.
pub(crate) async fn assert_rejected(client: &mut Client, reason: &str) -> TestResult {
    assert_eq!(
        next_frame(client).await?,
        ServerFrame::Rejected {
            reason: reason.to_string(),
        }
    );
    assert!(closed(client).await?);

    Ok(())
}

/// Validates frames against one direction of the committed protocol
/// schema, the copy clients build on.
static CLIENT_FRAME_SCHEMA: LazyLock<Result<Validator, String>> =
    LazyLock::new(|| frame_schema("ClientFrame"));
/// Validates frames against the other direction of the committed schema.
static SERVER_FRAME_SCHEMA: LazyLock<Result<Validator, String>> =
    LazyLock::new(|| frame_schema("ServerFrame"));

/// Compiles the committed schema with `definition` as its root.
fn frame_schema(definition: &str) -> Result<Validator, String> {
    let mut schema: Value = serde_json::from_str(include_str!("../../schema/remote_protocol.json"))
        .map_err(|error| format!("the committed protocol schema does not parse: {error}"))?;
    if let Some(object) = schema.as_object_mut() {
        object.remove("oneOf");
        object.insert(
            "$ref".to_string(),
            Value::from(format!("#/$defs/{definition}")),
        );
    }

    Validator::new(&schema)
        .map_err(|error| format!("the committed protocol schema does not compile: {error}"))
}

/// Fails with every error of `frame` against `schema`.
fn check_frame(schema: &Result<Validator, String>, frame: &Value) -> TestResult {
    let schema = schema.as_ref().map_err(Clone::clone)?;
    let errors: Vec<String> = schema
        .iter_errors(frame)
        .map(|error| format!("{error} at `{}`", error.instance_path()))
        .collect();
    if errors.is_empty() {
        return Ok(());
    }

    Err(format!("{frame} breaks the protocol schema: {}", errors.join("; ")).into())
}
