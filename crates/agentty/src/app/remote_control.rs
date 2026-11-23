//! Remote-control lifecycle: starts the loopback listener, optionally links
//! it to a relay (`--remote-relay`, else the platform's relay while this
//! Agentty is registered there), tracks paired devices, and tells connected
//! clients when session state changes.
//!
//! Remote clients reach sessions only through
//! [`App::remote_session_service`] over the session-runtime mailbox, so their
//! commands share terminal admission, the host limits them to the remote
//! scope, and model work stays behind the worker boundary.

use std::sync::Arc;

use ag_contracts::{ResponseStyle, SpeedMode};
use ag_remote::{
    DeviceStore, PublicKey, RelayStatus, RemoteConnection, RemoteHost, RemoteRelayConfig,
    RemoteServerStatus, StaticKeypair, pairing_link, pairing_link_on_page,
};
use ag_session::{HostProjectStatus, SessionListEntry};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app::execution_graph::GraphDriverReport;
use crate::app::link_copy::LinkCopy;
use crate::app::session_api::{HostInputs, LiveSessionDetails};
use crate::app::spawn::spawn_detached;
use crate::app::{App, AppEvent, review};
use crate::domain::setting::SettingName;
use crate::infra::browser::BrowserClient;
use crate::infra::db::AppRepositories;
use crate::infra::platform::RemoteControlService;
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
    /// Latest authenticated connections reported by the running listener.
    connections: Vec<RemoteConnection>,
    devices: Arc<dyn DeviceStore>,
    /// Identifies the running listener so late status from a stopped one
    /// is ignored.
    generation: u64,
    launcher: Arc<dyn RemoteControlLauncher>,
    /// Copies the pairing link for another device.
    link_copy: LinkCopy,
    /// Relay the running listener registers with and the key devices pin,
    /// while it is linked to one.
    linked_relay: Option<LinkedRelay>,
    /// Relay of the platform this Agentty is registered with, used unless
    /// `--remote-relay` names one.
    platform_relay: Option<RemoteControlService>,
    /// Loopback port, or `None` until `--experimental-remote` enables
    /// remote control.
    port: Option<u16>,
    /// Relay base URL from `--remote-relay`.
    relay_url: Option<String>,
    /// What the session API reported from host memory at the latest check,
    /// held only while a listener runs with a device connected.
    reported_state: Option<ReportedState>,
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
            connections: Vec::new(),
            devices: Arc::new(RepositoryDeviceStore::new(repositories.remote_devices())),
            generation: 0,
            launcher,
            link_copy: LinkCopy::new(clipboard, |request_id, is_copied| {
                AppEvent::RemoteLinkCopied {
                    is_copied,
                    request_id,
                }
            }),
            linked_relay: None,
            platform_relay: None,
            port: None,
            relay_url: None,
            reported_state: None,
            revision_tx,
            server: None,
            view: RemoteControlView::default(),
        }
    }

    /// Returns whether `--experimental-remote` enabled remote control.
    pub(crate) fn is_enabled(&self) -> bool {
        self.port.is_some()
    }

    /// Returns whether `--remote-relay` names a relay, which then wins over
    /// the platform's.
    pub(crate) fn has_explicit_relay(&self) -> bool {
        self.relay_url.is_some()
    }

    /// Returns the port the listener binds: the configured one with
    /// `--experimental-remote`, otherwise a free one while the platform's
    /// relay is available, since only devices reaching it through that
    /// relay are expected then.
    fn listen_port(&self) -> Option<u16> {
        self.port
            .or_else(|| self.platform_relay.as_ref().map(|_| 0))
    }

    /// Returns the relay a listener starting now registers with:
    /// `--remote-relay` when set, else the platform's.
    fn relay_to_link(&self) -> Option<(String, Option<String>)> {
        if let Some(relay_url) = &self.relay_url {
            return Some((relay_url.clone(), None));
        }

        self.platform_relay
            .as_ref()
            .map(|service| (service.relay_url.clone(), Some(service.pairing_url.clone())))
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
    /// its result would describe a link that is no longer shown.
    fn forget_link_copy(&mut self) {
        self.link_copy.forget();
        self.view.notice = None;
    }
}

/// Relay a running listener registers with.
struct LinkedRelay {
    /// Public key devices pin.
    instance_key: PublicKey,
    /// Page pairing links open in place of the relay's own, such as a
    /// platform's pairing page.
    pairing_page: Option<String>,
    /// Relay base URL.
    url: String,
}

