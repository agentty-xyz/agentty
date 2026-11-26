//! Remote-control lifecycle, pairing code, device, status, and revision
//! tests.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ag_remote::{
    DeviceStore, DeviceStoreError, NewRemoteDevice, PairingRequest, RelayStatus, RemoteConnection,
    RemoteDevice, RemoteError, RemoteHost, RemoteRelayConfig, RemoteServerStatus, StaticKeypair,
    pairing_link_on_page,
};
use async_trait::async_trait;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::RemoteControl;
use crate::app::command_gateway::{
    CommandDelivery, CommandInitiator, SessionCommand, SessionMutation,
};
use crate::app::graph_reply_decision::ReplyDecision;
use crate::app::session::SessionGitStatus;
use crate::app::sync::ProjectSyncContext;
use crate::app::test_support::{add_listed_project, insert_loadable_session};
use crate::app::{App, AppEvent, AppRuntimeEvent, ProjectSyncPhase, ProjectSyncStatus};
use crate::domain::agent::{AgentKind, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::session::{
    ForgeKind, QueuedMessage, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Session,
    SessionDiffStats, SessionId, SessionSize, Status,
};
use crate::domain::setting::SettingName;
use crate::infra::browser::MockBrowserClient;
use crate::infra::platform::RemoteControlService;
use crate::infra::remote_control::{MockRemoteControlLauncher, RemoteControlServer};
use crate::infra::terminal_clipboard::MockTerminalClipboard;
use crate::presentation::remote_control::{RemoteDeviceView, RemotePairingRequestView};
use crate::test_support;

/// Calls the fake server received, in order.
type CallLog = Arc<Mutex<Vec<String>>>;

/// Pairing request the fake server reports as already settled.
const GONE_REQUEST_ID: u64 = 99;

/// Scripted server that records calls and publishes test-driven status.
struct FakeServer {
    calls: CallLog,
    codes: Mutex<Vec<Option<String>>>,
    revoke_result: Result<(), String>,
    status_tx: watch::Sender<RemoteServerStatus>,
}

#[async_trait]
impl RemoteControlServer for FakeServer {
    fn issue_pairing_code(&self) -> Result<String, RemoteError> {
        self.codes
            .lock()
            .expect("codes lock")
            .pop()
            .unwrap_or_else(|| Some("LAST-CODE".to_string()))
            .ok_or(RemoteError::Entropy)
    }

    fn status(&self) -> watch::Receiver<RemoteServerStatus> {
        self.status_tx.subscribe()
    }

    fn approve_pairing(&self, request_id: u64) -> bool {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("approve:{request_id}"));

        request_id != GONE_REQUEST_ID
    }

    fn deny_pairing(&self, request_id: u64) -> bool {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("deny:{request_id}"));

        request_id != GONE_REQUEST_ID
    }

    async fn revoke_device(&self, device_id: &str) -> Result<(), String> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("revoke:{device_id}"));

        self.revoke_result.clone()
    }

    async fn shutdown(self: Box<Self>) {
        self.calls
            .lock()
            .expect("calls lock")
            .push("shutdown".to_string());
    }
}

/// Handles a test keeps to drive and observe one fake server.
struct FakeServerProbe {
    calls: CallLog,
    host: Arc<Mutex<Option<RemoteHost>>>,
    relay: Arc<Mutex<Option<RemoteRelayConfig>>>,
    status_tx: watch::Sender<RemoteServerStatus>,
}

/// Builds a launcher that starts one fake server, issuing `codes` in order
/// before `LAST-CODE`.
fn fake_launcher(
    codes: Vec<Option<String>>,
    revoke_result: Result<(), String>,
) -> (MockRemoteControlLauncher, FakeServerProbe) {
    let calls = CallLog::default();
    let host = Arc::new(Mutex::new(None));
    let relay = Arc::new(Mutex::new(None));
    let (status_tx, _status_rx) = watch::channel(RemoteServerStatus::default());
    let server = FakeServer {
        calls: Arc::clone(&calls),
        codes: Mutex::new(codes.into_iter().rev().collect()),
        revoke_result,
        status_tx: status_tx.clone(),
    };
    let captured_host = Arc::clone(&host);
    let captured_relay = Arc::clone(&relay);
    let mut launcher = MockRemoteControlLauncher::new();
    launcher
        .expect_start()
        .times(1)
        .return_once(move |relay, host| {
            *captured_host.lock().expect("host lock") = Some(host);
            *captured_relay.lock().expect("relay lock") = Some(relay);

            Box::new(server) as Box<dyn RemoteControlServer>
        });

    (
        launcher,
        FakeServerProbe {
            calls,
            host,
            relay,
            status_tx,
        },
    )
}

/// Builds remote control over `launcher` with link actions that do nothing.
fn remote_control(app: &App, launcher: MockRemoteControlLauncher) -> RemoteControl {
    remote_control_with_links(
        app,
        launcher,
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    )
}

/// Builds remote control over `launcher`, `browser`, and `clipboard`.
fn remote_control_with_links(
    app: &App,
    launcher: MockRemoteControlLauncher,
    browser: MockBrowserClient,
    clipboard: MockTerminalClipboard,
) -> RemoteControl {
    RemoteControl::new(
        Arc::new(launcher),
        Arc::new(browser),
        Arc::new(clipboard),
        app.services.db(),
    )
}

/// Starts a fake server for `app` with one connected device, so the
/// foreground cycle checks what the session API reports, and returns the
/// revisions the server follows with the current one marked seen.
/// Replaces any earlier remote control, so earlier revision subscriptions
/// no longer follow it.
pub(crate) async fn start_fake_remote_control(app: &mut App) -> watch::Receiver<u64> {
    let (mut revisions, probe) = start_server_without_devices(app).await;
    probe.status_tx.send_replace(connected("phone"));
    apply_events_until_device_connects(app).await;
    revisions.borrow_and_update();

    revisions
}

/// Applies background app events until the server reports a connection,
/// whether or not the device is paired in the app database. The same step
/// marks a paired device connected in the Platform tab.
async fn apply_events_until_device_connects(app: &mut App) {
    apply_events_until(app, |app| !app.remote_control.connections.is_empty()).await;
}

