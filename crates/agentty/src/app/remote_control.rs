//! Remote-control lifecycle: starts the loopback listener, optionally links
//! it to a relay, tracks paired devices, and tells connected clients when
//! session state changes.
//!
//! Remote clients reach sessions only through [`SessionService`] over the
//! session-runtime mailbox, so their commands share terminal admission and
//! model work stays behind the worker boundary.

use std::collections::HashMap;
use std::sync::Arc;

use ag_remote::{
    DeviceStore, PublicKey, RelayStatus, RemoteConnection, RemoteHost, RemoteRelayConfig,
    RemoteServerStatus, StaticKeypair, pairing_link,
};
use ag_session::{SessionId, SessionService};
use tokio::sync::{mpsc, watch};

use crate::app::command_gateway::CommandInitiator;
use crate::app::spawn::{spawn_blocking_detached, spawn_detached};
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
    /// Identifies the latest link copy so the result of an earlier one, or
    /// of a link that is no longer shown, is ignored.
    link_copy_request: u64,
    /// Status line and queued-action labels of each session showing either,
    /// as remote clients last learned of them.
    live_details: HashMap<SessionId, (Option<String>, Vec<String>)>,
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
            link_copy_request: 0,
            live_details: HashMap::new(),
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

    /// Returns the state projected into the Remote tab.
    pub(crate) fn view(&self) -> &RemoteControlView {
        &self.view
    }

    /// Moves the paired-device highlight one row down, stopping at the last
    /// device.
    pub(crate) fn select_next_device(&mut self) {
        let last_device_index = self.view.devices.len().saturating_sub(1);
        self.view.selected_device_index =
            (self.view.selected_device_index + 1).min(last_device_index);
    }

    /// Moves the paired-device highlight one row up, stopping at the first
    /// device.
    pub(crate) fn select_previous_device(&mut self) {
        self.view.selected_device_index = self.view.selected_device_index.saturating_sub(1);
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

    /// Clears the copy notice and disowns any copy still running, because
    /// its result would describe a link that is no longer shown. Returns the
    /// request a new copy reports under.
    fn forget_link_copy(&mut self) -> u64 {
        self.link_copy_request += 1;
        self.view.notice = None;

        self.link_copy_request
    }
}

impl App {
    /// Offers the experimental Remote tab with its paired devices. Remote
    /// control listens on the loopback `port` once it starts and accepts
    /// requests that name a loopback host or one of `allowed_hosts`. With
    /// `relay_url`, the listener also registers with that relay.
    pub async fn enable_remote_control(
        &mut self,
        port: u16,
        allowed_hosts: Vec<String>,
        relay_url: Option<String>,
    ) {
        self.remote_control.allowed_hosts = allowed_hosts;
        self.remote_control.port = Some(port);
        self.remote_control.relay_url = relay_url;
        self.tabs.enable_remote();
        self.refresh_remote_devices().await;
    }

    /// Tells connected remote clients that session state changed, so they
    /// fetch it again. Bursts coalesce into one notification.
    pub(crate) fn publish_session_change(&self) {
        self.remote_control
            .revision_tx
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// Tells connected remote clients when a session's status line or its
    /// queued actions changed, such as a new thought after `Working...`, a
    /// review step, or a sync queued in the terminal, which no session event
    /// marks.
    pub(crate) fn publish_live_session_changes(&mut self) {
        let live_details = self
            .sessions
            .sessions()
            .iter()
            .filter_map(|session| {
                let status_line = session.status_line(self.session_progress_message(&session.id));
                let queued_actions = session.queued_action_labels();

                (status_line.is_some() || !queued_actions.is_empty())
                    .then(|| (session.id.clone(), (status_line, queued_actions)))
            })
            .collect::<HashMap<_, _>>();
        if live_details == self.remote_control.live_details {
            return;
        }

        self.remote_control.live_details = live_details;
        self.publish_session_change();
    }

    /// Prepares the Remote tab for display: drops the outcome of earlier
    /// actions and, while the listener runs, replaces the pairing code,
    /// which may be spent or expired by now.
    pub(crate) fn show_remote_control(&mut self) {
        self.remote_control.view.error = None;
        self.remote_control.view.notice = None;
        self.issue_remote_pairing_code();
    }

    /// Starts the listener when it is off, then issues a fresh pairing code.
    pub(crate) async fn start_remote_control(&mut self) {
        let Some(port) = self.remote_control.port else {
            self.remote_control.view.error = Some(REMOTE_CONTROL_DISABLED.to_string());

            return;
        };
        self.remote_control.view.error = None;
        self.remote_control.forget_link_copy();
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
        self.remote_control.forget_link_copy();
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
    ///
    /// The `tmux` step runs in the background, because it waits for a `tmux`
    /// command. Its result arrives as [`AppEvent::RemoteLinkCopied`].
    pub(crate) fn copy_remote_link(&mut self) {
        let Some(link) = self.remote_control.relay_link() else {
            return;
        };
        let request_id = self.remote_control.forget_link_copy();
        let clipboard = Arc::clone(&self.remote_control.clipboard);
        let event_tx = self.services.event_sender();
        spawn_blocking_detached(move || {
            let is_copied = clipboard.copy_through_tmux(&link);
            // The app owns the event channel, so a closed one only drops the
            // result.
            let _ = event_tx.send(AppEvent::RemoteLinkCopied {
                is_copied,
                request_id,
            });
        });
    }

    /// Finishes the link copy identified by `request_id` and shows whether
    /// it reached the terminal, ignoring a copy that a later one or a new
    /// link replaced.
    ///
    /// When `tmux` did not copy the link, this asks the terminal from the
    /// foreground, the only thread that may write to it.
    pub(crate) fn apply_remote_link_copied(&mut self, request_id: u64, is_copied: bool) {
        if request_id != self.remote_control.link_copy_request {
            return;
        }

        let is_copied = is_copied
            || self
                .remote_control
                .relay_link()
                .is_some_and(|link| self.remote_control.clipboard.copy_through_terminal(&link));
        let notice = if is_copied {
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
        self.remote_control.forget_link_copy();
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

    /// Reloads paired devices, marks the connected ones, and keeps the
    /// highlight on a listed device.
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
                let last_device_index = self.remote_control.view.devices.len().saturating_sub(1);
                self.remote_control.view.selected_device_index = self
                    .remote_control
                    .view
                    .selected_device_index
                    .min(last_device_index);
            }
            Err(error) => {
                self.remote_control.view.error =
                    Some(format!("Could not load paired devices: {error}"));
            }
        }
    }
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
