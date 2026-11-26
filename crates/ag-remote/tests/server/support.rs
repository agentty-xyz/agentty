//! Running-server fixture over the crate's in-memory doubles, with a
//! WebSocket client.

use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ag_relay::{AdmissionPolicy, RelayConfig, RelayHandle, RelayLimits};
use ag_remote::protocol::{ClientFrame, ServerFrame};
use ag_remote::test_support::{MemoryDeviceStore, RecordingBackend};
use ag_remote::{
    DeviceStore, PROTOCOL_VERSION, RelayStatus, RemoteHost, RemoteRelayConfig, RemoteServerConfig,
    RemoteServerHandle, StaticKeypair,
};
use ag_session::{SessionBackend, SessionService};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Client side of one remote control WebSocket.
pub(crate) type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Result type for tests and the helpers they share.
pub(crate) type TestResult<Value = ()> = Result<Value, Box<dyn Error>>;

/// Longest wait for an expected frame before a test fails.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// A started server with handles on its doubles.
pub(crate) struct TestServer {
    pub(crate) backend: Arc<RecordingBackend>,
    pub(crate) devices: Arc<MemoryDeviceStore>,
    pub(crate) handle: RemoteServerHandle,
    pub(crate) revisions_tx: watch::Sender<u64>,
}

impl TestServer {
    /// Starts a server on a free loopback port with revision `1`.
    pub(crate) async fn start() -> TestResult<Self> {
        Self::start_with(MemoryDeviceStore::default(), RecordingBackend::default()).await
    }

    /// Starts a server over `devices` and `backend`.
    pub(crate) async fn start_with(
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
    ) -> TestResult<Self> {
        Self::start_configured(devices, backend, None).await
    }

    /// Starts a server over `backend` that also registers with `relay`.
    pub(crate) async fn start_linked(
        relay: RemoteRelayConfig,
        backend: RecordingBackend,
    ) -> TestResult<Self> {
        Self::start_configured(MemoryDeviceStore::default(), backend, Some(relay)).await
    }

    /// Starts a server over `devices` and `backend`, linked to `relay` when
    /// given.
    async fn start_configured(
        devices: MemoryDeviceStore,
        backend: RecordingBackend,
        relay: Option<RemoteRelayConfig>,
    ) -> TestResult<Self> {
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
                allowed_hosts: vec!["box.tailnet.ts.net".to_string()],
                bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                relay,
            },
            host,
        )
        .await?;

        Ok(Self {
            backend,
            devices,
            handle,
            revisions_tx,
        })
    }

    /// Opens a WebSocket without sending a frame.
    pub(crate) async fn connect(&self) -> TestResult<Client> {
        let url = format!("ws://{}/ws", self.handle.local_addr());
        let (client, _) = tokio_tungstenite::connect_async(url).await?;

        Ok(client)
    }

    /// Pairs a new device, approving it through the handle, and returns its
    /// open client, device id, and token.
    pub(crate) async fn pair(&self, device_name: &str) -> TestResult<(Client, String, String)> {
        let (mut client, request_id) = self.request_pairing(device_name).await?;
        if !self.handle.approve_pairing(request_id) {
            return Err("pairing request should be pending".into());
        }
        let ServerFrame::Paired {
            device_id, token, ..
        } = next_frame(&mut client).await?
        else {
            return Err("pairing should succeed".into());
        };

        Ok((client, device_id, token))
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

    /// Opens a connection that sends a hello with `token` and returns the
    /// server's first frame.
    pub(crate) async fn hello(&self, token: &str) -> TestResult<(Client, ServerFrame)> {
        let mut client = self.connect().await?;
        send(&mut client, &hello_frame(token)).await?;
        let frame = next_frame(&mut client).await?;

        Ok((client, frame))
    }
}

/// Starts a relay on a free loopback port.
pub(crate) async fn start_relay() -> TestResult<RelayHandle> {
    let relay = ag_relay::start(relay_config()).await?;

    Ok(relay)
}

/// Starts a relay on a free loopback port that registers only the
/// instances `admission` admits.
pub(crate) async fn start_relay_with_admission(
    admission: Arc<dyn AdmissionPolicy>,
) -> TestResult<RelayHandle> {
    let relay = ag_relay::start_with_admission(relay_config(), admission).await?;

    Ok(relay)
}

/// Settings of a test relay on a free loopback port.
fn relay_config() -> RelayConfig {
    RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: None,
        limits: relay_limits(),
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

/// Starts a server linked to `relay` and waits until it is registered.
pub(crate) async fn start_linked_server(
    relay: &RelayHandle,
    instance: &StaticKeypair,
) -> TestResult<TestServer> {
    start_linked_server_over(relay, instance, RecordingBackend::default()).await
}

/// Starts a server over `backend` linked to `relay` and waits until it is
/// registered.
pub(crate) async fn start_linked_server_over(
    relay: &RelayHandle,
    instance: &StaticKeypair,
    backend: RecordingBackend,
) -> TestResult<TestServer> {
    let server = TestServer::start_linked(relay_link(relay, instance), backend).await?;
    let mut status = server.handle.status();
    tokio::time::timeout(
        FRAME_TIMEOUT,
        status.wait_for(|status| status.relay == Some(RelayStatus::Connected)),
    )
    .await??;

    Ok(server)
}

/// Starts a server linked to `relay` as `instance`, without waiting for
/// the relay to register it.
pub(crate) async fn start_server_linked_to(
    relay: &RelayHandle,
    instance: &StaticKeypair,
) -> TestResult<TestServer> {
    TestServer::start_linked(relay_link(relay, instance), RecordingBackend::default()).await
}

/// The link of a server registering with `relay` as `instance`.
fn relay_link(relay: &RelayHandle, instance: &StaticKeypair) -> RemoteRelayConfig {
    RemoteRelayConfig {
        instance_key: instance.clone(),
        url: format!("ws://{}/", relay.local_addr()),
    }
}

/// Builds a hello frame for the current protocol.
pub(crate) fn hello_frame(token: &str) -> ClientFrame {
    ClientFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: token.to_string(),
    }
}

/// Sends one JSON frame.
pub(crate) async fn send(client: &mut Client, frame: &ClientFrame) -> TestResult {
    let text = serde_json::to_string(frame)?;
    client.send(Message::Text(text.into())).await?;

    Ok(())
}

/// Waits for the next server frame, skipping control messages.
pub(crate) async fn next_frame(client: &mut Client) -> TestResult<ServerFrame> {
    loop {
        let message = tokio::time::timeout(FRAME_TIMEOUT, client.next())
            .await?
            .ok_or("socket closed before the next frame")??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(&text)?);
        }
    }
}

/// Waits for the server's close frame, or `None` when the socket ends
/// without one.
pub(crate) async fn close_frame(client: &mut Client) -> TestResult<Option<CloseFrame>> {
    loop {
        match tokio::time::timeout(FRAME_TIMEOUT, client.next()).await? {
            Some(Ok(Message::Close(frame))) => return Ok(frame),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return Ok(None),
        }
    }
}

/// Asserts that the server rejects with `reason` and then closes with a
/// policy-violation code.
pub(crate) async fn assert_rejected(client: &mut Client, reason: &str) -> TestResult {
    assert_eq!(
        next_frame(client).await?,
        ServerFrame::Rejected {
            reason: reason.to_string(),
        }
    );
    let close = close_frame(client)
        .await?
        .ok_or("server should send a close frame")?;
    assert_eq!(u16::from(close.code), 1008);
    assert_eq!(close.reason.as_str(), reason);

    Ok(())
}
