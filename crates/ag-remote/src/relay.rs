//! Outbound link to a relay: registration, stream notices, and one
//! encrypted connection per device stream.
//!
//! The paths and control messages mirror the relay's `/v1` contract.

use std::sync::Arc;
use std::time::Duration;

use ag_noise::{Handshake, StaticKeypair};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::connection::{self, DeviceProof};
use crate::encrypted::{EncryptedTransport, MessageCarrier};
use crate::server::{HandshakeSlot, ServerState};

/// Path of the control connection.
const REGISTER_PATH: &str = "/v1/register";
/// Path prefix completed with a stream identifier to answer that stream.
const ACCEPT_PATH_PREFIX: &str = "/v1/accept/";
/// Text message that confirms a registration.
const REGISTERED_MESSAGE: &str = "registered";
/// Prefix of the text message that announces a stream identifier.
const STREAM_MESSAGE_PREFIX: &str = "stream ";
/// Longest stream identifier accepted from the relay, in characters.
const MAX_STREAM_ID_CHARS: usize = 64;
/// Time the relay or a device has to finish each handshake step.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait before the first reconnect attempt.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Longest wait between reconnect attempts.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// One WebSocket dialed to the relay.
type RelaySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Relay to register with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRelayConfig {
    /// Long-term key that identifies this instance to devices and proves its
    /// identifier to the relay. Persist it: a new key unpairs every device.
    pub instance_key: StaticKeypair,
    /// Relay base URL, such as `wss://relay.example`.
    pub url: String,
}

/// State of the link to the relay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayStatus {
    /// Registered; devices can reach this instance.
    Connected,
    /// Not registered: connecting, or waiting to retry.
    Connecting,
}

/// Keeps this instance registered with the relay until the server stops,
/// reconnecting with growing delays after each failure.
pub(crate) async fn run(state: Arc<ServerState>, config: RemoteRelayConfig) {
    let _open_socket = state.track_socket();
    // Idempotent: TLS connections need a process-wide provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let base_url = config.url.trim_end_matches('/').to_string();
    let mut retry_delay = INITIAL_RETRY_DELAY;
    state.set_relay_status(RelayStatus::Connecting);

    loop {
        let was_registered = tokio::select! {
            biased;
            () = state.stopping() => return,
            was_registered = link(&state, &base_url, &config.instance_key) => was_registered,
        };
        state.set_relay_status(RelayStatus::Connecting);
        if was_registered {
            retry_delay = INITIAL_RETRY_DELAY;
        }
        tokio::select! {
            biased;
            () = state.stopping() => return,
            () = sleep(retry_delay) => {}
        }
        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
    }
}

/// Registers once and answers stream notices until the control connection
/// ends. Returns whether the registration succeeded.
async fn link(state: &Arc<ServerState>, base_url: &str, instance_key: &StaticKeypair) -> bool {
    let registered = timeout(HANDSHAKE_TIMEOUT, register(base_url, instance_key)).await;
    let Some(mut control) = registered.ok().flatten() else {
        return false;
    };
    state.set_relay_status(RelayStatus::Connected);
    while let Some(stream_id) = next_stream_id(&mut control).await {
        let url = format!("{base_url}{ACCEPT_PATH_PREFIX}{stream_id}");
        tokio::spawn(serve_stream(Arc::clone(state), url, instance_key.clone()));
    }

    true
}

/// Opens the control connection and proves `instance_key` to the relay.
async fn register(base_url: &str, instance_key: &StaticKeypair) -> Option<RelaySocket> {
    let (mut control, _) = connect_async(format!("{base_url}{REGISTER_PATH}"))
        .await
        .ok()?;
    let mut handshake = Handshake::relay_initiator(instance_key).ok()?;
    MessageCarrier::send(&mut control, handshake.write_message().ok()?).await;
    let second = MessageCarrier::receive(&mut control).await?;
    handshake.read_message(&second).ok()?;
    MessageCarrier::send(&mut control, handshake.write_message().ok()?).await;
    let confirmation = next_text(&mut control).await?;

    (confirmation == REGISTERED_MESSAGE).then_some(control)
}

/// Reads the next stream notice. Returns `None` when the control connection
/// ends or sends anything else, which makes the link reconnect.
async fn next_stream_id(control: &mut RelaySocket) -> Option<String> {
    let notice = next_text(control).await?;
    let stream_id = notice.strip_prefix(STREAM_MESSAGE_PREFIX)?;
    let is_valid = stream_id.len() <= MAX_STREAM_ID_CHARS
        && stream_id.bytes().all(|byte| byte.is_ascii_alphanumeric());

    is_valid.then(|| stream_id.to_string())
}

/// Answers one stream and serves the device on it, while an unauthenticated
/// slot is free.
async fn serve_stream(state: Arc<ServerState>, url: String, instance_key: StaticKeypair) {
    let _open_socket = state.track_socket();
    let opened = tokio::select! {
        biased;
        () = state.stopping() => None,
        opened = timeout(HANDSHAKE_TIMEOUT, open_stream(&state, &url, &instance_key)) => {
            opened.ok().flatten()
        }
    };
    if let Some((transport, proof, handshake)) = opened {
        connection::serve(transport, state, handshake, proof).await;
    }
}

/// Dials the stream and runs the device handshake on it.
async fn open_stream(
    state: &Arc<ServerState>,
    url: &str,
    instance_key: &StaticKeypair,
) -> Option<(EncryptedTransport<RelaySocket>, DeviceProof, HandshakeSlot)> {
    let handshake = state.reserve_handshake()?;
    let (stream, _) = connect_async(url).await.ok()?;
    let (transport, device) = EncryptedTransport::accept(stream, instance_key).await?;

    Some((transport, DeviceProof::Key(device), handshake))
}

/// Returns the next message that is not keepalive traffic.
async fn next_data(socket: &mut RelaySocket) -> Option<Message> {
    loop {
        let message = socket.next().await?.ok()?;
        if !matches!(message, Message::Ping(_) | Message::Pong(_)) {
            return Some(message);
        }
    }
}

/// Returns the next data message when it is text.
async fn next_text(socket: &mut RelaySocket) -> Option<String> {
    match next_data(socket).await? {
        Message::Text(text) => Some(text.to_string()),
        _ => None,
    }
}

/// Wraps handshake or stream bytes as a binary message.
fn binary(bytes: Vec<u8>) -> Message {
    Message::Binary(bytes.into())
}

impl MessageCarrier for RelaySocket {
    async fn receive(&mut self) -> Option<Vec<u8>> {
        match next_data(self).await? {
            Message::Binary(bytes) => Some(bytes.to_vec()),
            _ => None,
        }
    }

    async fn send(&mut self, message: Vec<u8>) {
        let _ = SinkExt::send(self, binary(message)).await;
    }

    async fn close(&mut self) {
        let _ = SinkExt::close(self).await;
    }
}
