//! Remote-control lifecycle: starts the server, which registers with the
//! relay of the platform this Agentty is registered with, tracks paired
//! devices, and tells connected clients when session state changes.
//!
//! Remote clients reach sessions only through
//! [`App::remote_session_service`] over the session-runtime mailbox, so their
//! commands share terminal admission, the host limits them to the remote
//! scope, and model work stays behind the worker boundary.

use std::sync::Arc;

use ag_contracts::{ResponseStyle, SpeedMode};
use ag_remote::{
    DeviceStore, PublicKey, RelayStatus, RemoteConnection, RemoteHost, RemoteRelayConfig,
    RemoteServerStatus, StaticKeypair, pairing_link_on_page,
};
use ag_session::{HostProject, HostProjectStatus, SessionListEntry};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app::execution_graph::GraphDriverReport;
use crate::app::link_copy::LinkCopy;
use crate::app::session_api::{HostInputs, LiveSessionDetails, api_session_entries};
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
    RemoteControlView, RemoteDeviceView, RemotePairingRequestView, RemoteRelayView,
};

/// Confirms a copied pairing link.
const LINK_COPIED: &str = "Link copied. It works once.";
/// Reported when the terminal could not be asked to copy.
const LINK_NOT_COPIED: &str = "Could not copy the link; select it instead.";
/// Reported when the instance key cannot be loaded, created, or saved.
const REMOTE_KEY_UNAVAILABLE: &str = "Could not load or save the instance key.";
/// Reported when the saved instance key does not parse.
const REMOTE_KEY_INVALID: &str = "The stored instance key is invalid.";

/// Remote-control server ownership and its projected view.
pub(crate) struct RemoteControl {
    /// Opens the pairing link on this machine.
    browser: Arc<dyn BrowserClient>,
    /// Latest authenticated connections reported by the running server.
    connections: Vec<RemoteConnection>,
    devices: Arc<dyn DeviceStore>,
    /// Identifies the running server so late status from a stopped one is
    /// ignored.
    generation: u64,
    launcher: Arc<dyn RemoteControlLauncher>,
    /// Copies the pairing link for another device.
    link_copy: LinkCopy,
    /// Relay of the platform this Agentty is registered with, which remote
    /// control needs to start.
    platform_relay: Option<RemoteControlService>,
    /// What the session API reported from host memory at the latest check,
    /// held only while the server runs with a device connected.
    reported_state: Option<ReportedState>,
    revision_tx: watch::Sender<u64>,
    /// Running server and the relay it registers with.
    running: Option<RunningServer>,
    view: RemoteControlView,
}

impl RemoteControl {
    /// Creates stopped remote control that starts servers through
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
            platform_relay: None,
            reported_state: None,
            revision_tx,
            running: None,
            view: RemoteControlView::default(),
        }
    }

    /// Returns the state projected into the Platform tab.
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

    /// Returns the current pairing link, when one is shown.
    fn shown_pairing_link(&self) -> Option<String> {
        self.view.relay.as_ref().map(|relay| relay.link.clone())
    }

    /// Clears the copy notice and disowns any copy still running, because
    /// its result would describe a link that is no longer shown.
    fn forget_link_copy(&mut self) {
        self.link_copy.forget();
        self.view.notice = None;
    }
}

/// A running server with the relay it registers with.
struct RunningServer {
    relay: LinkedRelay,
    server: Box<dyn RemoteControlServer>,
}

/// Relay a running server registers with.
struct LinkedRelay {
    /// Public key devices pin.
    instance_key: PublicKey,
    /// The platform's pairing page, which pairing links open.
    pairing_page: String,
    /// Relay base URL.
    url: String,
}

