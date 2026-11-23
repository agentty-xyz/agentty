//! Outbound link to a relay: registration, stream notices, and one
//! encrypted connection per device stream.
//!
//! The paths and control messages mirror the relay's `/v1` contract. The
//! relay is untrusted, so every read and write on its sockets is bounded in
//! size and time.

use std::sync::Arc;
use std::time::Duration;

use ag_noise::{Handshake, PublicKey, StaticKeypair};
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
};

use crate::connection::{self, DeviceProof};
use crate::encrypted::{EncryptedTransport, MessageCarrier};
use crate::server::{ConnectionOrigin, HandshakeSlot, ServerState};

/// Path of the control connection.
const REGISTER_PATH: &str = "/v1/register";
/// Path prefix completed with a stream identifier to answer that stream.
const ACCEPT_PATH_PREFIX: &str = "/v1/accept/";
/// Text message that confirms a registration.
const REGISTERED_MESSAGE: &str = "registered";
/// Close reason the relay sends, with a policy-violation code, instead of
/// confirming a registration its admission policy refused.
const NOT_ADMITTED_REASON: &str = "Instance is not admitted";
/// Prefix of the text message that announces a stream identifier.
const STREAM_MESSAGE_PREFIX: &str = "stream ";
/// Longest stream identifier accepted from the relay, in characters.
const MAX_STREAM_ID_CHARS: usize = 64;
/// Largest WebSocket message accepted from the relay: one Noise message.
const MAX_RELAY_MESSAGE_BYTES: usize = 65_535;
/// Time the relay or a device has to finish each handshake step.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest silence on a relay socket before it counts as dead. The relay
/// pings every connection well within this.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Time the relay has to take one message.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait before the first reconnect attempt.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Longest wait between reconnect attempts.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

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
    /// The relay refused the latest registration because its admission
    /// policy does not admit this instance. The link keeps retrying, since
    /// the relay may admit it later.
    NotAdmitted,
}

/// Builds the link a device opens to pair through the relay at `relay_url`.
///
/// `relay_url` is a `ws://` or `wss://` URL. The relay serves the web client
/// at the same place over `http://` or `https://`, and the client reads the
/// relay address, `instance_key`, and `pairing_code` from the fragment.
/// Browsers keep the fragment to themselves, so the relay never sees the
/// instance key or the one-time code.
pub fn pairing_link(relay_url: &str, instance_key: &PublicKey, pairing_code: &str) -> String {
    let relay_url = relay_url.trim_end_matches('/');
    // `ws://` becomes `http://` and `wss://` becomes `https://`.
    let page = format!("http{}/", relay_url.get(2..).unwrap_or_default());

    pairing_link_on_page(&page, relay_url, instance_key, pairing_code)
}

/// Builds the link a device opens to pair through the relay at `relay_url`
/// from `page`, which serves the web client in place of the relay's own
/// page, such as a hosted service's pairing page.
///
/// The link is `page` with the fragment [`pairing_link`] uses, so the web
/// client reads it alike and `page` never sees the fragment either.
pub fn pairing_link_on_page(
    page: &str,
    relay_url: &str,
    instance_key: &PublicKey,
    pairing_code: &str,
) -> String {
    let relay_url = relay_url.trim_end_matches('/');

    format!(
        "{page}#relay={}&key={}&code={}",
        utf8_percent_encode(relay_url, NON_ALPHANUMERIC),
        instance_key.to_hex(),
        utf8_percent_encode(pairing_code, NON_ALPHANUMERIC),
    )
}

