//! HTTP listener, embedded web client, and server lifecycle.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_session::SessionService;
use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::connection;
use crate::device::{DeviceStore, DeviceStoreError};
use crate::entropy::{Entropy, OsEntropy};
use crate::pairing::{PairingCode, PairingState};

/// Largest accepted WebSocket message or frame, in bytes.
const MAX_MESSAGE_SIZE: usize = 1 << 20;
/// Sockets that may be open without an authenticated device, including
/// pairing requests that wait for approval.
pub(crate) const MAX_UNAUTHENTICATED_SOCKETS: usize = 8;
/// Host names every listener answers, regardless of the configured
/// allowlist.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];
/// Content security policy for every embedded asset.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
                                       connect-src 'self'; img-src 'self'; base-uri 'none'; \
                                       form-action 'none'; frame-ancestors 'none'";

/// Host capabilities the server drives.
pub struct RemoteHost {
    /// Paired-device persistence.
    pub devices: Arc<dyn DeviceStore>,
    /// Counter the host advances whenever session state changes.
    pub revisions: watch::Receiver<u64>,
    /// Session operations, ideally tagged with remote provenance.
    pub sessions: SessionService,
}

/// Listener settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteServerConfig {
    /// Host names, without ports, that requests may name in `Host` besides
    /// `localhost`, `127.0.0.1`, and `::1`, such as a tunnel's host name.
    /// Matching ignores ASCII case. Any other `Host` is refused, which keeps
    /// DNS-rebinding pages away from the listener.
    pub allowed_hosts: Vec<String>,
    /// Address to bind; port `0` selects a free port.
    pub bind_addr: SocketAddr,
}

/// Failure starting or controlling the server.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// The listener could not bind or report its address.
    #[error("Cannot listen for remote control: {0}")]
    Bind(#[source] std::io::Error),
    /// The device store failed.
    #[error("Remote device store failed: {0}")]
    DeviceStore(#[source] DeviceStoreError),
    /// The operating system could not supply random bytes.
    #[error("Cannot generate a secure pairing code")]
    Entropy,
}

/// Authenticated connections and pairing requests currently open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteServerStatus {
    /// Open authenticated connections in connection order.
    pub connections: Vec<RemoteConnection>,
    /// Devices that redeemed a pairing code and wait for approval, in
    /// arrival order.
    pub pairing_requests: Vec<PairingRequest>,
}

/// One device waiting for the host to approve its pairing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingRequest {
    /// Short code the device also shows, so the user can match them.
    pub confirmation: String,
    /// Name the device chose while pairing.
    pub device_name: String,
    /// Identifier passed to [`RemoteServerHandle::approve_pairing`] or
    /// [`RemoteServerHandle::deny_pairing`].
    pub id: u64,
}

/// One open authenticated connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteConnection {
    /// Authenticated device identifier.
    pub device_id: String,
    /// Authenticated device name.
    pub device_name: String,
}

/// Starts the server on `config.bind_addr` and returns its control handle.
///
/// # Errors
/// Returns [`RemoteError::Bind`] when the address cannot be bound.
pub async fn start(
    config: RemoteServerConfig,
    host: RemoteHost,
) -> Result<RemoteServerHandle, RemoteError> {
    start_with_entropy(config, host, Arc::new(OsEntropy)).await
}

/// Starts the server with an injected randomness source.
pub(crate) async fn start_with_entropy(
    config: RemoteServerConfig,
    host: RemoteHost,
    entropy: Arc<dyn Entropy>,
) -> Result<RemoteServerHandle, RemoteError> {
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .map_err(RemoteError::Bind)?;
    let local_addr = listener.local_addr().map_err(RemoteError::Bind)?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(ServerState::new(
        host,
        entropy,
        shutdown_rx.clone(),
        config.allowed_hosts,
    ));
    let router = Router::new()
        .route("/", get(index))
        .route("/app.css", get(stylesheet))
        .route("/app.js", get(script))
        .route("/ws", get(upgrade))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            require_allowed_host,
        ))
        .with_state(Arc::clone(&state));
    let server = tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        let shutdown = async move {
            let _ = shutdown_rx.wait_for(|is_stopping| *is_stopping).await;
        };
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await;
    });

    Ok(RemoteServerHandle {
        local_addr,
        server,
        state,
        stop_signal: StopSignal(shutdown_tx),
    })
}

