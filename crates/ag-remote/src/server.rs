//! Server lifecycle: the relay link, pairing, and the state every connection
//! shares.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_session::SessionService;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::device::{DeviceStore, DeviceStoreError};
use crate::entropy::{Entropy, OsEntropy};
use crate::pairing::{PairingCode, PairingState};
use crate::relay::{self, RelayStatus, RemoteRelayConfig};

/// Largest frame an encrypted device stream reassembles, in bytes: room for
/// a message whose images reach the most `crate::request` accepts.
pub(crate) const MAX_MESSAGE_SIZE: usize = 10 << 20;
/// Streams that may be open without an authenticated device, including
/// pairing requests that wait for approval.
pub(crate) const MAX_UNAUTHENTICATED_SOCKETS: usize = 8;

/// Host capabilities the server drives.
pub struct RemoteHost {
    /// Paired-device persistence.
    pub devices: Arc<dyn DeviceStore>,
    /// Counter the host advances whenever session state changes.
    pub revisions: watch::Receiver<u64>,
    /// Session operations, ideally tagged with remote provenance.
    pub sessions: SessionService,
}

/// Server settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteServerConfig {
    /// Relay to register with. Devices reach the server only through it,
    /// encrypt end to end, and pair with their own key.
    pub relay: RemoteRelayConfig,
}

/// Failure controlling the server.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// The device store failed.
    #[error("Remote device store failed: {0}")]
    DeviceStore(#[source] DeviceStoreError),
    /// The operating system could not supply random bytes.
    #[error("Cannot generate a secure pairing code")]
    Entropy,
}

/// Authenticated connections, pairing requests, and the relay link.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteServerStatus {
    /// Open authenticated connections in connection order.
    pub connections: Vec<RemoteConnection>,
    /// Devices that redeemed a pairing code and wait for approval, in
    /// arrival order.
    pub pairing_requests: Vec<PairingRequest>,
    /// State of the relay link.
    pub relay: RelayStatus,
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

/// Starts the server, which registers with `config.relay` and serves the
/// devices that connect through it, and returns its control handle. The
/// server opens no listener: it only dials out to the relay.
///
/// # Panics
/// Panics when called outside a Tokio runtime.
pub fn start(config: RemoteServerConfig, host: RemoteHost) -> RemoteServerHandle {
    start_with_entropy(config, host, Arc::new(OsEntropy))
}

/// Starts the server with an injected randomness source.
pub(crate) fn start_with_entropy(
    config: RemoteServerConfig,
    host: RemoteHost,
    entropy: Arc<dyn Entropy>,
) -> RemoteServerHandle {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(ServerState::new(host, entropy, shutdown_rx));
    let link = tokio::spawn(relay::run(Arc::clone(&state), config.relay));

    RemoteServerHandle {
        link,
        state,
        stop_signal: StopSignal(shutdown_tx),
    }
}

/// Control handle for a running server. Dropping it stops the server
/// without waiting; [`Self::shutdown`] also waits for every stream to close.
pub struct RemoteServerHandle {
    link: JoinHandle<()>,
    state: Arc<ServerState>,
    stop_signal: StopSignal,
}

impl RemoteServerHandle {
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

    /// Subscribes to open authenticated connections, pairing requests, and
    /// the relay link.
    pub fn status(&self) -> watch::Receiver<RemoteServerStatus> {
        self.state.status_tx.subscribe()
    }

    /// Lets a pending device finish pairing; false when it is unknown or
    /// already settled.
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

    /// Leaves the relay, closes every stream, and waits until they are
    /// closed.
    pub async fn shutdown(self) {
        self.stop_signal.stop();
        let _ = self.link.await;
        let mut open_sockets = self.state.open_sockets_tx.subscribe();
        let _ = open_sockets.wait_for(|count| *count == 0).await;
    }
}

/// Stop signal that fires when the owning handle drops.
struct StopSignal(watch::Sender<bool>);

impl StopSignal {
    /// Tells the relay link and every stream to stop.
    fn stop(&self) {
        self.0.send_replace(true);
    }
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// State shared by the relay link and every connection task.
pub(crate) struct ServerState {
    pub(crate) entropy: Arc<dyn Entropy>,
    pub(crate) shutdown_rx: watch::Receiver<bool>,
    connections: Mutex<ConnectionRegistry>,
    host: RemoteHost,
    open_sockets_tx: watch::Sender<usize>,
    pairing: Mutex<PairingState>,
    pairing_requests: Mutex<PairingRequestRegistry>,
    /// State of the relay link.
    relay_status: Mutex<RelayStatus>,
    status_tx: watch::Sender<RemoteServerStatus>,
    /// Streams holding a [`HandshakeSlot`].
    unauthenticated_sockets: Mutex<usize>,
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
            pairing_requests: Mutex::new(PairingRequestRegistry::default()),
            relay_status: Mutex::new(RelayStatus::default()),
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

    /// Resolves once the server starts stopping or its handle is gone.
    pub(crate) fn stopping(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut shutdown_rx = self.shutdown_rx.clone();

        async move {
            let _ = shutdown_rx.wait_for(|is_stopping| *is_stopping).await;
        }
    }

    /// Records the state of the relay link and publishes it.
    pub(crate) fn set_relay_status(&self, status: RelayStatus) {
        *self
            .relay_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = status;
        self.publish_status();
    }

    /// Locks the pairing state, recovering it after a panic elsewhere.
    pub(crate) fn pairing(&self) -> MutexGuard<'_, PairingState> {
        self.pairing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves one of the [`MAX_UNAUTHENTICATED_SOCKETS`] slots for a
    /// stream that has not authenticated, or returns `None` when all are
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

    /// Removes one pairing request after its stream stops waiting.
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
    ///
    /// The status is read while the channel is locked for the send, so
    /// publications run one at a time and each reads every change made
    /// before it began: the last one sent always matches the registries.
    /// Callers hold none of the registry locks.
    fn publish_status(&self) {
        self.status_tx
            .send_if_modified(|status| replace_status(status, self.current_status()));
    }

    /// Reads the registered connections, pairing requests, and relay link,
    /// taking each lock in turn and never two at once.
    fn current_status(&self) -> RemoteServerStatus {
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
        let relay = *self
            .relay_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        RemoteServerStatus {
            connections,
            pairing_requests,
            relay,
        }
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

/// Keeps one unauthenticated stream counted until it authenticates or ends.
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

/// Keeps one pairing request listed until it is settled or its stream
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

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
