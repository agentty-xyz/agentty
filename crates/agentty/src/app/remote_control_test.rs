//! Remote-control lifecycle, pairing code, device, and status tests.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use ag_remote::{
    DeviceStore, DeviceStoreError, NewRemoteDevice, PairingRequest, RelayStatus, RemoteConnection,
    RemoteDevice, RemoteError, RemoteHost, RemoteRelayConfig, RemoteServerStatus, StaticKeypair,
    pairing_link,
};
use async_trait::async_trait;
use mockall::predicate::eq;
use tokio::sync::watch;

use super::{DEFAULT_REMOTE_CONTROL_PORT, RemoteControl};
use crate::app::{App, AppEvent, AppRuntimeEvent, Tab};
use crate::domain::session::Status;
use crate::domain::setting::SettingName;
use crate::infra::browser::MockBrowserClient;
use crate::infra::remote_control::{MockRemoteControlLauncher, RemoteControlServer};
use crate::infra::terminal_clipboard::MockTerminalClipboard;
use crate::presentation::remote_control::{
    RemoteDeviceView, RemoteListenerView, RemotePairingRequestView,
};
use crate::test_support;

/// Calls the fake listener received, in order.
type CallLog = Arc<Mutex<Vec<String>>>;

/// Pairing request the fake listener reports as already settled.
const GONE_REQUEST_ID: u64 = 99;

/// Scripted listener that records calls and publishes test-driven status.
struct FakeServer {
    calls: CallLog,
    codes: Mutex<Vec<Option<String>>>,
    revoke_result: Result<(), String>,
    status_tx: watch::Sender<RemoteServerStatus>,
}

#[async_trait]
impl RemoteControlServer for FakeServer {
    fn local_addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], DEFAULT_REMOTE_CONTROL_PORT))
    }

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

/// Handles a test keeps to drive and observe one fake listener.
struct FakeServerProbe {
    calls: CallLog,
    host: Arc<Mutex<Option<RemoteHost>>>,
    relay: Arc<Mutex<Option<RemoteRelayConfig>>>,
    status_tx: watch::Sender<RemoteServerStatus>,
}

/// Builds a launcher that starts one fake listener on `port`, issuing
/// `codes` in order before `LAST-CODE`.
fn fake_launcher(
    port: u16,
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
        .with(
            eq(port),
            mockall::predicate::always(),
            mockall::predicate::always(),
            mockall::predicate::always(),
        )
        .times(1)
        .return_once(move |_, _, relay, host| {
            *captured_host.lock().expect("host lock") = Some(host);
            *captured_relay.lock().expect("relay lock") = relay;

            Ok(Box::new(server) as Box<dyn RemoteControlServer>)
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

/// Replaces the app's remote control with `launcher` over the app database.
async fn install_launcher(app: &mut App, launcher: MockRemoteControlLauncher) {
    app.remote_control = remote_control(app, launcher);
    app.enable_remote_control(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), None)
        .await;
}

/// Replaces the app's remote control with a relay-linked one over
/// `launcher`, `browser`, and `clipboard`.
async fn install_relay_launcher(
    app: &mut App,
    launcher: MockRemoteControlLauncher,
    browser: MockBrowserClient,
    clipboard: MockTerminalClipboard,
) {
    app.remote_control = remote_control_with_links(app, launcher, browser, clipboard);
    app.enable_remote_control(
        DEFAULT_REMOTE_CONTROL_PORT,
        Vec::new(),
        Some(RELAY_URL.to_string()),
    )
    .await;
}

/// Relay the relay-linked tests register with.
const RELAY_URL: &str = "wss://relay.example/";

/// Stores one paired device directly in the app database.
async fn pair_device(app: &App, id: &str, name: &str) {
    app.services
        .db()
        .remote_devices()
        .insert_device(id, name, &format!("hash-{id}"))
        .await
        .expect("device should be stored");
}

/// Applies background app events until a paired device shows as connected.
async fn apply_events_until_connected(app: &mut App) {
    apply_events_until(app, |app| {
        app.remote_control.view().connected_device_count() > 0
    })
    .await;
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
        relay: None,
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
        relay: None,
    }
}

#[tokio::test]
async fn start_remote_control_starts_listener_on_default_port_and_shows_code() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(
        DEFAULT_REMOTE_CONTROL_PORT,
        vec![Some("ABCD-EFGH".to_string())],
        Ok(()),
    );
    install_launcher(&mut app, launcher).await;

    // Act
    app.start_remote_control().await;

    // Assert
    let view = app.remote_control.view();
    assert_eq!(
        view.listener,
        Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
            relay: None,
            url: "http://127.0.0.1:4646/".to_string(),
        })
    );
    assert_eq!(view.error, None);
    assert_eq!(view.devices, []);
}