/// Control handle for a running server. Dropping it stops the server
/// without waiting; [`Self::shutdown`] also waits for every socket to close.
pub struct RemoteServerHandle {
    local_addr: SocketAddr,
    server: JoinHandle<()>,
    state: Arc<ServerState>,
    stop_signal: StopSignal,
}

impl RemoteServerHandle {
    /// Returns the bound listener address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Issues a fresh pairing code, replacing any active one.
    ///
    /// # Errors
    /// Returns [`RemoteError::Entropy`] when secure randomness is unavailable.
    pub fn issue_pairing_code(&self) -> Result<PairingCode, RemoteError> {
        self.state
            .pairing()
            .issue(self.state.entropy.as_ref())
            .ok_or(RemoteError::Entropy)
    }

    /// Subscribes to open authenticated connections and pairing requests.
    pub fn status(&self) -> watch::Receiver<RemoteServerStatus> {
        self.state.status_tx.subscribe()
    }

    /// Issues the device token for a pending request; false when it is
    /// unknown or already settled.
    pub fn approve_pairing(&self, request_id: u64) -> bool {
        self.state.settle_pairing(request_id, true)
    }

    /// Rejects a pending request; false when it is unknown or already
    /// settled.
    pub fn deny_pairing(&self, request_id: u64) -> bool {
        self.state.settle_pairing(request_id, false)
    }

    /// Deletes one paired device and closes its open connections.
    ///
    /// # Errors
    /// Returns [`RemoteError::DeviceStore`] when the device cannot be deleted;
    /// its connections then stay open.
    pub async fn revoke_device(&self, device_id: &str) -> Result<(), RemoteError> {
        self.state
            .devices()
            .delete_device(device_id)
            .await
            .map_err(RemoteError::DeviceStore)?;
        self.state.close_device_connections(device_id);

        Ok(())
    }

    /// Stops accepting connections, closes every socket, and waits until the
    /// listener port is released.
    pub async fn shutdown(self) {
        self.stop_signal.stop();
        let _ = self.server.await;
        let mut open_sockets = self.state.open_sockets_tx.subscribe();
        let _ = open_sockets.wait_for(|count| *count == 0).await;
    }
}

/// Stop signal that fires when the owning handle drops.
struct StopSignal(watch::Sender<bool>);

impl StopSignal {
    /// Tells the listener and every socket to stop.
    fn stop(&self) {
        self.0.send_replace(true);
    }
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// State shared by the router and every connection task.
pub(crate) struct ServerState {
    pub(crate) entropy: Arc<dyn Entropy>,
    pub(crate) shutdown_rx: watch::Receiver<bool>,
    /// Host names answered besides the loopback names.
    allowed_hosts: Vec<String>,
    connections: Mutex<ConnectionRegistry>,
    host: RemoteHost,
    open_sockets_tx: watch::Sender<usize>,
    pairing: Mutex<PairingState>,
    pairing_requests: Mutex<PairingRequestRegistry>,
    status_tx: watch::Sender<RemoteServerStatus>,
    /// Sockets holding a [`HandshakeSlot`].
    unauthenticated_sockets: Mutex<usize>,
}

impl ServerState {
    /// Creates idle state for `host` that answers `allowed_hosts` besides
    /// the loopback names.
    pub(crate) fn new(
        host: RemoteHost,
        entropy: Arc<dyn Entropy>,
        shutdown_rx: watch::Receiver<bool>,
        allowed_hosts: Vec<String>,
    ) -> Self {
        Self {
            allowed_hosts,
            connections: Mutex::new(ConnectionRegistry::default()),
            entropy,
            host,
            open_sockets_tx: watch::Sender::new(0),
            pairing: Mutex::new(PairingState::default()),
            pairing_requests: Mutex::new(PairingRequestRegistry::default()),
            shutdown_rx,
            status_tx: watch::Sender::new(RemoteServerStatus::default()),
            unauthenticated_sockets: Mutex::new(0),
        }
    }

    /// Returns the device store.
    pub(crate) fn devices(&self) -> &dyn DeviceStore {
        self.host.devices.as_ref()
    }

    /// Returns the session operations.
    pub(crate) fn sessions(&self) -> &SessionService {
        &self.host.sessions
    }

    /// Returns a fresh subscription to host revisions.
    pub(crate) fn revisions(&self) -> watch::Receiver<u64> {
        self.host.revisions.clone()
    }