/// Starts a fake server for `app` that no device has connected to, and
/// returns the revisions it follows, with the current one marked seen, and
/// the server's probe.
async fn start_server_without_devices(app: &mut App) -> (watch::Receiver<u64>, FakeServerProbe) {
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(app, launcher);
    app.start_remote_control().await;
    let mut revisions = probe
        .host
        .lock()
        .expect("host lock")
        .take()
        .expect("server should receive a host")
        .revisions;
    revisions.borrow_and_update();

    (revisions, probe)
}

/// Replaces the app's remote control with `launcher` over the app
/// database, with the platform's relay available.
fn install_launcher(app: &mut App, launcher: MockRemoteControlLauncher) {
    install_launcher_with_links(
        app,
        launcher,
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    );
}

/// Replaces the app's remote control with one over `launcher`, `browser`,
/// and `clipboard`, with the platform's relay available.
fn install_launcher_with_links(
    app: &mut App,
    launcher: MockRemoteControlLauncher,
    browser: MockBrowserClient,
    clipboard: MockTerminalClipboard,
) {
    app.remote_control = remote_control_with_links(app, launcher, browser, clipboard);
    app.remote_control.platform_relay = Some(platform_relay());
}

/// Returns the relay a platform offers.
fn platform_relay() -> RemoteControlService {
    RemoteControlService {
        pairing_url: "https://platform.example/sessions/".to_string(),
        relay_url: "wss://relay.platform.example".to_string(),
    }
}

/// Returns the pairing link the view shows, while remote control runs.
fn shown_link(app: &App) -> Option<String> {
    app.remote_control
        .view()
        .relay
        .as_ref()
        .map(|relay| relay.link.clone())
}

/// Stores one paired device directly in the app database.
async fn pair_device(app: &App, id: &str, name: &str) {
    app.services
        .db()
        .remote_devices()
        .insert_device(id, name, &format!("hash-{id}"))
        .await
        .expect("device should be stored");
}

/// Applies background app events until a link copy shows its result.
async fn apply_events_until_link_notice(app: &mut App) {
    apply_events_until(app, |app| app.remote_control.view().notice.is_some()).await;
}

/// Applies background app events until `is_done` holds.
///
/// Startup work may queue unrelated events ahead of the awaited one, and
/// one batch may apply several events, so this waits on the resulting state.
async fn apply_events_until(app: &mut App, is_done: fn(&App) -> bool) {
    let wait = async {
        while !is_done(app) {
            match app.next_runtime_event().await {
                AppRuntimeEvent::App(event) => app.apply_app_events(*event).await,
                AppRuntimeEvent::Session(command) => {
                    app.apply_session_runtime_command(*command).await;
                }
            }
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(5), wait)
        .await
        .expect("background events should reach the awaited state");
}

/// Builds the status a listener reports for one connected device.
fn connected(device_id: &str) -> RemoteServerStatus {
    RemoteServerStatus {
        connections: vec![RemoteConnection {
            device_id: device_id.to_string(),
            device_name: device_id.to_string(),
        }],
        pairing_requests: Vec::new(),
        relay: RelayStatus::Connecting,
    }
}

/// Builds the status a listener reports while devices wait to pair.
fn pairing(request_ids: &[u64]) -> RemoteServerStatus {
    RemoteServerStatus {
        connections: Vec::new(),
        pairing_requests: request_ids
            .iter()
            .map(|id| PairingRequest {
                confirmation: format!("C{id}"),
                device_name: format!("Device {id}"),
                id: *id,
            })
            .collect(),
        relay: RelayStatus::Connecting,
    }
}

#[tokio::test]
async fn start_remote_control_links_the_platform_relay_and_shows_a_pairing_link() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(vec![Some("ABCD-EFGH".to_string())], Ok(()));
    install_launcher(&mut app, launcher);

    // Act
    app.start_remote_control().await;

    // Assert
    let relay = probe
        .relay
        .lock()
        .expect("relay lock")
        .clone()
        .expect("server should get the platform relay");
    assert_eq!(relay.url, platform_relay().relay_url);
    let view = app.remote_control.view();
    let shown = view.relay.as_ref().expect("relay should be shown");
    assert_eq!(
        shown.link,
        pairing_link_on_page(
            &platform_relay().pairing_url,
            &platform_relay().relay_url,
            relay.instance_key.public(),
            "ABCD-EFGH",
        )
    );
    assert_eq!(shown.status, RelayStatus::Connecting);
    assert_eq!(view.error, None);
    assert_eq!(view.devices, []);
}

#[tokio::test]
async fn reopening_remote_control_keeps_the_server_and_replaces_the_code() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(vec![Some("FIRST-CODE".to_string())], Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;

    // Act
    app.start_remote_control().await;

    // Assert
    assert!(shown_link(&app).is_some_and(|link| link.ends_with("&code=LAST%2DCODE")));
}

#[tokio::test]
async fn start_remote_control_without_the_platform_relay_starts_nothing() {
    // Arrange: a launcher that expects no start.
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.remote_control = remote_control(&app, MockRemoteControlLauncher::new());

    // Act
    app.start_remote_control().await;

    // Assert
    assert_eq!(app.remote_control.view().relay, None);
    assert_eq!(app.remote_control.view().error, None);
}

#[tokio::test]
async fn pending_pairings_are_shown_then_approved_or_denied_oldest_first() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let generation = app.remote_control.generation;
    app.apply_remote_control_status(generation, pairing(&[1, 2]))
        .await;
    let shown = app.remote_control.view().pairing_requests.clone();

    // Act
    app.settle_remote_pairing(true);
    app.settle_remote_pairing(false);
    app.settle_remote_pairing(true);

    // Assert
    assert_eq!(
        shown,
        vec![
            RemotePairingRequestView {
                confirmation: "C1".to_string(),
                device_name: "Device 1".to_string(),
                id: 1,
            },
            RemotePairingRequestView {
                confirmation: "C2".to_string(),
                device_name: "Device 2".to_string(),
                id: 2,
            },
        ]
    );
    assert_eq!(app.remote_control.view().pairing_requests, []);
    assert_eq!(app.remote_control.view().error, None);
    assert_eq!(
        *probe.calls.lock().expect("calls lock"),
        ["approve:1", "deny:2"]
    );
}