#[tokio::test]
async fn reopening_remote_control_keeps_listener_and_replaces_code() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(
        DEFAULT_REMOTE_CONTROL_PORT,
        vec![Some("FIRST-CODE".to_string())],
        Ok(()),
    );
    install_launcher(&mut app, launcher).await;
    app.start_remote_control().await;

    // Act
    app.start_remote_control().await;

    // Assert
    let pairing_code = app
        .remote_control
        .view()
        .listener
        .as_ref()
        .map(|listener| listener.pairing_code.clone());
    assert_eq!(pairing_code.as_deref(), Some("LAST-CODE"));
}

#[tokio::test]
async fn start_remote_control_reports_start_failure() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let mut launcher = MockRemoteControlLauncher::new();
    launcher
        .expect_start()
        .times(1)
        .return_once(|_, _, _, _| Err("Port 4646 is in use".to_string()));
    install_launcher(&mut app, launcher).await;

    // Act
    app.start_remote_control().await;

    // Assert
    let view = app.remote_control.view();
    assert_eq!(view.listener, None);
    assert_eq!(view.error.as_deref(), Some("Port 4646 is in use"));
}

#[tokio::test]
async fn enable_remote_control_selects_the_listener_port() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(0, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
    app.enable_remote_control(0, Vec::new(), None).await;

    // Act
    app.start_remote_control().await;

    // Assert
    assert!(app.remote_control.view().listener.is_some());
}

#[tokio::test]
async fn enable_remote_control_passes_allowed_hosts_to_the_listener() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let mut launcher = MockRemoteControlLauncher::new();
    launcher
        .expect_start()
        .with(
            eq(DEFAULT_REMOTE_CONTROL_PORT),
            eq(vec!["box.tailnet.ts.net".to_string()]),
            eq(None),
            mockall::predicate::always(),
        )
        .times(1)
        .return_once(|_, _, _, _| Err("stop here".to_string()));
    install_launcher(&mut app, launcher).await;
    app.enable_remote_control(
        DEFAULT_REMOTE_CONTROL_PORT,
        vec!["box.tailnet.ts.net".to_string()],
        None,
    )
    .await;

    // Act
    app.start_remote_control().await;

    // Assert
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("stop here")
    );
}

#[tokio::test]
async fn pending_pairings_are_shown_then_approved_or_denied_oldest_first() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
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
    let (launcher, _probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
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
    let (launcher, _probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
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
async fn start_remote_control_without_the_flag_reports_how_to_enable_it() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.remote_control = remote_control(&app, MockRemoteControlLauncher::new());

    // Act
    app.start_remote_control().await;

    // Assert
    assert!(!app.remote_control.is_enabled());
    assert_eq!(app.remote_control.view().listener, None);
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("Remote control needs `agentty --experimental-remote`.")
    );
}

#[tokio::test]
async fn pairing_code_failure_is_reported_and_code_is_ignored_while_off() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.issue_remote_pairing_code();
    let view_while_off = app.remote_control.view().clone();
    let (launcher, _probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, vec![None], Ok(()));
    install_launcher(&mut app, launcher).await;

    // Act
    app.start_remote_control().await;

    // Assert
    assert_eq!(view_while_off.listener, None);
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("Cannot generate a secure pairing code")
    );
}

