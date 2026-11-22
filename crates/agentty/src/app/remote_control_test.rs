//! Remote-control lifecycle, pairing code, device, and status tests.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use ag_remote::{
    DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteConnection, RemoteDevice, RemoteError,
    RemoteHost, RemoteServerStatus,
};
use async_trait::async_trait;
use mockall::predicate::eq;
use tokio::sync::watch;

use super::{DEFAULT_REMOTE_CONTROL_PORT, RemoteControl};
use crate::app::{App, AppRuntimeEvent};
use crate::infra::remote_control::{MockRemoteControlLauncher, RemoteControlServer};
use crate::presentation::remote_control::{RemoteDeviceView, RemoteListenerView};
use crate::test_support;

/// Calls the fake listener received, in order.
type CallLog = Arc<Mutex<Vec<String>>>;

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
    let (status_tx, _status_rx) = watch::channel(RemoteServerStatus::default());
    let server = FakeServer {
        calls: Arc::clone(&calls),
        codes: Mutex::new(codes.into_iter().rev().collect()),
        revoke_result,
        status_tx: status_tx.clone(),
    };
    let captured_host = Arc::clone(&host);
    let mut launcher = MockRemoteControlLauncher::new();
    launcher
        .expect_start()
        .with(eq(port), mockall::predicate::always())
        .times(1)
        .return_once(move |_, host| {
            *captured_host.lock().expect("host lock") = Some(host);

            Ok(Box::new(server) as Box<dyn RemoteControlServer>)
        });

    (
        launcher,
        FakeServerProbe {
            calls,
            host,
            status_tx,
        },
    )
}

/// Replaces the app's remote control with `launcher` over the app database.
fn install_launcher(app: &mut App, launcher: MockRemoteControlLauncher) {
    app.remote_control = RemoteControl::new(Arc::new(launcher), app.services.db());
    app.enable_remote_control(DEFAULT_REMOTE_CONTROL_PORT);
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

/// Applies background app events until a paired device shows as connected.
///
/// Startup work may queue unrelated events ahead of the forwarded status.
async fn apply_events_until_connected(app: &mut App) {
    let wait = async {
        while app.remote_control.view().connected_device_count() == 0 {
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
        .expect("forwarded status should mark a device connected");
}

/// Builds the status a listener reports for one connected device.
fn connected(device_id: &str) -> RemoteServerStatus {
    RemoteServerStatus {
        connections: vec![RemoteConnection {
            device_id: device_id.to_string(),
            device_name: device_id.to_string(),
        }],
    }
}

#[tokio::test]
async fn open_remote_control_starts_listener_on_default_port_and_shows_code() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let (launcher, _probe) = fake_launcher(
        DEFAULT_REMOTE_CONTROL_PORT,
        vec![Some("ABCD-EFGH".to_string())],
        Ok(()),
    );
    install_launcher(&mut app, launcher);

    // Act
    app.open_remote_control().await;

    // Assert
    let view = app.remote_control.view();
    assert_eq!(
        view.listener,
        Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;

    // Act
    app.open_remote_control().await;

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
async fn open_remote_control_reports_start_failure() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let mut launcher = MockRemoteControlLauncher::new();
    launcher
        .expect_start()
        .times(1)
        .return_once(|_, _| Err("Port 4646 is in use".to_string()));
    install_launcher(&mut app, launcher);

    // Act
    app.open_remote_control().await;

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
    install_launcher(&mut app, launcher);
    app.enable_remote_control(0);

    // Act
    app.open_remote_control().await;

    // Assert
    assert!(app.remote_control.view().listener.is_some());
}

#[tokio::test]
async fn open_remote_control_without_the_flag_reports_how_to_enable_it() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.remote_control = RemoteControl::new(
        Arc::new(MockRemoteControlLauncher::new()),
        app.services.db(),
    );

    // Act
    app.open_remote_control().await;

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
    install_launcher(&mut app, launcher);

    // Act
    app.open_remote_control().await;

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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;

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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;
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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;
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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;
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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;

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
    install_launcher(&mut app, launcher);
    app.open_remote_control().await;
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