impl LinkedRelay {
    /// Builds the link that pairs a device through this relay with
    /// `pairing_code`.
    fn pairing_link(&self, pairing_code: &str) -> String {
        match &self.pairing_page {
            Some(page) => pairing_link_on_page(page, &self.url, &self.instance_key, pairing_code),
            None => pairing_link(&self.url, &self.instance_key, pairing_code),
        }
    }
}

/// What the session API reports from host memory, without database reads,
/// compared once per foreground cycle to tell remote clients when it
/// changes.
///
/// Each part comes from the helper the session API itself reads, so an
/// input added there is compared here too. It holds no clock reading: a
/// running session reports when its timer started and the seconds it had
/// already run, not a ticking value.
#[derive(Debug, Eq, PartialEq)]
struct ReportedState {
    /// Proposals whose chat reply is being decided, which hides their
    /// questions from a session snapshot.
    deciding_proposal_replies: Vec<String>,
    /// What live graph drivers add to persisted graph progress.
    graph_drivers: Vec<GraphDriverReport>,
    /// In-memory inputs of the host context.
    host: HostInputs,
    /// Active project status, as the session listing reports it.
    project: HostProjectStatus,
    /// Each loaded session, in list order.
    sessions: Vec<ReportedSession>,
}

/// What the session API reports about one loaded session from memory.
#[derive(Debug, Eq, PartialEq)]
struct ReportedSession {
    /// Live snapshot details, with the shown focused review's invocation
    /// token, which changes whenever the shown review does, instead of its
    /// text.
    details: LiveSessionDetails<Uuid>,
    /// Session list entry.
    entry: SessionListEntry,
    /// Response style, which the snapshot reads from the stored row that
    /// the loaded session mirrors.
    response_style: ResponseStyle,
    /// Response speed, which the snapshot reads from the stored row that
    /// the loaded session mirrors.
    speed_mode: SpeedMode,
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
    ///
    /// The foreground cycle publishes changes to what the API reports from
    /// memory through [`Self::publish_reported_state_changes`]; callers
    /// publish explicitly only state held in the database alone, such as
    /// transcripts and graph tables.
    pub(crate) fn publish_session_change(&self) {
        self.remote_control
            .revision_tx
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// Tells connected remote clients when anything the session API reports
    /// from host memory changed since the previous check: a session's list
    /// entry or live details, the project status, the host's models, a
    /// reply being decided, or a live graph driver. The foreground cycle
    /// calls it once per pass, so a change reaches devices however it was
    /// made.
    ///
    /// Does nothing while no device is connected; the first connection
    /// starts the comparison.
    pub(crate) fn publish_reported_state_changes(&mut self) {
        let Some(previous_state) = self.remote_control.reported_state.as_ref() else {
            return;
        };

        let reported_state = self.reported_state();
        if reported_state == *previous_state {
            return;
        }

        self.remote_control.reported_state = Some(reported_state);
        self.publish_session_change();
    }

    /// Captures what the session API reports from host memory now.
    fn reported_state(&self) -> ReportedState {
        // The listing maps the loaded sessions in order, so each entry
        // pairs with its own session.
        let sessions = self
            .list_api_session_entries()
            .into_iter()
            .zip(self.sessions.sessions())
            .map(|(entry, session)| ReportedSession {
                details: self.live_session_details(session, review::shown_focused_review_id),
                entry,
                response_style: session.response_style,
                speed_mode: session.speed_mode,
            })
            .collect();

        ReportedState {
            deciding_proposal_replies: self.deciding_graph_proposal_replies(),
            graph_drivers: self.live_graph_driver_reports(),
            host: self.api_host_inputs(),
            project: self.api_project_status(),
            sessions,
        }
    }

    /// Starts comparing what the session API reports once a device
    /// connects, and stops once none is connected.
    ///
    /// A device reads the state as soon as it authenticates, before the
    /// host hears of its connection, and nothing was compared meanwhile. The
    /// first connection therefore tells devices to read again, from the
    /// state later checks compare against.
    fn follow_connected_devices(&mut self, was_connected: bool) {
        if self.remote_control.connections.is_empty() {
            self.remote_control.reported_state = None;

            return;
        }
        if was_connected {
            return;
        }

        self.remote_control.reported_state = Some(self.reported_state());
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

    /// Stops remote control while it runs, and starts it otherwise.
    pub(crate) async fn toggle_remote_control(&mut self) {
        if self.remote_control.view.listener.is_some() {
            self.stop_remote_control().await;
        } else {
            self.start_remote_control().await;
        }
    }

    /// Starts the listener when it is off, then issues a fresh pairing code.
    pub(crate) async fn start_remote_control(&mut self) {
        let Some(port) = self.remote_control.listen_port() else {
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

    /// Starts the listener, linked to `--remote-relay` when it names a
    /// relay, else to the platform's relay while this Agentty is
    /// registered there.
    async fn start_remote_listener(&mut self, port: u16) {
        let (relay, linked_relay) = match self.remote_control.relay_to_link() {
            Some((url, pairing_page)) => match self.remote_instance_key().await {
                Ok(instance_key) => {
                    let linked_relay = LinkedRelay {
                        instance_key: *instance_key.public(),
                        pairing_page,
                        url: url.clone(),
                    };

                    (
                        Some(RemoteRelayConfig { instance_key, url }),
                        Some(linked_relay),
                    )
                }
                Err(error) => {
                    self.remote_control.view.error = Some(error);

                    return;
                }
            },
            None => (None, None),
        };
        let host = RemoteHost {
            devices: Arc::clone(&self.remote_control.devices),
            revisions: self.remote_control.subscribe_revisions(),
            sessions: self.remote_session_service(),
        };
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
                self.remote_control.linked_relay = linked_relay;
                self.remote_control.server = Some(server);
            }
            Err(error) => self.remote_control.view.error = Some(error),
        }
    }

    /// Loads this installation's relay key, creating and saving it on first
    /// use. Devices pin its public half, so it must survive restarts.
    pub(crate) async fn remote_instance_key(&self) -> Result<StaticKeypair, String> {
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
                let relay_status = self
                    .remote_control
                    .view
                    .listener
                    .as_ref()
                    .and_then(|listener| listener.relay.as_ref())
                    .map_or(RelayStatus::Connecting, |relay| relay.status);
                let relay = self.remote_control.linked_relay.as_ref().map(|linked| {
                    RemoteRelayView::new(linked.pairing_link(&pairing_code), relay_status)
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

    /// Copies the relay pairing link through the terminal. The `tmux` step
    /// runs in the background, and its result arrives as
    /// [`AppEvent::RemoteLinkCopied`].
    pub(crate) fn copy_remote_link(&mut self) {
        let Some(link) = self.remote_control.relay_link() else {
            return;
        };
        self.remote_control.view.notice = None;
        self.remote_control
            .link_copy
            .start(link, self.services.event_sender());
    }

    /// Finishes the link copy identified by `request_id` and shows whether
    /// it reached the terminal, ignoring a copy that a later one or a new
    /// link replaced.
    pub(crate) fn apply_remote_link_copied(&mut self, request_id: u64, is_copied: bool) {
        let Some(is_copied) = self.remote_control.link_copy.finish(request_id, is_copied) else {
            return;
        };
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
        self.remote_control.linked_relay = None;
        self.remote_control.reported_state = None;
        self.remote_control.view.listener = None;
        self.remote_control.forget_link_copy();
        self.remote_control.view.pairing_requests.clear();
        for device in &mut self.remote_control.view.devices {
            device.is_connected = false;
        }
    }

    /// Replaces the platform relay remote control may use. A running
    /// listener that `--remote-relay` does not link restarts, so it
    /// registers with the new relay, or with none: devices connected
    /// through the old relay drop, and when only the platform's relay made
    /// remote control available, it stays off.
    pub(crate) async fn set_platform_relay(&mut self, relay: Option<RemoteControlService>) {
        if self.remote_control.platform_relay == relay {
            return;
        }
        self.remote_control.platform_relay = relay;
        if self.remote_control.has_explicit_relay() || self.remote_control.server.is_none() {
            return;
        }

        self.stop_remote_control().await;
        if self.remote_control.listen_port().is_some() {
            self.start_remote_control().await;
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

        let was_connected = !self.remote_control.connections.is_empty();
        self.remote_control.connections = status.connections;
        self.follow_connected_devices(was_connected);
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
            relay.status = status.relay.unwrap_or(RelayStatus::Connecting);
        }
        self.refresh_remote_devices().await;
    }

    /// Reloads paired devices, marks the connected ones, and keeps the
    /// highlight on a listed device.
    pub(crate) async fn refresh_remote_devices(&mut self) {
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
    // A paired device can reconnect as soon as the listener starts, before
    // this subscribed, so the status current at subscription goes first.
    status_rx.mark_changed();
    spawn_detached(async move {
        while status_rx.changed().await.is_ok() {
            let status = status_rx.borrow_and_update().clone();
            let _ = event_tx.send(AppEvent::RemoteControlStatusUpdated { generation, status });
        }
    });
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
pub(super) mod tests;