#[tokio::test]
async fn settling_a_pairing_that_already_ended_reports_it() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.settle_remote_pairing(true);
    let (launcher, _probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let generation = app.remote_control.generation;
    app.apply_remote_control_status(generation, pairing(&[GONE_REQUEST_ID]))
        .await;

    // Act
    app.settle_remote_pairing(true);

    // Assert
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("That pairing request is no longer pending.")
    );
    assert_eq!(app.remote_control.view().pairing_requests, []);
}

#[tokio::test]
async fn stopping_clears_pending_pairings() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let generation = app.remote_control.generation;
    app.apply_remote_control_status(generation, pairing(&[1]))
        .await;

    // Act
    app.stop_remote_control().await;

    // Assert
    assert_eq!(app.remote_control.view().pairing_requests, []);
}

#[tokio::test]
async fn pairing_code_failure_is_reported_and_code_is_ignored_while_off() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.issue_remote_pairing_code();
    let view_while_off = app.remote_control.view().clone();
    let (launcher, _probe) = fake_launcher(vec![None], Ok(()));
    install_launcher(&mut app, launcher);

    // Act: the relay link's status arrives while no link is shown.
    app.start_remote_control().await;
    let generation = app.remote_control.generation;
    let status = RemoteServerStatus {
        relay: RelayStatus::Connected,
        ..RemoteServerStatus::default()
    };
    app.apply_remote_control_status(generation, status).await;

    // Assert
    assert_eq!(view_while_off.relay, None);
    assert_eq!(app.remote_control.view().relay, None);
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("Cannot generate a secure pairing code")
    );
}

#[tokio::test]
async fn server_status_marks_connected_devices() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    pair_device(&app, "tablet", "Tablet").await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;

    // Act
    probe.status_tx.send_replace(connected("tablet"));
    apply_events_until_device_connects(&mut app).await;

    // Assert
    assert_eq!(
        app.remote_control.view().devices,
        vec![
            RemoteDeviceView {
                id: "phone".to_string(),
                is_connected: false,
                name: "Phone".to_string(),
            },
            RemoteDeviceView {
                id: "tablet".to_string(),
                is_connected: true,
                name: "Tablet".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn a_device_connected_before_forwarding_starts_reaches_the_host() {
    // Arrange: a paired device reconnects as soon as the listener starts,
    // before the host subscribes to its status.
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    probe.status_tx.send_replace(connected("phone"));
    install_launcher(&mut app, launcher);

    // Act
    app.start_remote_control().await;
    apply_events_until_device_connects(&mut app).await;

    // Assert
    assert_eq!(app.remote_control.view().connected_device_count(), 1);
    assert!(app.remote_control.reported_state.is_some());
}

#[tokio::test]
async fn status_from_a_stopped_server_is_ignored() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    let (launcher, _probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let stale_generation = 1;
    app.stop_remote_control().await;

    // Act
    app.apply_remote_control_status(stale_generation, connected("phone"))
        .await;

    // Assert
    assert!(
        app.remote_control
            .view()
            .devices
            .iter()
            .all(|device| !device.is_connected)
    );
}

#[tokio::test]
async fn stop_remote_control_shuts_down_and_disconnects_devices() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    probe.status_tx.send_replace(connected("phone"));
    apply_events_until_device_connects(&mut app).await;

    // Act
    app.stop_remote_control().await;
    app.stop_remote_control().await;

    // Assert
    let view = app.remote_control.view();
    assert_eq!(view.relay, None);
    assert_eq!(view.connected_device_count(), 0);
    assert_eq!(view.devices.len(), 1);
    assert_eq!(*probe.calls.lock().expect("calls lock"), ["shutdown"]);
}

#[tokio::test]
async fn status_forwarding_ends_once_the_server_is_gone() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    app.stop_remote_control().await;

    // Act
    drop(probe.status_tx);
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    app.process_pending_app_events().await;

    // Assert
    assert_eq!(app.remote_control.view().relay, None);
    assert_eq!(app.remote_control.view().connected_device_count(), 0);
}

#[tokio::test]
async fn revoke_remote_device_uses_the_running_server() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(
        Vec::new(),
        Err("Could not remove the device: gone".to_string()),
    );
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;

    // Act
    app.revoke_remote_device("phone").await;

    // Assert
    assert_eq!(*probe.calls.lock().expect("calls lock"), ["revoke:phone"]);
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("Could not remove the device: gone")
    );
}

#[tokio::test]
async fn revoke_remote_device_deletes_the_stored_device_while_off() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;

    // Act
    app.revoke_remote_device("phone").await;

    // Assert
    assert_eq!(app.remote_control.view().devices, []);
    assert_eq!(app.remote_control.view().error, None);
}

/// Device store whose every operation fails.
struct FailingDeviceStore;

#[async_trait]
impl DeviceStore for FailingDeviceStore {
    async fn list_devices(&self) -> Result<Vec<RemoteDevice>, DeviceStoreError> {
        Err(DeviceStoreError("disk full".to_string()))
    }

    async fn find_device_by_token_hash(
        &self,
        _token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError> {
        Err(DeviceStoreError("disk full".to_string()))
    }

    async fn insert_device(&self, _device: NewRemoteDevice) -> Result<(), DeviceStoreError> {
        Err(DeviceStoreError("disk full".to_string()))
    }

    async fn touch_device(&self, _device_id: &str) -> Result<(), DeviceStoreError> {
        Err(DeviceStoreError("disk full".to_string()))
    }

    async fn delete_device(&self, _device_id: &str) -> Result<(), DeviceStoreError> {
        Err(DeviceStoreError("disk full".to_string()))
    }
}

#[tokio::test]
async fn device_store_failures_are_reported() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.remote_control.devices = Arc::new(FailingDeviceStore);

    // Act
    app.revoke_remote_device("phone").await;

    // Assert
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("Could not load paired devices: disk full")
    );
}

#[tokio::test]
async fn server_sessions_are_limited_to_the_remote_scope() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let project_id = app.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session(
            "hidden",
            ag_session::test_support::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("session should persist");
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let sessions = probe
        .host
        .lock()
        .expect("host lock")
        .take()
        .expect("listener should receive a host")
        .sessions;

    // Act
    let proposal = app
        .drive_session_request(sessions.get_graph_proposal("proposal"))
        .await;
    let unlisted = app
        .drive_session_request(sessions.get_session(&"hidden".into()))
        .await;

    // Assert
    // Other callers read the stored session and get no proposal.
    assert_eq!(
        proposal,
        Err(ag_session::SessionError::Operation(
            "Remote control does not serve this operation".to_string()
        ))
    );
    assert_eq!(unlisted, Ok(None));
}