#[tokio::test]
async fn listener_status_marks_connected_devices() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    pair_device(&app, "tablet", "Tablet").await;
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
    app.start_remote_control().await;

    // Act
    probe.status_tx.send_replace(connected("tablet"));
    apply_events_until_connected(&mut app).await;

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
async fn status_from_a_stopped_listener_is_ignored() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    let (launcher, _probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
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
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
    app.start_remote_control().await;
    probe.status_tx.send_replace(connected("phone"));
    apply_events_until_connected(&mut app).await;

    // Act
    app.stop_remote_control().await;
    app.stop_remote_control().await;

    // Assert
    let view = app.remote_control.view();
    assert_eq!(view.listener, None);
    assert_eq!(view.connected_device_count(), 0);
    assert_eq!(view.devices.len(), 1);
    assert_eq!(*probe.calls.lock().expect("calls lock"), ["shutdown"]);
}

#[tokio::test]
async fn status_forwarding_ends_once_the_listener_is_gone() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
    app.start_remote_control().await;
    app.stop_remote_control().await;

    // Act
    drop(probe.status_tx);
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    app.process_pending_app_events().await;

    // Assert
    assert_eq!(app.remote_control.view().listener, None);
    assert_eq!(app.remote_control.view().connected_device_count(), 0);
}

#[tokio::test]
async fn revoke_remote_device_uses_the_running_listener() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(
        DEFAULT_REMOTE_CONTROL_PORT,
        Vec::new(),
        Err("Could not remove the device: gone".to_string()),
    );
    install_launcher(&mut app, launcher).await;
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
async fn publish_session_change_advances_the_remote_revision() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_launcher(&mut app, launcher).await;
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

#[tokio::test]
async fn status_line_and_queued_action_changes_advance_the_remote_revision_once_each() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.sessions.push_session(test_support::session_fixture(
        "session-1",
        Status::InProgress,
    ));
    app.sessions
        .push_session(test_support::session_fixture("session-2", Status::Review));
    let mut revisions = app.remote_control.subscribe_revisions();
    let initial_revision = *revisions.borrow_and_update();

    // Act
    app.publish_live_session_changes();
    let after_first_line = *revisions.borrow_and_update();
    app.publish_live_session_changes();
    let unchanged = revisions
        .has_changed()
        .expect("revision sender should live");
    app.session_progress_messages
        .insert("session-1".into(), "Reading files".to_string());
    app.publish_live_session_changes();
    let after_progress = *revisions.borrow_and_update();
    app.sessions.queue_session_sync("session-2", 1);
    app.publish_live_session_changes();
    let after_queued_action = *revisions.borrow_and_update();

    // Assert
    assert_eq!(after_first_line, initial_revision + 1);
    assert!(!unchanged);
    assert_eq!(after_progress, initial_revision + 2);
    assert_eq!(after_queued_action, initial_revision + 3);
}

#[tokio::test]
async fn relay_listener_gets_a_saved_key_and_shows_a_pairing_link() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_relay_launcher(
        &mut app,
        launcher,
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    )
    .await;

    // Act
    app.start_remote_control().await;
    let generation = app.remote_control.generation;
    let status = RemoteServerStatus {
        relay: Some(RelayStatus::Connected),
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
        .expect("listener should get a relay");
    assert_eq!(relay.url, RELAY_URL);
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
        .listener
        .clone()
        .and_then(|listener| listener.relay)
        .expect("relay should be shown");
    assert!(relay_view.is_connected);
    assert_eq!(
        relay_view.link,
        pairing_link(RELAY_URL, relay.instance_key.public(), "LAST-CODE")
    );
    assert_ne!(relay_view.qr_code, Vec::<String>::new());
}

#[tokio::test]
async fn relay_listener_reuses_the_saved_key() {
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
    let (launcher, probe) = fake_launcher(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), Ok(()));
    install_relay_launcher(
        &mut app,
        launcher,
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    )
    .await;

    // Act
    app.start_remote_control().await;

    // Assert
    let relay = probe.relay.lock().expect("relay lock").clone();
    assert_eq!(relay.map(|relay| relay.instance_key), Some(saved));
}