/// Keeps this instance registered with the relay until the server stops,
/// reconnecting with growing delays after each failure.
pub(crate) async fn run(state: Arc<ServerState>, config: RemoteRelayConfig) {
    let _open_socket = state.track_socket();
    state.set_relay_status(RelayStatus::Connecting);
    let dialer = Dialer {
        base_url: config.url.trim_end_matches('/').to_string(),
        tls: tokio::task::spawn_blocking(tls_config).await.ok(),
    };
    let mut retry_delay = INITIAL_RETRY_DELAY;

    loop {
        let outcome = tokio::select! {
            biased;
            () = state.stopping() => return,
            outcome = link(&state, &dialer, &config.instance_key) => outcome,
        };
        // A refusal stays reported until a later attempt ends otherwise.
        state.set_relay_status(if outcome == LinkOutcome::NotAdmitted {
            RelayStatus::NotAdmitted
        } else {
            RelayStatus::Connecting
        });
        let delay = retry_delay_after(retry_delay, outcome == LinkOutcome::Registered);
        tokio::select! {
            biased;
            () = state.stopping() => return,
            () = sleep(delay) => {}
        }
        retry_delay = (delay * 2).min(MAX_RETRY_DELAY);
    }
}

/// Wait before reconnecting: the shortest delay after a link that had
/// registered, so a brief relay outage recovers quickly, and the growing
/// `previous` delay after a failed attempt.
fn retry_delay_after(previous: Duration, was_registered: bool) -> Duration {
    if was_registered {
        return INITIAL_RETRY_DELAY;
    }

    previous
}

/// Builds the TLS settings for `wss://` relays once: the system trust store
/// is slow to read. Certificates that do not parse are skipped.
fn tls_config() -> Arc<ClientConfig> {
    // Idempotent: TLS connections need a process-wide provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);

    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Opens `WebSocket` connections to one relay with shared TLS settings and
/// size limits.
#[derive(Clone)]
struct Dialer {
    base_url: String,
    /// Shared TLS settings; `None` lets each dial load its own.
    tls: Option<Arc<ClientConfig>>,
}

impl Dialer {
    /// Connects to `path` on the relay.
    async fn open(&self, path: &str) -> Option<RelayStream> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_RELAY_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_RELAY_MESSAGE_BYTES));
        let (socket, _) = connect_async_tls_with_config(
            format!("{}{path}", self.base_url),
            Some(config),
            true,
            self.tls.clone().map(Connector::Rustls),
        )
        .await
        .ok()?;

        Some(RelayStream {
            is_broken: false,
            is_not_admitted: false,
            socket,
        })
    }
}

/// How one registration attempt ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkOutcome {
    /// The registration failed for another reason, such as an unreachable
    /// relay or a timeout.
    Failed,
    /// The relay refused the registration as not admitted.
    NotAdmitted,
    /// The relay confirmed the registration, and the control connection has
    /// since ended.
    Registered,
}

/// Registers once and answers stream notices until the control connection
/// ends.
async fn link(
    state: &Arc<ServerState>,
    dialer: &Dialer,
    instance_key: &StaticKeypair,
) -> LinkOutcome {
    let registered = timeout(HANDSHAKE_TIMEOUT, register(dialer, instance_key))
        .await
        .unwrap_or(Err(LinkOutcome::Failed));
    let mut control = match registered {
        Ok(control) => control,
        Err(outcome) => return outcome,
    };
    state.set_relay_status(RelayStatus::Connected);
    while let Some(stream_id) = next_stream_id(&mut control).await {
        tokio::spawn(serve_stream(
            Arc::clone(state),
            dialer.clone(),
            format!("{ACCEPT_PATH_PREFIX}{stream_id}"),
            instance_key.clone(),
        ));
    }

    LinkOutcome::Registered
}

/// Opens the control connection and proves `instance_key` to the relay.
/// Returns the confirmed control connection, or how the attempt ended
/// without one.
async fn register(
    dialer: &Dialer,
    instance_key: &StaticKeypair,
) -> Result<RelayStream, LinkOutcome> {
    let mut control = dialer
        .open(REGISTER_PATH)
        .await
        .ok_or(LinkOutcome::Failed)?;
    let confirmation = confirm_registration(&mut control, instance_key).await;
    if confirmation.as_deref() == Some(REGISTERED_MESSAGE) {
        return Ok(control);
    }

    Err(if control.is_not_admitted {
        LinkOutcome::NotAdmitted
    } else {
        LinkOutcome::Failed
    })
}