#[tokio::test]
async fn publish_session_change_advances_the_remote_revision() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let mut revisions = probe
        .host
        .lock()
        .expect("host lock")
        .take()
        .expect("listener should receive a host")
        .revisions;
    let initial_revision = *revisions.borrow_and_update();

    // Act
    app.publish_session_change();

    // Assert
    assert!(
        revisions
            .has_changed()
            .expect("revision sender should live")
    );
    assert_eq!(*revisions.borrow(), initial_revision + 1);
}

/// One change to state the session API reports from host memory.
#[derive(Clone, Copy, Debug)]
enum ReportedChange {
    DiffStats,
    GitStatus,
    HarnessModels,
    Model,
    ProjectSyncPhase,
    ProposalReplyDecision,
    QueuedAction,
    QueuedMessage,
    ReasoningLevel,
    ResponseStyle,
    ReviewRequestSummary,
    SessionGitStatus,
    ShownFocusedReview,
    SpeedMode,
    Status,
    StatusLine,
    Title,
}

impl ReportedChange {
    /// Every change, in the order the table test applies them.
    const ALL: [Self; 17] = [
        Self::DiffStats,
        Self::GitStatus,
        Self::HarnessModels,
        Self::Model,
        Self::ProjectSyncPhase,
        Self::ProposalReplyDecision,
        Self::QueuedAction,
        Self::QueuedMessage,
        Self::ReasoningLevel,
        Self::ResponseStyle,
        Self::ReviewRequestSummary,
        Self::SessionGitStatus,
        Self::ShownFocusedReview,
        Self::SpeedMode,
        Self::Status,
        Self::StatusLine,
        Self::Title,
    ];

    /// Applies the change to `app`, which loads the running `session-1`
    /// and the reviewed `session-2`, through the reducer event that makes
    /// it where one exists.
    async fn apply(self, app: &mut App) {
        if let Some(event) = self.app_event(app) {
            app.apply_app_events(event).await;

            return;
        }

        match self {
            Self::ProjectSyncPhase => {
                app.project_sync_statuses.insert(
                    app.active_project_id(),
                    ProjectSyncStatus {
                        context: ProjectSyncContext {
                            default_branch: "main".to_string(),
                            operation_id: 1,
                            project_id: app.active_project_id(),
                            project_name: "project".to_string(),
                        },
                        phase: ProjectSyncPhase::Running,
                    },
                );
            }
            Self::ProposalReplyDecision => {
                app.services
                    .proposal_reply_decisions
                    .lock()
                    .expect("decisions lock")
                    .insert(
                        "proposal".to_string(),
                        ReplyDecision {
                            cancellation: CancellationToken::new(),
                            is_abandoned: Arc::new(AtomicBool::new(false)),
                            session_id: "session-1".into(),
                        },
                    );
            }
            Self::QueuedAction => app.sessions.queue_session_sync("session-2", 1),
            Self::QueuedMessage => session_mut(app, "session-1")
                .queued_messages
                .push(QueuedMessage::new(1, "next".into())),
            Self::ReviewRequestSummary => {
                session_mut(app, "session-2").review_request = Some(review_request());
            }
            Self::ShownFocusedReview => {
                app.set_review_ready_output("session-2", 42, "review".to_string());
            }
            Self::Status => {
                test_support::set_session_status_for_test(app, "session-2", Status::Done);
            }
            Self::StatusLine => {
                app.session_progress_messages
                    .insert("session-1".into(), "Reading files".to_string());
            }
            Self::Title => session_mut(app, "session-1").title = Some("Renamed".to_string()),
            Self::DiffStats
            | Self::GitStatus
            | Self::HarnessModels
            | Self::Model
            | Self::ReasoningLevel
            | Self::ResponseStyle
            | Self::SessionGitStatus
            | Self::SpeedMode => unreachable!("{self:?} applies through an app event"),
        }
    }

    /// Returns the reducer event that makes the change, when one does.
    fn app_event(self, app: &App) -> Option<AppEvent> {
        let generation = app.sync_handle.current_generation();
        let project_id = app.active_project_id();
        let session_id = SessionId::from("session-1");
        let event = match self {
            Self::DiffStats => AppEvent::SessionDiffStatsUpdated {
                diff_stats: SessionDiffStats::Known {
                    added_lines: 12,
                    deleted_lines: 4,
                    has_diff: true,
                    session_size: SessionSize::Small,
                },
                session_id,
            },
            Self::GitStatus => AppEvent::GitStatusUpdated {
                generation,
                project_id,
                session_statuses: HashMap::new(),
                status: Some((0, 2)),
            },
            Self::HarnessModels => AppEvent::HarnessModelsUpdated {
                options: vec![test_support::ready_harness_model(
                    AgentKind::Harness.default_model(),
                )],
            },
            Self::Model => AppEvent::SessionModelUpdated {
                session_agent: AgentSelection::new(
                    AgentKind::Antigravity,
                    AgentKind::Antigravity.default_model(),
                ),
                session_id,
            },
            Self::ReasoningLevel => AppEvent::SessionReasoningLevelUpdated {
                reasoning_level: ReasoningLevel::Low,
                session_id,
            },
            Self::ResponseStyle => AppEvent::SessionResponseStyleUpdated {
                response_style: ResponseStyle::Detailed,
                session_id,
            },
            Self::SessionGitStatus => AppEvent::GitStatusUpdated {
                generation,
                project_id,
                session_statuses: HashMap::from([(
                    SessionId::from("session-2"),
                    SessionGitStatus {
                        has_merge_conflict: Some(true),
                        ..SessionGitStatus::default()
                    },
                )]),
                status: None,
            },
            Self::SpeedMode => AppEvent::SessionSpeedModeUpdated {
                session_id,
                speed_mode: SpeedMode::Fast,
            },
            Self::ProjectSyncPhase
            | Self::ProposalReplyDecision
            | Self::QueuedAction
            | Self::QueuedMessage
            | Self::ReviewRequestSummary
            | Self::ShownFocusedReview
            | Self::Status
            | Self::StatusLine
            | Self::Title => return None,
        };

        Some(event)
    }
}

