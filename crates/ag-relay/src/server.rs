//! Listener, routes, and the state every connection shares.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::limit::{HandshakeLimiter, RelayLimits};
use crate::registry::Registry;
use crate::{control, stream};

/// Path of the instance control connection.
pub const REGISTER_PATH: &str = "/v1/register";
/// Path prefix a device completes with an instance identifier.
pub const CONNECT_PATH_PREFIX: &str = "/v1/connect/";
/// Path prefix an instance completes with a stream identifier.
pub const ACCEPT_PATH_PREFIX: &str = "/v1/accept/";
/// Text message that confirms a registration.
pub const REGISTERED_MESSAGE: &str = "registered";
/// Prefix of the text message that announces a stream identifier.
pub const STREAM_MESSAGE_PREFIX: &str = "stream ";

/// Largest message on a control connection, in bytes. Handshake messages
/// are far smaller.
const CONTROL_MESSAGE_BYTES: usize = 1024;
/// Largest message on a stream, in bytes: one Noise message.
const STREAM_MESSAGE_BYTES: usize = 65_535;

/// Listener settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayConfig {
    /// Address to bind; port `0` selects a free port.
    pub bind_addr: SocketAddr,
    /// Header that holds the client address, such as `X-Real-IP`. Set it only
    /// behind a reverse proxy that overwrites the header on every request;
    /// otherwise clients choose the address their limits count against.
    /// Without it, or when the header is missing or not one address, the
    /// connection's peer address is used. Limits count an IPv6 client by its
    /// `/64` network, because one host usually holds a whole one.
    pub client_address_header: Option<String>,
    /// Resources clients may hold.
    pub limits: RelayLimits,
}

/// Failure starting the relay.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// The listener address could not be bound.
    #[error("Cannot bind the relay listener: {0}")]
    Bind(#[source] std::io::Error),
    /// [`RelayConfig::client_address_header`] is not a valid header name.
    #[error("Client address header is not a valid header name")]
    InvalidHeaderName,
}

/// Starts the relay on `config.bind_addr` and returns its control handle.
///
/// # Errors
///
/// Returns [`RelayError::InvalidHeaderName`] for an unusable client address
/// header and [`RelayError::Bind`] when the address cannot be bound.
pub async fn start(config: RelayConfig) -> Result<RelayHandle, RelayError> {
    let client_address_header = match config.client_address_header {
        Some(name) => Some(HeaderName::try_from(name).map_err(|_| RelayError::InvalidHeaderName)?),
        None => None,
    };
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .map_err(RelayError::Bind)?;
    let local_addr = listener.local_addr().map_err(RelayError::Bind)?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(RelayState {
        client_address_header,
        handshakes: Arc::new(HandshakeLimiter::new(&config.limits)),
        limits: config.limits,
        open_sockets_tx: watch::Sender::new(0),
        registry: Registry::new(),
        shutdown_rx,
    });
    let router = Router::new()
        .route("/healthz", get(health))
        .route(REGISTER_PATH, get(register))
        .route("/v1/connect/{instance_id}", get(connect))
        .route("/v1/accept/{stream_id}", get(accept))
        .with_state(Arc::clone(&state));
    let stopping = state.stopping();
    let server = tokio::spawn(async move {
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        let _ = axum::serve(listener, service)
            .with_graceful_shutdown(stopping)
            .await;
    });

    Ok(RelayHandle {
        local_addr,
        server,
        state,
        stop_signal: StopSignal(shutdown_tx),
    })
}

/// Control handle for a running relay. Dropping it stops the relay without
/// waiting; [`Self::shutdown`] also waits for every connection to close.
pub struct RelayHandle {
    local_addr: SocketAddr,
    server: JoinHandle<()>,
    state: Arc<RelayState>,
    stop_signal: StopSignal,
}

impl RelayHandle {
    /// Returns the bound listener address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops accepting connections, closes every open one, and waits until
    /// the listener port is released. Requests that have not become
    /// connections within [`RelayLimits::shutdown_timeout`] are dropped.
    pub async fn shutdown(mut self) {
        self.stop_signal.stop();
        let stopped = timeout(self.state.limits.shutdown_timeout, &mut self.server).await;
        if stopped.is_err() {
            self.server.abort();
            let _ = self.server.await;
        }
        let mut open_sockets = self.state.open_sockets_tx.subscribe();
        let _ = open_sockets.wait_for(|count| *count == 0).await;
    }
}