impl LinkedRelay {
    /// Builds the link that pairs a device through this relay with
    /// `pairing_code`.
    fn pairing_link(&self, pairing_code: &str) -> String {
        pairing_link_on_page(
            &self.pairing_page,
            &self.url,
            &self.instance_key,
            pairing_code,
        )
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
    /// In-memory inputs of the host context that every project shares.
    host: HostInputs,
    /// Each loaded page, the active page first, then the viewed pages in
    /// ascending project order.
    pages: Vec<ReportedPage>,
    /// The projects the host lists, as the project listing reports them.
    projects: Vec<HostProject>,
}

/// What the session API reports about one loaded page from memory.
#[derive(Debug, Eq, PartialEq)]
struct ReportedPage {
    /// The project's status, as its session listing reports it.
    project: HostProjectStatus,
    /// Project whose sessions the page holds.
    project_id: i64,
    /// Each session on the page, in list order.
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
    /// entry or live details on any loaded page, a loaded page's project
    /// status, the project list, the host's models, a reply being decided,
    /// or a live graph driver. The foreground cycle
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
        let pages = self
            .sessions
            .state()
            .loaded_project_ids()
            .into_iter()
            .filter_map(|project_id| self.reported_page(project_id))
            .collect();

        ReportedState {
            deciding_proposal_replies: self.deciding_graph_proposal_replies(),
            graph_drivers: self.live_graph_driver_reports(),
            host: self.api_host_inputs(),
            pages,
            projects: self.api_projects(),
        }
    }

    /// Captures what the session API reports about `project_id`'s loaded
    /// page, or `None` when the host holds no page or context of it.
    fn reported_page(&self, project_id: i64) -> Option<ReportedPage> {
        let context = self.projects.context(project_id)?;
        let page = self.sessions.state().page_for_project(project_id)?;
        // The listing maps the page's sessions in order, so each entry pairs
        // with its own session.
        let sessions = api_session_entries(page)
            .into_iter()
            .zip(page.sessions())
            .map(|(entry, session)| ReportedSession {
                details: self.live_session_details(session, review::shown_focused_review_id),
                entry,
                response_style: session.response_style,
                speed_mode: session.speed_mode,
            })
            .collect();

        Some(ReportedPage {
            project: self.api_project_status_for(context),
            project_id,
            sessions,
        })
    }

    /// Starts comparing what the session API reports once a device
    /// connects, and stops once none is connected, when the viewed pages are
    /// dropped too.
    ///
    /// A device reads the state as soon as it authenticates, before the
    /// host hears of its connection, and nothing was compared meanwhile. The
    /// first connection therefore tells devices to read again, from the
    /// state later checks compare against.
    fn follow_connected_devices(&mut self, was_connected: bool) {
        if self.remote_control.connections.is_empty() {
            self.remote_control.reported_state = None;
            if was_connected {
                self.clear_viewed_pages();
            }

            return;
        }
        if was_connected {
            return;
        }

        self.remote_control.reported_state = Some(self.reported_state());
        self.publish_session_change();
    }

    /// Prepares remote control for display in the Platform tab: drops the
    /// outcome of earlier actions and, while the server runs, replaces the
    /// pairing code, which may be spent or expired by now.
    pub(crate) fn show_remote_control(&mut self) {
        self.remote_control.view.error = None;
        self.remote_control.view.notice = None;
        self.issue_remote_pairing_code();
    }

    /// Starts the server, while the platform's relay is available and it is
    /// off, then issues a fresh pairing code.
    pub(crate) async fn start_remote_control(&mut self) {
        let Some(service) = self.remote_control.platform_relay.clone() else {
            return;
        };
        self.remote_control.view.error = None;
        self.remote_control.forget_link_copy();
        if self.remote_control.running.is_none() {
            self.start_remote_server(service).await;
        }

        self.issue_remote_pairing_code();
        self.refresh_remote_devices().await;
    }

    /// Starts the server linked to the platform's relay `service`.
    async fn start_remote_server(&mut self, service: RemoteControlService) {
        let instance_key = match self.remote_instance_key().await {
            Ok(instance_key) => instance_key,
            Err(error) => {
                self.remote_control.view.error = Some(error);

                return;
            }
        };
        let relay = LinkedRelay {
            instance_key: *instance_key.public(),
            pairing_page: service.pairing_url,
            url: service.relay_url.clone(),
        };
        let host = RemoteHost {
            devices: Arc::clone(&self.remote_control.devices),
            revisions: self.remote_control.subscribe_revisions(),
            sessions: self.remote_session_service(),
        };
        let server = self.remote_control.launcher.start(
            RemoteRelayConfig {
                instance_key,
                url: service.relay_url,
            },
            host,
        );
        self.remote_control.generation += 1;
        forward_status(
            self.remote_control.generation,
            server.status(),
            self.services.event_sender(),
        );
        self.remote_control.running = Some(RunningServer { relay, server });
    }