/// Returns the loaded session `session_id` of `app`.
fn session_mut<'app>(app: &'app mut App, session_id: &str) -> &'app mut Session {
    app.sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should be loaded")
}

/// Builds the linked review request of a reviewed session.
fn review_request() -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 1,
        summary: ReviewRequestSummary {
            display_id: "#5".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-2".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    }
}

/// Builds an app that loads the running `session-1` and the reviewed
/// `session-2`, with a listener running, and returns the revisions the
/// listener follows.
async fn connected_app_with_sessions() -> (App, tempfile::TempDir, watch::Receiver<u64>) {
    let (mut app, base_dir) = Box::pin(test_support::new_test_app()).await;
    app.sessions.push_session(test_support::session_fixture(
        "session-1",
        Status::InProgress,
    ));
    app.sessions
        .push_session(test_support::session_fixture("session-2", Status::Review));
    let revisions = Box::pin(start_fake_remote_control(&mut app)).await;

    (app, base_dir, revisions)
}

#[tokio::test]
async fn each_reported_change_advances_the_remote_revision_once_after_the_cycle_check() {
    for change in ReportedChange::ALL {
        // Arrange
        let (mut app, _base_dir, mut revisions) = connected_app_with_sessions().await;
        let initial_revision = *revisions.borrow_and_update();

        // Act
        change.apply(&mut app).await;
        app.publish_reported_state_changes();
        let after_change = *revisions.borrow_and_update();
        app.publish_reported_state_changes();
        let is_changed_again = revisions
            .has_changed()
            .expect("revision sender should live");

        // Assert
        assert_eq!(after_change, initial_revision + 1, "{change:?}");
        assert!(!is_changed_again, "{change:?}");
    }
}

#[tokio::test]
async fn reported_changes_leave_the_remote_revision_alone_while_no_device_is_connected() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.sessions.push_session(test_support::session_fixture(
        "session-1",
        Status::InProgress,
    ));
    let (mut revisions, _probe) = start_server_without_devices(&mut app).await;
    let initial_revision = *revisions.borrow_and_update();

    // Act
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_reported_state_changes();

    // Assert
    assert_eq!(*revisions.borrow(), initial_revision);
    assert!(app.remote_control.reported_state.is_none());
}

#[tokio::test]
async fn the_first_connection_tells_a_device_that_already_read_to_read_again() {
    // Arrange: the device authenticates and reads the current revision
    // before the host hears of its connection.
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.sessions.push_session(test_support::session_fixture(
        "session-1",
        Status::InProgress,
    ));
    let (mut revisions, probe) = start_server_without_devices(&mut app).await;
    let read_revision = *revisions.borrow_and_update();
    let generation = app.remote_control.generation;

    // Act: the state changes before the connection arrives, the first
    // device connects, a second one joins, and later checks find nothing
    // new.
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_reported_state_changes();
    let before_connection = *revisions.borrow_and_update();
    probe.status_tx.send_replace(connected("phone"));
    apply_events_until_device_connects(&mut app).await;
    let after_connection = *revisions.borrow_and_update();
    app.publish_reported_state_changes();
    let mut both_connected = connected("phone");
    both_connected
        .connections
        .extend(connected("tablet").connections);
    app.apply_remote_control_status(generation, both_connected)
        .await;
    app.publish_reported_state_changes();
    let after_second_device = *revisions.borrow_and_update();

    // Assert
    assert_eq!(before_connection, read_revision);
    assert_eq!(after_connection, read_revision + 1);
    assert_eq!(after_second_device, after_connection);
}

#[tokio::test]
async fn the_check_stops_when_the_last_device_leaves_and_restarts_on_reconnection() {
    // Arrange
    let (mut app, _base_dir, mut revisions) = connected_app_with_sessions().await;
    let generation = app.remote_control.generation;
    let initial_revision = *revisions.borrow_and_update();

    // Act
    app.apply_remote_control_status(generation, RemoteServerStatus::default())
        .await;
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_reported_state_changes();
    let while_disconnected = *revisions.borrow_and_update();
    let is_following_while_disconnected = app.remote_control.reported_state.is_some();
    app.apply_remote_control_status(generation, connected("phone"))
        .await;
    let after_reconnection = *revisions.borrow_and_update();
    app.publish_reported_state_changes();
    let is_changed_after_reconnection = revisions
        .has_changed()
        .expect("revision sender should live");

    // Assert
    assert_eq!(while_disconnected, initial_revision);
    assert!(!is_following_while_disconnected);
    assert_eq!(after_reconnection, initial_revision + 1);
    assert!(!is_changed_after_reconnection);
}

#[tokio::test]
async fn reported_changes_leave_the_remote_revision_alone_while_remote_control_is_off() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.sessions.push_session(test_support::session_fixture(
        "session-1",
        Status::InProgress,
    ));
    let mut revisions = app.remote_control.subscribe_revisions();
    let initial_revision = *revisions.borrow_and_update();

    // Act
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_reported_state_changes();

    // Assert
    assert_eq!(*revisions.borrow(), initial_revision);
    assert!(app.remote_control.reported_state.is_none());
}

#[tokio::test]
async fn viewed_page_and_project_list_changes_advance_the_remote_revision_once_each() {
    // Arrange
    let (mut app, base_dir, mut revisions) = connected_app_with_sessions().await;
    let initial_revision = *revisions.borrow_and_update();
    // Each step ends with the check the foreground cycle makes.
    let mut revision_after = |app: &mut App| {
        app.publish_reported_state_changes();

        *revisions.borrow_and_update()
    };

    // Act
    let other_project_id = add_listed_project(&mut app, &base_dir, "other").await;
    let after_listing = revision_after(&mut app);
    insert_loadable_session(&app, "viewed1", Status::Review, other_project_id).await;
    app.ensure_project_page(other_project_id)
        .await
        .expect("listed page should load");
    let after_loading = revision_after(&mut app);
    app.sessions
        .state_mut()
        .session_mut_for_id("viewed1")
        .expect("viewed session should be loaded")
        .title = Some("Renamed".to_string());
    let after_viewed_change = revision_after(&mut app);
    let after_no_change = revision_after(&mut app);

    // Assert
    assert_eq!(after_listing, initial_revision + 1);
    assert_eq!(after_loading, after_listing + 1);
    assert_eq!(after_viewed_change, after_loading + 1);
    assert_eq!(after_no_change, after_viewed_change);
}

