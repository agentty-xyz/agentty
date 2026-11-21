//! HTTP listener, embedded web client, and server lifecycle.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_session::SessionService;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{HeaderMap, StatusCode, header};
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteServerConfig {
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

/// Authenticated connections currently open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteServerStatus {
    /// Open authenticated connections in connection order.
    pub connections: Vec<RemoteConnection>,
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
    let state = Arc::new(ServerState::new(host, entropy, shutdown_rx.clone()));
    let router = Router::new()
        .route("/", get(index))
        .route("/app.css", get(stylesheet))
        .route("/app.js", get(script))
        .route("/ws", get(upgrade))
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

    /// Subscribes to the set of open authenticated connections.
    pub fn status(&self) -> watch::Receiver<RemoteServerStatus> {
        self.state.status_tx.subscribe()
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
    connections: Mutex<ConnectionRegistry>,
    host: RemoteHost,
    open_sockets_tx: watch::Sender<usize>,
    pairing: Mutex<PairingState>,
    status_tx: watch::Sender<RemoteServerStatus>,
}

impl ServerState {
    /// Creates idle state for `host`.
    pub(crate) fn new(
        host: RemoteHost,
        entropy: Arc<dyn Entropy>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            connections: Mutex::new(ConnectionRegistry::default()),
            entropy,
            host,
            open_sockets_tx: watch::Sender::new(0),
            pairing: Mutex::new(PairingState::default()),
            shutdown_rx,
            status_tx: watch::Sender::new(RemoteServerStatus::default()),
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

    /// Publishes the registered connections in connection order.
    fn publish_status(&self) {
        let connections = self
            .registry()
            .entries
            .values()
            .map(|entry| entry.connection.clone())
            .collect();
        self.status_tx
            .send_if_modified(|status| replace_connections(status, connections));
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

/// Replaces `status` connections and reports whether they changed.
fn replace_connections(
    status: &mut RemoteServerStatus,
    connections: Vec<RemoteConnection>,
) -> bool {
    if status.connections == connections {
        return false;
    }
    status.connections = connections;

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
/// client.
async fn upgrade(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_matches_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }

    upgrade
        .max_message_size(MAX_MESSAGE_SIZE)
        .max_frame_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| connection::serve(socket, state))
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