#[tokio::test]
async fn invalid_saved_key_stops_the_listener_from_starting() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.services
        .db()
        .settings()
        .upsert_setting(SettingName::RemoteInstanceKey, "not a key")
        .await
        .expect("setting should save");
    install_relay_launcher(
        &mut app,
        MockRemoteControlLauncher::new(),
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    )
    .await;

    // Act
    app.start_remote_control().await;

    // Assert
    assert_eq!(app.remote_control.view().listener, None);
    assert_eq!(
        app.remote_control.view().error.as_deref(),
        Some("The stored remote key is invalid.")
    );
}

/// Verifies the link is opened in the browser and copied through `tmux`,
/// through the terminal when `tmux` does not take it, and reported as not
/// copied when neither does.
#[tokio::test]
async fn link_actions_open_and_copy_the_relay_link() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(
        DEFAULT_REMOTE_CONTROL_PORT,
        vec![Some("CODE-0001".to_string())],
        Ok(()),
    );
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
    install_relay_launcher(&mut app, launcher, browser, clipboard).await;
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
        DEFAULT_REMOTE_CONTROL_PORT,
        vec![Some("CODE-0001".to_string()), Some("CODE-0002".to_string())],
        Ok(()),
    );
    let mut clipboard = MockTerminalClipboard::new();
    clipboard
        .expect_copy_through_tmux()
        .times(1)
        .return_const(true);
    install_relay_launcher(&mut app, launcher, MockBrowserClient::new(), clipboard).await;
    app.start_remote_control().await;

    // Act
    app.copy_remote_link();
    apply_events_until_link_notice(&mut app).await;
    let replaced_request = app.remote_control.link_copy_request;
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
async fn enable_remote_control_offers_the_remote_tab_with_paired_devices() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    app.tabs.set(Tab::Remote);
    let tab_before = app.tabs.current();
    let is_projected_before = app.view_snapshot().remote_control.is_some();

    // Act
    app.enable_remote_control(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), None)
        .await;

    // Assert
    assert_eq!(tab_before, Tab::Sessions);
    assert!(!is_projected_before);
    assert_eq!(app.tabs.current(), Tab::Remote);
    assert_eq!(
        app.view_snapshot().remote_control.map(|view| &view.devices),
        Some(&vec![RemoteDeviceView {
            id: "phone".to_string(),
            is_connected: false,
            name: "Phone".to_string(),
        }])
    );
}

#[tokio::test]
async fn entering_the_remote_tab_replaces_the_code_and_drops_old_outcomes() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(
        DEFAULT_REMOTE_CONTROL_PORT,
        vec![Some("FIRST-CODE".to_string())],
        Ok(()),
    );
    install_launcher(&mut app, launcher).await;
    app.tabs.set(Tab::Settings);
    app.next_tab();
    let listener_while_off = app.remote_control.view().listener.clone();
    app.start_remote_control().await;
    app.remote_control.view.error = Some("stale".to_string());
    app.remote_control.view.notice = Some("stale".to_string());

    // Act
    app.next_tab();
    let code_on_another_tab = app.remote_control.view().listener.clone();
    app.previous_tab();

    // Assert
    let view = app.remote_control.view();
    assert_eq!(listener_while_off, None);
    assert_eq!(
        code_on_another_tab.map(|listener| listener.pairing_code),
        Some("FIRST-CODE".to_string())
    );
    assert_eq!(app.tabs.current(), Tab::Remote);
    assert_eq!(
        view.listener
            .as_ref()
            .map(|listener| listener.pairing_code.as_str()),
        Some("LAST-CODE")
    );
    assert_eq!(view.error, None);
    assert_eq!(view.notice, None);
}

#[tokio::test]
async fn device_selection_stays_within_the_paired_devices() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    pair_device(&app, "phone", "Phone").await;
    pair_device(&app, "tablet", "Tablet").await;
    app.enable_remote_control(DEFAULT_REMOTE_CONTROL_PORT, Vec::new(), None)
        .await;

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