#[tokio::test]
async fn the_last_device_leaving_drops_the_viewed_pages() {
    // Arrange
    let (mut app, base_dir, _revisions) = connected_app_with_sessions().await;
    let generation = app.remote_control.generation;
    let other_project_id = add_listed_project(&mut app, &base_dir, "other").await;
    // A device's request loads the page and hands it to the background
    // poll, as a session-API caller's does.
    let remote = app.remote_session_service();
    let listed = app
        .drive_session_request(
            remote.project_sessions(ag_session::ProjectScope::Project(other_project_id)),
        )
        .await
        .map(|listing| listing.project_id);
    let polled_while_viewed = app.sync_handle.generation_for(other_project_id);
    let mut both_connected = connected("phone");
    both_connected
        .connections
        .extend(connected("tablet").connections);
    app.apply_remote_control_status(generation, both_connected)
        .await;

    // Act
    app.apply_remote_control_status(generation, connected("tablet"))
        .await;
    let while_one_connected = app.sessions.state().viewed_project_ids();
    app.apply_remote_control_status(generation, RemoteServerStatus::default())
        .await;

    // Assert
    assert_eq!(listed, Ok(Some(other_project_id)));
    assert!(polled_while_viewed.is_some());
    assert_eq!(while_one_connected, vec![other_project_id]);
    assert_eq!(app.sessions.state().viewed_project_ids(), Vec::<i64>::new());
    assert!(app.projects.context(other_project_id).is_none());
    assert_eq!(app.sync_handle.generation_for(other_project_id), None);
    assert_eq!(app.sessions.sessions().len(), 2);
}

#[tokio::test]
async fn stopping_remote_control_stops_the_reported_state_check() {
    // Arrange
    let (mut app, _base_dir, mut revisions) = connected_app_with_sessions().await;
    app.stop_remote_control().await;
    let stopped_revision = *revisions.borrow_and_update();

    // Act
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_reported_state_changes();

    // Assert
    assert_eq!(*revisions.borrow(), stopped_revision);
    assert!(app.remote_control.reported_state.is_none());
}

#[tokio::test]
async fn status_line_and_queued_action_changes_advance_the_remote_revision_once_each() {
    // Arrange
    let (mut app, _base_dir, mut revisions) = connected_app_with_sessions().await;
    let initial_revision = *revisions.borrow_and_update();

    // Act
    // The listener started with the running session's `Working...` line.
    app.publish_reported_state_changes();
    let after_first_check = *revisions.borrow_and_update();
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_reported_state_changes();
    let after_progress = *revisions.borrow_and_update();
    app.sessions.queue_session_sync("session-2", 1);
    app.publish_reported_state_changes();
    let after_queued_action = *revisions.borrow_and_update();
    app.publish_reported_state_changes();
    let unchanged = revisions
        .has_changed()
        .expect("revision sender should live");

    // Assert
    assert_eq!(after_first_check, initial_revision);
    assert_eq!(after_progress, initial_revision + 1);
    assert_eq!(after_queued_action, initial_revision + 2);
    assert!(!unchanged);
}

#[tokio::test]
async fn graph_actions_admitted_in_the_terminal_advance_the_remote_revision() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let mut view = crate::presentation::execution_graph::tests::graph_view();
    view.graphs[0].nodes.truncate(1);
    view.progress.as_mut().expect("progress").nodes.truncate(1);
    let progress = view.progress.clone();
    let mut repository = ag_store::MockGraphRepository::new();
    repository
        .expect_progress()
        .returning(move |_| Ok(progress.clone()));
    repository.expect_cancel().once().returning(|_| Ok(()));
    app.services.set_graph_repository(Arc::new(repository));
    let mut revisions = app.remote_control.subscribe_revisions();
    let initial_revision = *revisions.borrow_and_update();

    // Act
    let cancelled = app
        .apply_graph_view_action(&view, crate::app::graph_view::GraphViewAction::Cancel)
        .await;

    // Assert
    // Cancelling writes the graph without a session event; the revision
    // still tells devices to refetch it.
    assert_eq!(cancelled, Ok(()));
    assert_eq!(*revisions.borrow_and_update(), initial_revision + 1);
}

#[tokio::test]
async fn graph_actions_admitted_from_devices_advance_the_remote_revision() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    // Devices reach only the sessions the host lists.
    app.sessions
        .push_session(test_support::session_fixture("sess", Status::Review));
    let mut view = crate::presentation::execution_graph::tests::graph_view();
    view.graphs[0].nodes.truncate(1);
    view.progress.as_mut().expect("progress").nodes.truncate(1);
    let graph = view.graphs[0].clone();
    let progress = view.progress.clone();
    let mut repository = ag_store::MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .returning(move |_| Ok(vec![graph.clone()]));
    repository
        .expect_progress()
        .returning(move |_| Ok(progress.clone()));
    repository.expect_load_result().returning(|_| Ok(None));
    repository.expect_cancel().once().returning(|_| Ok(()));
    app.services.set_graph_repository(Arc::new(repository));
    let snapshot = app
        .api_session_graphs(ag_session::SessionGraphsRequest {
            graph_id: Some("graph".into()),
            node_id: None,
            session_id: "sess".into(),
        })
        .await
        .expect("graphs should load")
        .graph
        .expect("selected graph")
        .snapshot;
    let mut revisions = app.remote_control.subscribe_revisions();
    let initial_revision = *revisions.borrow_and_update();
    let command = SessionCommand::new(
        CommandInitiator::Remote,
        CommandDelivery::Admission {
            request: SessionMutation::ActOnSessionGraph {
                request: Box::new(ag_session::SessionGraphActionRequest {
                    action: ag_session::SessionGraphAction::Cancel,
                    graph_id: "graph".into(),
                    session_id: "sess".into(),
                    snapshot,
                }),
            },
            response_tx: None,
        },
    );

    // Act
    let cancelled = app.admit_session_command(command).await;

    // Assert
    assert_eq!(cancelled, Ok(()));
    assert_eq!(*revisions.borrow_and_update(), initial_revision + 1);
}

