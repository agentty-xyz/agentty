//! Remote-control lifecycle: starts the loopback listener, tracks paired
//! devices, and tells connected clients when session state changes.
//!
//! Remote clients reach sessions only through [`SessionService`] over the
//! session-runtime mailbox, so their commands share terminal admission and
//! model work stays behind the worker boundary.

use std::sync::Arc;

use ag_remote::{DeviceStore, RemoteConnection, RemoteHost, RemoteServerStatus};
use ag_session::SessionService;
use tokio::sync::{mpsc, watch};

use crate::app::command_gateway::CommandInitiator;
use crate::app::spawn::spawn_detached;
use crate::app::{App, AppEvent};
use crate::infra::db::AppRepositories;
use crate::infra::remote_control::{
    RemoteControlLauncher, RemoteControlServer, RepositoryDeviceStore,
};
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView,
};

/// Loopback port used unless `--remote-control-port` selects another.
pub const DEFAULT_REMOTE_CONTROL_PORT: u16 = 4646;
/// Reported when remote control is started without its launch flag.
const REMOTE_CONTROL_DISABLED: &str = "Remote control needs `agentty --experimental-remote`.";

/// Remote-control listener ownership and its projected view.
pub(crate) struct RemoteControl {
    /// Latest authenticated connections reported by the running listener.
    connections: Vec<RemoteConnection>,
    devices: Arc<dyn DeviceStore>,
    /// Identifies the running listener so late status from a stopped one
    /// is ignored.
    generation: u64,
    launcher: Arc<dyn RemoteControlLauncher>,
    /// Loopback port, or `None` until `--experimental-remote` enables
    /// remote control.
    port: Option<u16>,
    revision_tx: watch::Sender<u64>,
    server: Option<Box<dyn RemoteControlServer>>,
    view: RemoteControlView,
}

impl RemoteControl {
    /// Creates disabled remote control that starts listeners through
    /// `launcher` and persists paired devices in `repositories`.
    pub(crate) fn new(
        launcher: Arc<dyn RemoteControlLauncher>,
        repositories: &AppRepositories,
    ) -> Self {
        let (revision_tx, _revision_rx) = watch::channel(0);

        Self {
            connections: Vec::new(),
            devices: Arc::new(RepositoryDeviceStore::new(repositories.remote_devices())),
            generation: 0,
            launcher,
            port: None,
            revision_tx,
            server: None,
            view: RemoteControlView::default(),
        }
    }

    /// Returns whether `--experimental-remote` enabled remote control.
    pub(crate) fn is_enabled(&self) -> bool {
        self.port.is_some()
    }

    /// Returns the state projected into the overlay and status bar.
    pub(crate) fn view(&self) -> &RemoteControlView {
        &self.view
    }

    /// Subscribes to the session-change revision that remote clients follow.
    pub(crate) fn subscribe_revisions(&self) -> watch::Receiver<u64> {
        self.revision_tx.subscribe()
    }
}

impl App {
    /// Enables the experimental `/remote-control` command, listening on the
    /// loopback `port` when it starts.
    pub fn enable_remote_control(&mut self, port: u16) {
        self.remote_control.port = Some(port);
    }

    /// Tells connected remote clients that session state changed, so they
    /// fetch it again. Bursts coalesce into one notification.
    pub(crate) fn publish_session_change(&self) {
        self.remote_control
            .revision_tx
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// Starts the listener when it is off, then issues a fresh pairing code.
    pub(crate) async fn open_remote_control(&mut self) {
        let Some(port) = self.remote_control.port else {
            self.remote_control.view.error = Some(REMOTE_CONTROL_DISABLED.to_string());

            return;
        };
        self.remote_control.view.error = None;
        if self.remote_control.server.is_none() {
            let host = RemoteHost {
                devices: Arc::clone(&self.remote_control.devices),
                revisions: self.remote_control.subscribe_revisions(),
                sessions: SessionService::new(Arc::new(
                    self.sessions
                        .handle()
                        .with_initiator(CommandInitiator::Remote),
                )),
            };
            let started = self.remote_control.launcher.start(port, host).await;
            match started {
                Ok(server) => {
                    self.remote_control.generation += 1;
                    forward_status(
                        self.remote_control.generation,
                        server.status(),
                        self.services.event_sender(),
                    );
                    self.remote_control.server = Some(server);
                }
                Err(error) => self.remote_control.view.error = Some(error),
            }
        }

        self.issue_remote_pairing_code();
        self.refresh_remote_devices().await;
    }

    /// Replaces the pairing code shown while the listener runs.
    pub(crate) fn issue_remote_pairing_code(&mut self) {
        let Some(server) = self.remote_control.server.as_ref() else {
            return;
        };

        match server.issue_pairing_code() {
            Ok(pairing_code) => {
                self.remote_control.view.listener = Some(RemoteListenerView {
                    pairing_code,
                    url: format!("http://{}/", server.local_addr()),
                });
            }
            Err(error) => self.remote_control.view.error = Some(error.to_string()),
        }
    }

    /// Stops the listener and disconnects every remote client.
    pub(crate) async fn stop_remote_control(&mut self) {
        if let Some(server) = self.remote_control.server.take() {
            server.shutdown().await;
        }
        self.remote_control.generation += 1;
        self.remote_control.connections.clear();
        self.remote_control.view.listener = None;
        for device in &mut self.remote_control.view.devices {
            device.is_connected = false;
        }
    }

    /// Forgets one paired device, closing its connection when it is live.
    pub(crate) async fn revoke_remote_device(&mut self, device_id: &str) {
        let revoked = match self.remote_control.server.as_ref() {
            Some(server) => server.revoke_device(device_id).await,
            None => self
                .remote_control
                .devices
                .delete_device(device_id)
                .await
                .map_err(|error| format!("Could not remove the device: {error}")),
        };
        self.remote_control.view.error = revoked.err();
        self.refresh_remote_devices().await;
    }

    /// Applies connection changes from the listener identified by
    /// `generation`, ignoring a listener that already stopped.
    pub(crate) async fn apply_remote_control_status(
        &mut self,
        generation: u64,
        status: RemoteServerStatus,
    ) {
        if generation != self.remote_control.generation {
            return;
        }

        self.remote_control.connections = status.connections;
        self.refresh_remote_devices().await;
    }

    /// Reloads paired devices and marks the connected ones.
    async fn refresh_remote_devices(&mut self) {
        match self.remote_control.devices.list_devices().await {
            Ok(devices) => {
                let connections = &self.remote_control.connections;
                self.remote_control.view.devices = devices
                    .into_iter()
                    .map(|device| RemoteDeviceView {
                        is_connected: connections
                            .iter()
                            .any(|connection| connection.device_id == device.id),
                        id: device.id,
                        name: device.name,
                    })
                    .collect();
            }
            Err(error) => {
                self.remote_control.view.error =
                    Some(format!("Could not load paired devices: {error}"));
            }
        }
    }
}

/// Forwards listener connection changes into the app reducer until the
/// listener stops. The app owns the listener, so a closed event channel
/// only drops the remaining updates.
fn forward_status(
    generation: u64,
    mut status_rx: watch::Receiver<RemoteServerStatus>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) {
    spawn_detached(async move {
        while status_rx.changed().await.is_ok() {
            let status = status_rx.borrow_and_update().clone();
            let _ = event_tx.send(AppEvent::RemoteControlStatusUpdated { generation, status });
        }
    });
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
