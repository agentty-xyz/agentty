//! Remote-control server and paired-device persistence boundary tests.

use std::sync::Arc;
use std::time::Duration;

use ag_relay::RelayHandle;
use ag_remote::{
    DeviceStore, NewRemoteDevice, RelayStatus, RemoteDevice, RemoteHost, RemoteRelayConfig,
    StaticKeypair,
};
use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, ReviewRequest, Session,
    SessionBackend, SessionError, SessionId, SessionService,
};
use ag_store::{AppRepositories, Database, DbError, MockRemoteDeviceRepository};
use async_trait::async_trait;
use tokio::sync::watch;

use super::{RealRemoteControlLauncher, RemoteControlLauncher, RepositoryDeviceStore};
use crate::test_support::start_relay;

/// Session backend for server tests that never receive requests.
struct IdleBackend;

#[async_trait]
impl SessionBackend for IdleBackend {
    async fn create_session(
        &self,
        _request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        Err(SessionError::NotFound)
    }

    async fn get_session(&self, _session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        Ok(None)
    }

    async fn send_message(
        &self,
        _session_id: &SessionId,
        _message: String,
    ) -> Result<(), SessionError> {
        Err(SessionError::NotFound)
    }

    async fn submit_durable_message(
        &self,
        _session_id: &SessionId,
        _request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::NotFound)
    }

    async fn answer_questions(
        &self,
        _session_id: &SessionId,
        _request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::NotFound)
    }

    async fn cancel_session(&self, _session_id: &SessionId) -> Result<(), SessionError> {
        Err(SessionError::NotFound)
    }

    async fn merge_session(&self, _session_id: &SessionId) -> Result<(), SessionError> {
        Err(SessionError::NotFound)
    }

    async fn create_review_request(
        &self,
        _session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        Err(SessionError::NotFound)
    }
}

/// Opens an in-memory database and returns its device store.
async fn database_device_store() -> RepositoryDeviceStore {
    let database = Database::open_in_memory()
        .await
        .expect("in-memory database should open");

    RepositoryDeviceStore::new(AppRepositories::from(database).remote_devices())
}

/// Builds a server host over `devices` with an idle session backend.
fn host(devices: Arc<dyn DeviceStore>) -> RemoteHost {
    let (_revision_tx, revisions) = watch::channel(0);

    RemoteHost {
        devices,
        revisions,
        sessions: SessionService::new(Arc::new(IdleBackend)),
    }
}

/// Builds a repository mock whose every operation fails.
fn failing_repository() -> MockRemoteDeviceRepository {
    let failure = || DbError::InvalidData {
        entity: "remote_device",
        reason: "disk full".to_string(),
    };
    let mut repository = MockRemoteDeviceRepository::new();
    repository
        .expect_list_devices()
        .returning(move || Err(failure()));
    repository
        .expect_find_device_by_token_hash()
        .returning(move |_| Err(failure()));
    repository
        .expect_insert_device()
        .returning(move |_, _, _| Err(failure()));
    repository
        .expect_touch_device()
        .returning(move |_| Err(failure()));
    repository
        .expect_delete_device()
        .returning(move |_| Err(failure()));

    repository
}

#[tokio::test]
async fn repository_device_store_persists_paired_devices() {
    // Arrange
    let store = database_device_store().await;

    // Act
    store
        .insert_device(NewRemoteDevice {
            id: "phone".to_string(),
            name: "Phone".to_string(),
            token_hash: "hash".to_string(),
        })
        .await
        .expect("device should be stored");
    store
        .touch_device("phone")
        .await
        .expect("device should be touched");
    let found = store
        .find_device_by_token_hash("hash")
        .await
        .expect("lookup should succeed");
    let listed = store.list_devices().await.expect("listing should succeed");
    store
        .delete_device("phone")
        .await
        .expect("device should be deleted");
    let remaining = store.list_devices().await.expect("listing should succeed");

    // Assert
    let found = found.expect("device should be found by its token hash");
    assert_eq!(found.id, "phone");
    assert_eq!(found.name, "Phone");
    assert!(found.last_seen_at.is_some());
    assert_eq!(
        listed
            .iter()
            .map(|device| device.id.as_str())
            .collect::<Vec<_>>(),
        ["phone"]
    );
    assert_eq!(remaining, []);
}

#[tokio::test]
async fn repository_device_store_reports_storage_failures() {
    // Arrange
    let store = RepositoryDeviceStore::new(Arc::new(failing_repository()));
    let new_device = NewRemoteDevice {
        id: "phone".to_string(),
        name: "Phone".to_string(),
        token_hash: "hash".to_string(),
    };

    // Act
    let listed = store.list_devices().await;
    let found = store.find_device_by_token_hash("hash").await;
    let inserted = store.insert_device(new_device).await;
    let touched = store.touch_device("phone").await;
    let deleted = store.delete_device("phone").await;

    // Assert
    let expected = "Invalid remote_device: disk full";
    assert_eq!(listed.map_err(|error| error.0), Err(expected.to_string()));
    assert_eq!(
        found.map_err(|error| error.0),
        Err::<Option<RemoteDevice>, _>(expected.to_string())
    );
    assert_eq!(inserted.map_err(|error| error.0), Err(expected.to_string()));
    assert_eq!(touched.map_err(|error| error.0), Err(expected.to_string()));
    assert_eq!(deleted.map_err(|error| error.0), Err(expected.to_string()));
}

/// The link of a server that registers with `relay` under a fresh key.
fn relay_link(relay: &RelayHandle) -> RemoteRelayConfig {
    RemoteRelayConfig {
        instance_key: StaticKeypair::generate().expect("instance key"),
        url: format!("ws://{}", relay.local_addr()),
    }
}

#[tokio::test]
async fn real_launcher_registers_with_the_relay_until_shutdown() {
    // Arrange
    let relay = start_relay().await;
    let devices: Arc<dyn DeviceStore> = Arc::new(database_device_store().await);
    let launcher = RealRemoteControlLauncher;

    // Act
    let server = launcher.start(relay_link(&relay), host(devices));
    let registered = tokio::time::timeout(
        Duration::from_secs(5),
        server
            .status()
            .wait_for(|status| status.relay == RelayStatus::Connected),
    )
    .await
    .is_ok();
    let pairing_code = server
        .issue_pairing_code()
        .expect("pairing code should be issued");
    let status = server.status().borrow().clone();
    let approved_unknown = server.approve_pairing(7);
    let denied_unknown = server.deny_pairing(7);
    let revoked = server.revoke_device("missing").await;
    server.shutdown().await;

    // Assert
    assert!(registered, "the server should register with the relay");
    assert_eq!(pairing_code.len(), 9);
    assert_eq!(pairing_code.chars().nth(4), Some('-'));
    assert_eq!(status.connections, []);
    assert_eq!(status.pairing_requests, []);
    assert!(!approved_unknown, "an unknown request cannot be approved");
    assert!(!denied_unknown, "an unknown request cannot be denied");
    assert_eq!(revoked, Ok(()));
}

#[tokio::test]
async fn real_launcher_reports_store_failure_when_revoking() {
    // Arrange
    let relay = start_relay().await;
    let launcher = RealRemoteControlLauncher;
    let server = launcher.start(
        relay_link(&relay),
        host(Arc::new(RepositoryDeviceStore::new(Arc::new(
            failing_repository(),
        )))),
    );

    // Act
    let revoked = server.revoke_device("phone").await;
    server.shutdown().await;

    // Assert
    let message = revoked.expect_err("revocation should fail");
    assert!(
        message.starts_with("Could not remove the device:"),
        "unexpected message: {message}"
    );
}