#[tokio::test]
async fn shown_focused_review_changes_advance_the_remote_revision_once_each() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.sessions
        .push_session(test_support::session_fixture("session-1", Status::Review));
    let mut revisions = start_fake_remote_control(&mut app).await;
    let initial_revision = *revisions.borrow_and_update();

    // Act
    app.set_review_ready_output("session-1", 42, "first review".to_string());
    app.publish_reported_state_changes();
    let after_ready = *revisions.borrow_and_update();
    app.publish_reported_state_changes();
    let unchanged = revisions
        .has_changed()
        .expect("revision sender should live");
    app.set_review_ready_output("session-1", 42, "regenerated review".to_string());
    app.publish_reported_state_changes();
    let after_regenerated = *revisions.borrow_and_update();
    app.clear_review_output("session-1");
    app.publish_reported_state_changes();
    let after_clear = *revisions.borrow_and_update();
    app.suppress_review_output("session-1");
    app.publish_reported_state_changes();
    let after_suppress = *revisions.borrow_and_update();
    app.set_review_ready_output("session-1", 43, "next review".to_string());
    app.publish_reported_state_changes();
    let after_next = *revisions.borrow_and_update();
    // Leaving review hides the review; the status change itself shows in
    // the list entry.
    test_support::set_session_status_for_test(&mut app, "session-1", Status::Done);
    app.publish_reported_state_changes();
    let after_done = *revisions.borrow_and_update();

    // Assert
    assert_eq!(after_ready, initial_revision + 1);
    assert!(!unchanged);
    assert_eq!(after_regenerated, initial_revision + 2);
    assert_eq!(after_clear, initial_revision + 3);
    // Suppressing an already cleared review shows nothing new.
    assert_eq!(after_suppress, after_clear);
    assert_eq!(after_next, initial_revision + 4);
    // A session that leaves review stops showing its review.
    assert_eq!(after_done, initial_revision + 5);
}

#[tokio::test]
async fn relay_that_does_not_admit_this_instance_stays_shown_across_new_codes() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);
    app.start_remote_control().await;
    let generation = app.remote_control.generation;

    // Act
    let status = RemoteServerStatus {
        relay: RelayStatus::NotAdmitted,
        ..RemoteServerStatus::default()
    };
    app.apply_remote_control_status(generation, status).await;
    app.issue_remote_pairing_code();

    // Assert
    let relay_status = app
        .remote_control
        .view()
        .relay
        .as_ref()
        .map(|relay| relay.status);
    assert_eq!(relay_status, Some(RelayStatus::NotAdmitted));
}

#[tokio::test]
async fn the_server_gets_a_saved_key_and_shows_its_relay_state_and_qr_code() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);

    // Act
    app.start_remote_control().await;
    let generation = app.remote_control.generation;
    let status = RemoteServerStatus {
        relay: RelayStatus::Connected,
        ..RemoteServerStatus::default()
    };
    app.apply_remote_control_status(generation, status).await;
    app.issue_remote_pairing_code();

    // Assert
    let relay = probe
        .relay
        .lock()
        .expect("relay lock")
        .clone()
        .expect("server should get a relay");
    let stored = app
        .services
        .db()
        .settings()
        .get_setting(SettingName::RemoteInstanceKey)
        .await
        .expect("setting should load")
        .expect("key should be saved");
    assert!(stored.ends_with(&relay.instance_key.public().to_hex()));
    let relay_view = app
        .remote_control
        .view()
        .relay
        .clone()
        .expect("relay should be shown");
    assert_eq!(relay_view.status, RelayStatus::Connected);
    assert_eq!(
        relay_view.link,
        pairing_link_on_page(
            &platform_relay().pairing_url,
            &platform_relay().relay_url,
            relay.instance_key.public(),
            "LAST-CODE",
        )
    );
    assert_ne!(relay_view.qr_code, Vec::<String>::new());
}

#[tokio::test]
async fn the_server_reuses_the_saved_key() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let saved = StaticKeypair::generate().expect("key should generate");
    app.services
        .db()
        .settings()
        .upsert_setting(
            SettingName::RemoteInstanceKey,
            &format!(
                "{}:{}",
                hex::encode(saved.private_bytes()),
                saved.public().to_hex()
            ),
        )
        .await
        .expect("setting should save");
    let (launcher, probe) = fake_launcher(Vec::new(), Ok(()));
    install_launcher(&mut app, launcher);

    // Act
    app.start_remote_control().await;

    // Assert
    let relay = probe.relay.lock().expect("relay lock").clone();
    assert_eq!(relay.map(|relay| relay.instance_key), Some(saved));
}

#[tokio::test]
async fn invalid_saved_key_stops_the_server_from_starting() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.services
        .db()
        .settings()
        .upsert_setting(SettingName::RemoteInstanceKey, "not a key")
        .await
        .expect("setting should save");
    install_launcher(&mut app, MockRemoteControlLauncher::new());

    // Act
    app.start_remote_control().await;

    // Assert
    assert_eq!(app.remote_control.view().relay, None);
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("The stored instance key is invalid.")
    );
}

/// Verifies the link is opened in the browser and copied through `tmux`,
/// through the terminal when `tmux` does not take it, and reported as not
/// copied when neither does.
#[tokio::test]
async fn link_actions_open_and_copy_the_relay_link() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(vec![Some("CODE-0001".to_string())], Ok(()));
    let mut browser = MockBrowserClient::new();
    browser
        .expect_open()
        .withf(|link| link.contains("code=CODE%2D0001"))
        .times(1)
        .return_const(());
    let mut clipboard = MockTerminalClipboard::new();
    let mut tmux_order = mockall::Sequence::new();
    clipboard
        .expect_copy_through_tmux()
        .times(1)
        .in_sequence(&mut tmux_order)
        .return_const(true);
    clipboard
        .expect_copy_through_tmux()
        .times(2)
        .in_sequence(&mut tmux_order)
        .return_const(false);
    let mut terminal_order = mockall::Sequence::new();
    clipboard
        .expect_copy_through_terminal()
        .withf(|link| link.contains("code=CODE%2D0001"))
        .times(1)
        .in_sequence(&mut terminal_order)
        .return_const(true);
    clipboard
        .expect_copy_through_terminal()
        .times(1)
        .in_sequence(&mut terminal_order)
        .return_const(false);
    install_launcher_with_links(&mut app, launcher, browser, clipboard);
    app.start_remote_control().await;

    // Act
    app.open_remote_link();
    app.copy_remote_link();
    let pending = app.remote_control.view().notice.clone();
    apply_events_until_link_notice(&mut app).await;
    let copied_through_tmux = app.remote_control.view().notice.clone();
    app.copy_remote_link();
    apply_events_until_link_notice(&mut app).await;
    let copied_through_terminal = app.remote_control.view().notice.clone();
    app.copy_remote_link();
    apply_events_until_link_notice(&mut app).await;
    let not_copied = app.remote_control.view().notice.clone();
    app.stop_remote_control().await;
    app.open_remote_link();
    app.copy_remote_link();

    // Assert
    assert_eq!(pending, None);
    assert_eq!(
        copied_through_tmux.as_deref(),
        Some("Link copied. It works once.")
    );
    assert_eq!(
        copied_through_terminal.as_deref(),
        Some("Link copied. It works once.")
    );
    assert_eq!(
        not_copied.as_deref(),
        Some("Could not copy the link; select it instead.")
    );
    assert_eq!(app.remote_control.view().notice, None);
}