    /// Locks the pairing state, recovering it after a panic elsewhere.
    pub(crate) fn pairing(&self) -> MutexGuard<'_, PairingState> {
        self.pairing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves one of the [`MAX_UNAUTHENTICATED_SOCKETS`] slots for a
    /// socket that has not authenticated, or returns `None` when all are
    /// taken.
    pub(crate) fn reserve_handshake(self: &Arc<Self>) -> Option<HandshakeSlot> {
        let mut unauthenticated = self
            .unauthenticated_sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *unauthenticated >= MAX_UNAUTHENTICATED_SOCKETS {
            return None;
        }
        *unauthenticated += 1;

        Some(HandshakeSlot {
            state: Arc::clone(self),
        })
    }

    /// Lists a pairing request until the host settles it or the returned
    /// guard drops, and returns the receiver of the host's decision.
    pub(crate) fn request_pairing(
        self: &Arc<Self>,
        device_name: String,
        confirmation: String,
    ) -> (oneshot::Receiver<bool>, PairingRequestGuard) {
        let (decision_tx, decision_rx) = oneshot::channel();
        let id = {
            let mut registry = self.pairing_registry();
            let id = registry.next_id;
            registry.next_id += 1;
            registry.entries.insert(
                id,
                PendingPairing {
                    decision_tx,
                    request: PairingRequest {
                        confirmation,
                        device_name,
                        id,
                    },
                },
            );

            id
        };
        self.publish_status();

        (
            decision_rx,
            PairingRequestGuard {
                id,
                state: Arc::clone(self),
            },
        )
    }

    /// Delivers the host's decision on one pairing request and reports
    /// whether a waiting device received it.
    fn settle_pairing(&self, request_id: u64, is_approved: bool) -> bool {
        let pending = self.pairing_registry().entries.remove(&request_id);
        self.publish_status();

        pending.is_some_and(|pending| pending.decision_tx.send(is_approved).is_ok())
    }

    /// Removes one pairing request after its socket stops waiting.
    fn withdraw_pairing(&self, request_id: u64) {
        self.pairing_registry().entries.remove(&request_id);
        self.publish_status();
    }

    /// Locks the pairing request registry, recovering it after a panic
    /// elsewhere.
    fn pairing_registry(&self) -> MutexGuard<'_, PairingRequestRegistry> {
        self.pairing_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts one socket task until the returned guard drops.
    pub(crate) fn track_socket(self: &Arc<Self>) -> OpenSocketGuard {
        self.open_sockets_tx.send_modify(|count| *count += 1);

        OpenSocketGuard {
            state: Arc::clone(self),
        }
    }

    /// Registers one authenticated connection and returns its close signal
    /// with a guard that unregisters it.
    pub(crate) fn register(
        self: &Arc<Self>,
        connection: RemoteConnection,
    ) -> (oneshot::Receiver<()>, RegistrationGuard) {
        let (close_tx, close_rx) = oneshot::channel();
        let id = {
            let mut registry = self.registry();
            let id = registry.next_id;
            registry.next_id += 1;
            registry.entries.insert(
                id,
                RegisteredConnection {
                    close_tx,
                    connection,
                },
            );

            id
        };
        self.publish_status();

        (
            close_rx,
            RegistrationGuard {
                id,
                state: Arc::clone(self),
            },
        )
    }

    /// Signals every open connection of `device_id` to close.
    fn close_device_connections(&self, device_id: &str) {
        let closed = {
            let mut registry = self.registry();
            let ids: Vec<u64> = registry
                .entries
                .iter()
                .filter(|(_, entry)| entry.connection.device_id == device_id)
                .map(|(id, _)| *id)
                .collect();

            ids.into_iter()
                .filter_map(|id| registry.entries.remove(&id))
                .collect::<Vec<_>>()
        };
        for entry in closed {
            let _ = entry.close_tx.send(());
        }
        self.publish_status();
    }

    /// Removes one connection after its task ends.
    fn unregister(&self, id: u64) {
        self.registry().entries.remove(&id);
        self.publish_status();
    }

    /// Publishes the registered connections and pairing requests in arrival
    /// order.
    fn publish_status(&self) {
        let connections = self
            .registry()
            .entries
            .values()
            .map(|entry| entry.connection.clone())
            .collect();
        let pairing_requests = self
            .pairing_registry()
            .entries
            .values()
            .map(|pending| pending.request.clone())
            .collect();
        let next = RemoteServerStatus {
            connections,
            pairing_requests,
        };
        self.status_tx
            .send_if_modified(|status| replace_status(status, next));
    }

