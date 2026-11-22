//! Remote-control lifecycle: starts the loopback listener, optionally links
//! it to a relay, tracks paired devices, and tells connected clients when
//! session state changes.
//!
//! Remote clients reach sessions only through [`SessionService`] over the
//! session-runtime mailbox, so their commands share terminal admission and
//! model work stays behind the worker boundary.

use std::sync::Arc;

use ag_remote::{
    DeviceStore, PublicKey, RelayStatus, RemoteConnection, RemoteHost, RemoteRelayConfig,
    RemoteServerStatus, StaticKeypair,
};
use ag_session::SessionService;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::sync::{mpsc, watch};

use crate::app::command_gateway::CommandInitiator;
use crate::app::spawn::spawn_detached;
use crate::app::{App, AppEvent};
use crate::domain::setting::SettingName;
use crate::infra::browser::BrowserClient;
use crate::infra::db::AppRepositories;
use crate::infra::remote_control::{
    RemoteControlLauncher, RemoteControlServer, RepositoryDeviceStore,
};
use crate::infra::terminal_clipboard::TerminalClipboard;
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView, RemotePairingRequestView,
    RemoteRelayView,
};

/// Loopback port used unless `--remote-control-port` selects another.
pub const DEFAULT_REMOTE_CONTROL_PORT: u16 = 4646;
/// Reported when remote control is started without its launch flag.
const REMOTE_CONTROL_DISABLED: &str = "Remote control needs `agentty --experimental-remote`.";
/// Confirms a copied pairing link.
const LINK_COPIED: &str = "Link copied. It works once.";
/// Reported when the terminal could not be asked to copy.
const LINK_NOT_COPIED: &str = "Could not copy the link; select it instead.";
/// Reported when the relay key cannot be loaded, created, or saved.
const REMOTE_KEY_UNAVAILABLE: &str = "Could not load or save the remote key.";
/// Reported when the saved relay key does not parse.
const REMOTE_KEY_INVALID: &str = "The stored remote key is invalid.";

/// Remote-control listener ownership and its projected view.
pub(crate) struct RemoteControl {
    /// Host names a tunnel may use besides loopback.
    allowed_hosts: Vec<String>,
    /// Opens the pairing link on this machine.
    browser: Arc<dyn BrowserClient>,
    /// Copies the pairing link for another device.
    clipboard: Arc<dyn TerminalClipboard>,
    /// Latest authenticated connections reported by the running listener.
    connections: Vec<RemoteConnection>,
    devices: Arc<dyn DeviceStore>,
    /// Identifies the running listener so late status from a stopped one
    /// is ignored.
    generation: u64,
    /// Public key devices pin, while a relay-linked listener runs.
    instance_key: Option<PublicKey>,
    launcher: Arc<dyn RemoteControlLauncher>,
    /// Loopback port, or `None` until `--experimental-remote` enables
    /// remote control.
    port: Option<u16>,
    /// Relay base URL from `--remote-relay`.
    relay_url: Option<String>,
    revision_tx: watch::Sender<u64>,
    server: Option<Box<dyn RemoteControlServer>>,
    view: RemoteControlView,
}