/// Verifies a copy result that arrives after the pairing code changed is
/// dropped, because the copied link no longer works. The late result is
/// replayed through the reducer so its arrival order is deterministic. It
/// reports that `tmux` did not copy, so a result that was not dropped would
/// ask the terminal, which the clipboard here does not expect.
#[tokio::test]
async fn link_copy_result_is_dropped_once_the_link_changes() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(
        vec![Some("CODE-0001".to_string()), Some("CODE-0002".to_string())],
        Ok(()),
    );
    let mut clipboard = MockTerminalClipboard::new();
    clipboard
        .expect_copy_through_tmux()
        .times(1)
        .return_const(true);
    install_launcher_with_links(&mut app, launcher, MockBrowserClient::new(), clipboard);
    app.start_remote_control().await;

    // Act
    app.copy_remote_link();
    apply_events_until_link_notice(&mut app).await;
    // The first copy reports under the first request.
    let replaced_request = 1;
    app.issue_remote_pairing_code();
    let after_new_code = app.remote_control.view().notice.clone();
    app.apply_app_events(AppEvent::RemoteLinkCopied {
        is_copied: false,
        request_id: replaced_request,
    })
    .await;

    // Assert
    assert_eq!(after_new_code, None);
    assert_eq!(app.remote_control.view().notice, None);
    app.stop_remote_control().await;
}

#[tokio::test]
async fn showing_remote_control_replaces_the_code_and_drops_old_outcomes() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(vec![Some("FIRST-CODE".to_string())], Ok(()));
    install_launcher(&mut app, launcher);
    app.show_remote_control();
    let link_while_off = shown_link(&app);
    app.start_remote_control().await;
    let first_link = shown_link(&app);
    app.remote_control.view.error = Some("stale".to_string());
    app.remote_control.view.notice = Some("stale".to_string());

    // Act
    app.show_remote_control();

    // Assert
    let view = app.remote_control.view();
    assert_eq!(link_while_off, None);
    assert!(first_link.is_some_and(|link| link.ends_with("&code=FIRST%2DCODE")));
    assert!(shown_link(&app).is_some_and(|link| link.ends_with("&code=LAST%2DCODE")));
    assert_eq!(view.error, None);
    assert_eq!(view.notice, None);
}

#[tokio::test]
async fn device_selection_stays_within_the_paired_devices() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    pair_device(&app, "tablet", "Tablet").await;
    app.refresh_remote_devices().await;

    // Act
    app.remote_control.select_previous_device();
    let before_first = app.remote_control.view().selected_device_index;
    app.remote_control.select_next_device();
    app.remote_control.select_next_device();
    let after_last = app.remote_control.view().selected_device_index;
    app.revoke_remote_device("tablet").await;
    let after_removal = app.remote_control.view().selected_device_index;
    app.remote_control.select_previous_device();

    // Assert
    assert_eq!(before_first, 0);
    assert_eq!(after_last, 1);
    assert_eq!(after_removal, 0);
    assert_eq!(app.remote_control.view().selected_device_index, 0);
}

/// Returns another relay a platform may offer later.
fn moved_platform_relay() -> RemoteControlService {
    RemoteControlService {
        pairing_url: "https://platform.example/pair/".to_string(),
        relay_url: "wss://relay-2.platform.example".to_string(),
    }
}

/// Builds a launcher that starts a fresh fake server each time, recording
/// every relay it is linked to.
fn restartable_launcher() -> (MockRemoteControlLauncher, Arc<Mutex<Vec<String>>>) {
    let relays = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&relays);
    let mut launcher = MockRemoteControlLauncher::new();
    launcher.expect_start().returning(move |relay, _| {
        recorded.lock().expect("relays lock").push(relay.url);
        let (status_tx, _status_rx) = watch::channel(RemoteServerStatus::default());

        Box::new(FakeServer {
            calls: CallLog::default(),
            codes: Mutex::new(Vec::new()),
            revoke_result: Ok(()),
            status_tx,
        }) as Box<dyn RemoteControlServer>
    });

    (launcher, relays)
}

#[tokio::test]
async fn a_new_platform_relay_waits_while_off_and_moves_running_remote_control() {
    // Arrange: remote control is off when the platform's relay first
    // changes.
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, relays) = restartable_launcher();
    app.remote_control = remote_control(&app, launcher);
    app.set_platform_relay(Some(platform_relay())).await;
    let started_while_off = relays.lock().expect("relays lock").len();
    app.start_remote_control().await;

    // Act
    app.set_platform_relay(Some(moved_platform_relay())).await;
    app.set_platform_relay(Some(moved_platform_relay())).await;
    let moved_link = shown_link(&app);
    app.set_platform_relay(None).await;

    // Assert
    assert_eq!(started_while_off, 0);
    assert_eq!(
        *relays.lock().expect("relays lock"),
        vec![platform_relay().relay_url, moved_platform_relay().relay_url]
    );
    assert!(moved_link.is_some_and(|link| link.starts_with("https://platform.example/pair/#")));
    assert_eq!(app.remote_control.view().relay, None);
    assert_eq!(app.remote_control.view().error, None);
}