    /// Loads this installation's instance key, creating and saving it on
    /// first use. Devices pin its public half, so it must survive restarts.
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

    /// Replaces the pairing link shown while the server runs.
    pub(crate) fn issue_remote_pairing_code(&mut self) {
        self.remote_control.forget_link_copy();
        let Some(running) = self.remote_control.running.as_ref() else {
            return;
        };

        match running.server.issue_pairing_code() {
            Ok(pairing_code) => {
                let status = self
                    .remote_control
                    .view
                    .relay
                    .as_ref()
                    .map_or(RelayStatus::Connecting, |relay| relay.status);
                self.remote_control.view.relay = Some(RemoteRelayView::new(
                    running.relay.pairing_link(&pairing_code),
                    status,
                ));
            }
            Err(error) => self.remote_control.view.error = Some(error.to_string()),
        }
    }

    /// Opens the pairing link in this machine's browser.
    pub(crate) fn open_remote_link(&mut self) {
        if let Some(link) = self.remote_control.shown_pairing_link() {
            self.remote_control.browser.open(&link);
        }
    }

    /// Copies the pairing link through the terminal. The `tmux` step runs
    /// in the background, and its result arrives as
    /// [`AppEvent::RemoteLinkCopied`].
    pub(crate) fn copy_remote_link(&mut self) {
        let Some(link) = self.remote_control.shown_pairing_link() else {
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

    /// Stops the server and disconnects every remote client.
    pub(crate) async fn stop_remote_control(&mut self) {
        if let Some(running) = self.remote_control.running.take() {
            running.server.shutdown().await;
        }
        self.remote_control.generation += 1;
        self.remote_control.connections.clear();
        self.remote_control.reported_state = None;
        self.remote_control.view.relay = None;
        self.remote_control.forget_link_copy();
        self.remote_control.view.pairing_requests.clear();
        for device in &mut self.remote_control.view.devices {
            device.is_connected = false;
        }
    }

    /// Replaces the platform relay remote control uses. Running remote
    /// control restarts, so it registers with the new relay, or stops when
    /// there is none: devices connected through the old relay drop.
    pub(crate) async fn set_platform_relay(&mut self, relay: Option<RemoteControlService>) {
        if self.remote_control.platform_relay == relay {
            return;
        }
        self.remote_control.platform_relay = relay;
        if self.remote_control.running.is_none() {
            return;
        }

        self.stop_remote_control().await;
        self.start_remote_control().await;
    }

    /// Approves or denies the oldest device waiting to pair. The server's
    /// next status removes the request either way.
    pub(crate) fn settle_remote_pairing(&mut self, approve: bool) {
        let Some(running) = self.remote_control.running.as_ref() else {
            return;
        };
        let Some(request) = self.remote_control.view.pairing_requests.first() else {
            return;
        };

        let is_settled = if approve {
            running.server.approve_pairing(request.id)
        } else {
            running.server.deny_pairing(request.id)
        };
        if !is_settled {
            self.remote_control.view.error =
                Some("That pairing request is no longer pending.".to_string());
        }
        self.remote_control.view.pairing_requests.remove(0);
    }

    /// Forgets one paired device, closing its connection when it is live.
    pub(crate) async fn revoke_remote_device(&mut self, device_id: &str) {
        let revoked = match self.remote_control.running.as_ref() {
            Some(running) => running.server.revoke_device(device_id).await,
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

    /// Applies connection changes from the server identified by
    /// `generation`, ignoring a server that already stopped.
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
        if let Some(relay) = self.remote_control.view.relay.as_mut() {
            relay.status = status.relay;
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

/// Forwards server connection changes into the app reducer until the
/// server stops. The app owns the server, so a closed event channel only
/// drops the remaining updates.
fn forward_status(
    generation: u64,
    mut status_rx: watch::Receiver<RemoteServerStatus>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) {
    // A paired device can reconnect as soon as the server starts, before
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