/// Stop signal that fires when the owning handle drops.
struct StopSignal(watch::Sender<bool>);

impl StopSignal {
    /// Tells the listener and every connection to stop.
    fn stop(&self) {
        self.0.send_replace(true);
    }
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// State shared by every connection.
pub(crate) struct RelayState {
    /// Resources clients may hold.
    pub(crate) limits: RelayLimits,
    /// Registered instances and unanswered streams.
    pub(crate) registry: Registry<WebSocket>,
    /// Header a trusted proxy sets to the client address.
    client_address_header: Option<HeaderName>,
    /// Registration handshakes in progress.
    handshakes: Arc<HandshakeLimiter>,
    /// Count of open connections, for [`RelayHandle::shutdown`].
    open_sockets_tx: watch::Sender<usize>,
    /// Becomes true once the relay stops.
    shutdown_rx: watch::Receiver<bool>,
}

impl RelayState {
    /// Resolves once the relay starts stopping or its handle is gone.
    pub(crate) fn stopping(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut shutdown_rx = self.shutdown_rx.clone();

        async move {
            let _ = shutdown_rx.wait_for(|is_stopping| *is_stopping).await;
        }
    }

    /// Counts one open connection until the guard drops.
    pub(crate) fn track_socket(self: &Arc<Self>) -> OpenSocketGuard {
        self.open_sockets_tx.send_modify(|count| *count += 1);

        OpenSocketGuard {
            state: Arc::clone(self),
        }
    }

    /// Address that limits count a client against: the configured header
    /// when it holds one address and the peer address otherwise, reduced to
    /// its `/64` network for IPv6.
    fn client_address(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let address = self
            .client_address_header
            .as_ref()
            .and_then(|name| headers.get(name))
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(peer.ip());

        limit_address(address)
    }
}

/// Reduces `address` to what limits count: an IPv4-mapped address becomes
/// its IPv4 address, and any other IPv6 address becomes its `/64` network.
fn limit_address(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        IpAddr::V4(address) => IpAddr::V4(address),
        IpAddr::V6(address) => {
            let network = u128::from(address) & !u128::from(u64::MAX);

            IpAddr::V6(network.into())
        }
    }
}

/// Guard that counts one open connection.
pub(crate) struct OpenSocketGuard {
    state: Arc<RelayState>,
}

impl Drop for OpenSocketGuard {
    fn drop(&mut self) {
        self.state.open_sockets_tx.send_modify(|count| *count -= 1);
    }
}

/// Liveness probe.
async fn health() -> &'static str {
    "ok"
}

/// Upgrades an instance's control connection, unless the relay or the
/// client's address already has as many registration handshakes in progress
/// as allowed.
async fn register(
    State(state): State<Arc<RelayState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let address = state.client_address(peer, &headers);
    let Some(handshake) = state.handshakes.reserve(address) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    upgrade
        .max_message_size(CONTROL_MESSAGE_BYTES)
        .max_frame_size(CONTROL_MESSAGE_BYTES)
        .on_upgrade(move |socket| control::serve(socket, state, address, handshake))
}

/// Upgrades a device's stream toward a registered instance.
async fn connect(
    State(state): State<Arc<RelayState>>,
    Path(instance_id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !state.registry.has_instance(&instance_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    upgrade
        .max_message_size(STREAM_MESSAGE_BYTES)
        .max_frame_size(STREAM_MESSAGE_BYTES)
        .on_upgrade(move |socket| stream::serve_device(socket, state, instance_id))
}

/// Upgrades an instance's answer to a waiting stream.
async fn accept(
    State(state): State<Arc<RelayState>>,
    Path(stream_id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !state.registry.has_pending(&stream_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    upgrade
        .max_message_size(STREAM_MESSAGE_BYTES)
        .max_frame_size(STREAM_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            stream::answer(socket, &state, &stream_id);
        })
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