/// Runs the relay handshake on `control` and returns the relay's answer,
/// or `None` when the handshake fails or the relay closes the connection.
async fn confirm_registration(
    control: &mut RelayStream,
    instance_key: &StaticKeypair,
) -> Option<String> {
    let mut handshake = Handshake::relay_initiator(instance_key).ok()?;
    control.send(handshake.write_message().ok()?).await;
    let second = control.receive().await?;
    handshake.read_message(&second).ok()?;
    control.send(handshake.write_message().ok()?).await;

    control.next_text().await
}

/// Reads the next stream notice. Returns `None` when the control connection
/// ends, falls silent, or sends anything else, which makes the link
/// reconnect.
async fn next_stream_id(control: &mut RelayStream) -> Option<String> {
    let notice = control.next_text().await?;
    let stream_id = notice.strip_prefix(STREAM_MESSAGE_PREFIX)?;
    let is_valid = stream_id.len() <= MAX_STREAM_ID_CHARS
        && stream_id.bytes().all(|byte| byte.is_ascii_alphanumeric());

    is_valid.then(|| stream_id.to_string())
}

/// Answers one stream and serves the device on it, while an unauthenticated
/// relay slot is free.
async fn serve_stream(
    state: Arc<ServerState>,
    dialer: Dialer,
    path: String,
    instance_key: StaticKeypair,
) {
    let _open_socket = state.track_socket();
    let opened = tokio::select! {
        biased;
        () = state.stopping() => None,
        opened = timeout(HANDSHAKE_TIMEOUT, open_stream(&state, &dialer, &path, &instance_key)) => {
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
    dialer: &Dialer,
    path: &str,
    instance_key: &StaticKeypair,
) -> Option<(EncryptedTransport<RelayStream>, DeviceProof, HandshakeSlot)> {
    let handshake = state.reserve_handshake(ConnectionOrigin::Relay)?;
    let stream = dialer.open(path).await?;
    let (transport, device) = EncryptedTransport::accept(stream, instance_key).await?;

    Some((transport, DeviceProof::Key(device), handshake))
}

/// One WebSocket dialed to the relay. A send that times out marks it
/// broken, so the next receive reports the end.
struct RelayStream {
    is_broken: bool,
    /// Whether the relay closed the socket because it does not admit this
    /// instance.
    is_not_admitted: bool,
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl RelayStream {
    /// Returns the next message that is not keepalive traffic, or `None`
    /// once the socket ends, breaks, or stays silent past [`IDLE_TIMEOUT`].
    async fn next_data(&mut self) -> Option<Message> {
        while !self.is_broken {
            match timeout(IDLE_TIMEOUT, self.socket.next()).await {
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
                Ok(Some(Ok(message))) if !message.is_close() => return Some(message),
                ended => {
                    self.is_not_admitted = matches!(
                        ended,
                        Ok(Some(Ok(Message::Close(Some(frame)))))
                            if frame.code == CloseCode::Policy
                                && frame.reason == NOT_ADMITTED_REASON
                    );
                    self.is_broken = true;
                }
            }
        }

        None
    }

    /// Returns the next data message as text, or `None` when it is not
    /// UTF-8. Callers match the exact control messages they expect.
    async fn next_text(&mut self) -> Option<String> {
        let text = self.next_data().await?.into_text().ok()?;

        Some(text.to_string())
    }
}

impl MessageCarrier for RelayStream {
    /// Returns the payload of the next data message. A text message from the
    /// relay is passed on as its bytes and then fails decryption.
    async fn receive(&mut self) -> Option<Vec<u8>> {
        Some(self.next_data().await?.into_data().to_vec())
    }

    async fn send(&mut self, message: Vec<u8>) {
        let sent = timeout(
            WRITE_TIMEOUT,
            self.socket.send(Message::Binary(message.into())),
        )
        .await;
        self.is_broken |= !matches!(sent, Ok(Ok(())));
    }

    async fn close(&mut self) {
        let _ = timeout(WRITE_TIMEOUT, self.socket.close(None)).await;
        self.is_broken = true;
    }
}

#[cfg(test)]
#[path = "relay_test.rs"]
mod tests;