impl RemoteControl {
    /// Creates disabled remote control that starts listeners through
    /// `launcher`, persists paired devices in `repositories`, and opens or
    /// copies pairing links through `browser` and `clipboard`.
    pub(crate) fn new(
        launcher: Arc<dyn RemoteControlLauncher>,
        browser: Arc<dyn BrowserClient>,
        clipboard: Arc<dyn TerminalClipboard>,
        repositories: &AppRepositories,
    ) -> Self {
        let (revision_tx, _revision_rx) = watch::channel(0);

        Self {
            allowed_hosts: Vec::new(),
            browser,
            clipboard,
            connections: Vec::new(),
            devices: Arc::new(RepositoryDeviceStore::new(repositories.remote_devices())),
            generation: 0,
            instance_key: None,
            launcher,
            port: None,
            relay_url: None,
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

    /// Returns the current relay pairing link, when one is shown.
    fn relay_link(&self) -> Option<String> {
        let listener = self.view.listener.as_ref()?;

        listener.relay.as_ref().map(|relay| relay.link.clone())
    }
}

impl App {
    /// Enables the experimental `/remote-control` command, listening on the
    /// loopback `port` when it starts and accepting requests that name a
    /// loopback host or one of `allowed_hosts`. With `relay_url`, the
    /// listener also registers with that relay.
    pub fn enable_remote_control(
        &mut self,
        port: u16,
        allowed_hosts: Vec<String>,
        relay_url: Option<String>,
    ) {
        self.remote_control.allowed_hosts = allowed_hosts;
        self.remote_control.port = Some(port);
        self.remote_control.relay_url = relay_url;
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
        self.remote_control.view.notice = None;
        if self.remote_control.server.is_none() {
            self.start_remote_listener(port).await;
        }

        self.issue_remote_pairing_code();
        self.refresh_remote_devices().await;
    }

    /// Starts the listener, linked to the configured relay when there is one.
    async fn start_remote_listener(&mut self, port: u16) {
        let relay = match self.remote_control.relay_url.clone() {
            Some(url) => match self.remote_instance_key().await {
                Ok(instance_key) => Some(RemoteRelayConfig { instance_key, url }),
                Err(error) => {
                    self.remote_control.view.error = Some(error);

                    return;
                }
            },
            None => None,
        };
        let host = RemoteHost {
            devices: Arc::clone(&self.remote_control.devices),
            revisions: self.remote_control.subscribe_revisions(),
            sessions: SessionService::new(Arc::new(
                self.sessions
                    .handle()
                    .with_initiator(CommandInitiator::Remote),
            )),
        };
        let instance_key = relay.as_ref().map(|relay| *relay.instance_key.public());
        let started = self
            .remote_control
            .launcher
            .start(port, self.remote_control.allowed_hosts.clone(), relay, host)
            .await;
        match started {
            Ok(server) => {
                self.remote_control.generation += 1;
                forward_status(
                    self.remote_control.generation,
                    server.status(),
                    self.services.event_sender(),
                );
                self.remote_control.instance_key = instance_key;
                self.remote_control.server = Some(server);
            }
            Err(error) => self.remote_control.view.error = Some(error),
        }
    }

    /// Loads this installation's relay key, creating and saving it on first
    /// use. Devices pin its public half, so it must survive restarts.
    async fn remote_instance_key(&self) -> Result<StaticKeypair, String> {
        let settings = self.services.db().settings();
        let stored = settings
            .get_setting(SettingName::RemoteInstanceKey)
            .await
            .ok()
            .ok_or(REMOTE_KEY_UNAVAILABLE)?;
        if let Some(stored) = stored {
            return parse_instance_key(&stored).ok_or(REMOTE_KEY_INVALID.to_string());
        }
        let instance_key = StaticKeypair::generate()
            .ok()
            .ok_or(REMOTE_KEY_UNAVAILABLE)?;
        settings
            .upsert_setting(
                SettingName::RemoteInstanceKey,
                &format_instance_key(&instance_key),
            )
            .await
            .ok()
            .ok_or(REMOTE_KEY_UNAVAILABLE)?;

        Ok(instance_key)
    }

    /// Replaces the pairing code shown while the listener runs.
    pub(crate) fn issue_remote_pairing_code(&mut self) {
        let Some(server) = self.remote_control.server.as_ref() else {
            return;
        };

        match server.issue_pairing_code() {
            Ok(pairing_code) => {
                let is_connected = self
                    .remote_control
                    .view
                    .listener
                    .as_ref()
                    .and_then(|listener| listener.relay.as_ref())
                    .is_some_and(|relay| relay.is_connected);
                let relay = self
                    .remote_control
                    .relay_url
                    .as_deref()
                    .zip(self.remote_control.instance_key.as_ref())
                    .map(|(relay_url, instance_key)| {
                        RemoteRelayView::new(
                            pairing_link(relay_url, instance_key, &pairing_code),
                            is_connected,
                        )
                    });
                self.remote_control.view.listener = Some(RemoteListenerView {
                    pairing_code,
                    relay,
                    url: format!("http://{}/", server.local_addr()),
                });
            }
            Err(error) => self.remote_control.view.error = Some(error.to_string()),
        }
    }

    /// Opens the relay pairing link in this machine's browser.
    pub(crate) fn open_remote_link(&mut self) {
        if let Some(link) = self.remote_control.relay_link() {
            self.remote_control.browser.open(&link);
        }
    }

    /// Copies the relay pairing link through the terminal.
    pub(crate) fn copy_remote_link(&mut self) {
        let Some(link) = self.remote_control.relay_link() else {
            return;
        };
        let notice = if self.remote_control.clipboard.copy(&link) {
            LINK_COPIED
        } else {
            LINK_NOT_COPIED
        };
        self.remote_control.view.notice = Some(notice.to_string());
    }

    /// Stops the listener and disconnects every remote client.
    pub(crate) async fn stop_remote_control(&mut self) {
        if let Some(server) = self.remote_control.server.take() {
            server.shutdown().await;
        }
        self.remote_control.generation += 1;
        self.remote_control.connections.clear();
        self.remote_control.instance_key = None;
        self.remote_control.view.listener = None;
        self.remote_control.view.notice = None;
        self.remote_control.view.pairing_requests.clear();
        for device in &mut self.remote_control.view.devices {
            device.is_connected = false;
        }
    }

    /// Approves or denies the oldest device waiting to pair. The listener's
    /// next status removes the request either way.
    pub(crate) fn settle_remote_pairing(&mut self, approve: bool) {
        let Some(server) = self.remote_control.server.as_ref() else {
            return;
        };
        let Some(request) = self.remote_control.view.pairing_requests.first() else {
            return;
        };

        let is_settled = if approve {
            server.approve_pairing(request.id)
        } else {
            server.deny_pairing(request.id)
        };
        if !is_settled {
            self.remote_control.view.error =
                Some("That pairing request is no longer pending.".to_string());
        }
        self.remote_control.view.pairing_requests.remove(0);
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
        self.remote_control.view.pairing_requests = status
            .pairing_requests
            .into_iter()
            .map(|request| RemotePairingRequestView {
                confirmation: request.confirmation,
                device_name: request.device_name,
                id: request.id,
            })
            .collect();
        let relay = self
            .remote_control
            .view
            .listener
            .as_mut()
            .and_then(|listener| listener.relay.as_mut());
        if let Some(relay) = relay {
            relay.is_connected = status.relay == Some(RelayStatus::Connected);
        }
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

/// Builds the link a device opens to pair through `relay_url`.
///
/// `relay_url` is a `ws://` or `wss://` URL, as `--remote-relay` requires.
/// The relay serves the pairing page at the same place over `http://` or
/// `https://`. Everything after `#` stays in the browser, so the relay never
/// sees the instance key or the one-time code it carries.
pub(crate) fn pairing_link(
    relay_url: &str,
    instance_key: &PublicKey,
    pairing_code: &str,
) -> String {
    let relay_url = relay_url.trim_end_matches('/');
    // `ws://` becomes `http://` and `wss://` becomes `https://`.
    let page = format!("http{}", relay_url.get(2..).unwrap_or_default());

    format!(
        "{page}/#relay={}&key={}&code={}",
        utf8_percent_encode(relay_url, NON_ALPHANUMERIC),
        instance_key.to_hex(),
        utf8_percent_encode(pairing_code, NON_ALPHANUMERIC),
    )
}

/// Stored form of an instance key: its private and public halves as
/// hexadecimal, joined by `:`.
fn format_instance_key(instance_key: &StaticKeypair) -> String {
    format!(
        "{}:{}",
        hex::encode(instance_key.private_bytes()),
        instance_key.public().to_hex()
    )
}

/// Reads the stored form [`format_instance_key`] writes.
fn parse_instance_key(stored: &str) -> Option<StaticKeypair> {
    let (private, public) = stored.split_once(':')?;
    let public = PublicKey::from_hex(public).ok()?;
    let private = hex::decode(private).ok()?;

    StaticKeypair::from_parts(&private, public.as_bytes()).ok()
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