    /// Locks the connection registry, recovering it after a panic elsewhere.
    fn registry(&self) -> MutexGuard<'_, ConnectionRegistry> {
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Keeps one socket task counted until it ends.
pub(crate) struct OpenSocketGuard {
    state: Arc<ServerState>,
}

impl Drop for OpenSocketGuard {
    fn drop(&mut self) {
        self.state
            .open_sockets_tx
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// Keeps one authenticated connection in the registry until it ends.
pub(crate) struct RegistrationGuard {
    id: u64,
    state: Arc<ServerState>,
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        self.state.unregister(self.id);
    }
}

/// Open authenticated connections keyed by connection order.
#[derive(Default)]
struct ConnectionRegistry {
    entries: BTreeMap<u64, RegisteredConnection>,
    next_id: u64,
}

/// One registry entry with its close signal.
struct RegisteredConnection {
    close_tx: oneshot::Sender<()>,
    connection: RemoteConnection,
}

/// Keeps one unauthenticated socket counted until it authenticates or ends.
pub(crate) struct HandshakeSlot {
    state: Arc<ServerState>,
}

impl Drop for HandshakeSlot {
    fn drop(&mut self) {
        let mut unauthenticated = self
            .state
            .unauthenticated_sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *unauthenticated = unauthenticated.saturating_sub(1);
    }
}

/// Keeps one pairing request listed until it is settled or its socket
/// stops waiting.
pub(crate) struct PairingRequestGuard {
    id: u64,
    state: Arc<ServerState>,
}

impl Drop for PairingRequestGuard {
    fn drop(&mut self) {
        self.state.withdraw_pairing(self.id);
    }
}

/// Pairing requests waiting for the host, keyed by arrival order.
#[derive(Default)]
struct PairingRequestRegistry {
    entries: BTreeMap<u64, PendingPairing>,
    next_id: u64,
}

/// One pairing request with the channel that delivers the host's decision.
struct PendingPairing {
    decision_tx: oneshot::Sender<bool>,
    request: PairingRequest,
}

/// Replaces `status` and reports whether it changed.
fn replace_status(status: &mut RemoteServerStatus, next: RemoteServerStatus) -> bool {
    if *status == next {
        return false;
    }
    *status = next;

    true
}

/// Serves the web client page.
async fn index() -> Response {
    asset(
        "text/html; charset=utf-8",
        include_str!("../assets/index.html"),
    )
}

/// Serves the web client stylesheet.
async fn stylesheet() -> Response {
    asset("text/css; charset=utf-8", include_str!("../assets/app.css"))
}

/// Serves the web client script.
async fn script() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../assets/app.js"),
    )
}

/// Builds one embedded asset response with restrictive headers.
fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// Accepts a WebSocket upgrade from a same-origin page or a non-browser
/// client while an unauthenticated slot is free.
async fn upgrade(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_matches_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(handshake) = state.reserve_handshake() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    upgrade
        .max_message_size(MAX_MESSAGE_SIZE)
        .max_frame_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| connection::serve(socket, state, handshake))
}

/// Refuses every request whose `Host` is neither loopback nor allowed.
async fn require_allowed_host(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    if !host_is_allowed(request.headers(), &state.allowed_hosts) {
        return StatusCode::FORBIDDEN.into_response();
    }

    next.run(request).await
}

/// Returns whether the `Host` header names a loopback host or one of
/// `allowed_hosts`, ignoring its port and ASCII case.
pub(crate) fn host_is_allowed(headers: &HeaderMap, allowed_hosts: &[String]) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
    else {
        return false;
    };
    let name = host_name(host);

    LOOPBACK_HOSTS
        .into_iter()
        .chain(allowed_hosts.iter().map(String::as_str))
        .any(|allowed| allowed.eq_ignore_ascii_case(name))
}

/// Strips the port from a `Host` value, unwrapping bracketed IPv6
/// addresses such as `[::1]:4646`.
fn host_name(host: &str) -> &str {
    if let Some(bracketed) = host.strip_prefix('[') {
        return bracketed
            .split_once(']')
            .map_or(bracketed, |(name, _)| name);
    }

    host.split_once(':').map_or(host, |(name, _)| name)
}

/// Returns whether the request has no `Origin` or one naming the `Host`.
///
/// Browsers always send `Origin` on WebSocket upgrades, so this rejects pages
/// on other sites that try to reach the local server.
pub(crate) fn origin_matches_host(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let origin_authority = origin.to_str().ok().and_then(|origin| {
        origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
    });
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());

    matches!(
        (origin_authority, host),
        (Some(origin_authority), Some(host)) if origin_authority.eq_ignore_ascii_case(host)
    )
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
