//! Remote-control server boundary over the `ag-remote` server, plus the
//! `ag-store` persistence of its paired devices.

use std::sync::Arc;

use ag_remote::{
    DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteDevice, RemoteError, RemoteHost,
    RemoteRelayConfig, RemoteServerConfig, RemoteServerHandle, RemoteServerStatus,
};
use ag_store::{RemoteDeviceRepository, RemoteDeviceRow};
use async_trait::async_trait;
use tokio::sync::watch;

/// [`DeviceStore`] backed by the `remote_device` table.
pub(crate) struct RepositoryDeviceStore {
    repository: Arc<dyn RemoteDeviceRepository>,
}

impl RepositoryDeviceStore {
    /// Persists paired devices through `repository`.
    pub(crate) fn new(repository: Arc<dyn RemoteDeviceRepository>) -> Self {
        Self { repository }
    }

    /// Converts one stored row into the server's device model.
    fn device(row: RemoteDeviceRow) -> RemoteDevice {
        RemoteDevice {
            created_at: row.created_at,
            id: row.id,
            last_seen_at: row.last_seen_at,
            name: row.name,
        }
    }

    /// Wraps a persistence failure for the server.
    fn error(error: impl std::fmt::Display) -> DeviceStoreError {
        DeviceStoreError(error.to_string())
    }
}

#[async_trait]
impl DeviceStore for RepositoryDeviceStore {
    async fn list_devices(&self) -> Result<Vec<RemoteDevice>, DeviceStoreError> {
        let rows = self.repository.list_devices().await.map_err(Self::error)?;

        Ok(rows.into_iter().map(Self::device).collect())
    }

    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError> {
        let row = self
            .repository
            .find_device_by_token_hash(token_hash)
            .await
            .map_err(Self::error)?;

        Ok(row.map(Self::device))
    }

    async fn insert_device(&self, device: NewRemoteDevice) -> Result<(), DeviceStoreError> {
        self.repository
            .insert_device(&device.id, &device.name, &device.token_hash)
            .await
            .map_err(Self::error)
    }

    async fn touch_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        self.repository
            .touch_device(device_id)
            .await
            .map_err(Self::error)
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        self.repository
            .delete_device(device_id)
            .await
            .map_err(Self::error)
    }
}

/// Starts remote-control servers, which reach devices only through a relay.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait RemoteControlLauncher: Send + Sync {
    /// Registers with `relay` and serves `host` to the devices that connect
    /// through it until the returned server shuts down.
    fn start(&self, relay: RemoteRelayConfig, host: RemoteHost) -> Box<dyn RemoteControlServer>;
}

/// One running remote-control server.
#[async_trait]
pub(crate) trait RemoteControlServer: Send + Sync {
    /// Replaces any active pairing code and returns its display form.
    ///
    /// # Errors
    /// Returns [`RemoteError::Entropy`] when no secure code can be generated.
    fn issue_pairing_code(&self) -> Result<String, RemoteError>;

    /// Subscribes to the authenticated connections and pending pairings.
    fn status(&self) -> watch::Receiver<RemoteServerStatus>;

    /// Lets one pending device finish pairing; `false` when the request is
    /// no longer pending.
    fn approve_pairing(&self, request_id: u64) -> bool;

    /// Refuses one pending device; `false` when the request is no longer
    /// pending.
    fn deny_pairing(&self, request_id: u64) -> bool;

    /// Forgets one paired device and closes its live connections.
    ///
    /// # Errors
    /// Returns a user-facing message when the device cannot be removed.
    async fn revoke_device(&self, device_id: &str) -> Result<(), String>;

    /// Leaves the relay and closes every connection.
    async fn shutdown(self: Box<Self>);
}

/// Production [`RemoteControlLauncher`] backed by [`ag_remote::start`].
pub(crate) struct RealRemoteControlLauncher;

impl RemoteControlLauncher for RealRemoteControlLauncher {
    fn start(&self, relay: RemoteRelayConfig, host: RemoteHost) -> Box<dyn RemoteControlServer> {
        Box::new(ag_remote::start(RemoteServerConfig { relay }, host))
    }
}

#[async_trait]
impl RemoteControlServer for RemoteServerHandle {
    fn issue_pairing_code(&self) -> Result<String, RemoteError> {
        RemoteServerHandle::issue_pairing_code(self).map(|pairing_code| pairing_code.code)
    }

    fn status(&self) -> watch::Receiver<RemoteServerStatus> {
        RemoteServerHandle::status(self)
    }

    fn approve_pairing(&self, request_id: u64) -> bool {
        RemoteServerHandle::approve_pairing(self, request_id)
    }

    fn deny_pairing(&self, request_id: u64) -> bool {
        RemoteServerHandle::deny_pairing(self, request_id)
    }

    async fn revoke_device(&self, device_id: &str) -> Result<(), String> {
        RemoteServerHandle::revoke_device(self, device_id)
            .await
            .map_err(|error| format!("Could not remove the device: {error}"))
    }

    async fn shutdown(self: Box<Self>) {
        RemoteServerHandle::shutdown(*self).await;
    }
}

#[cfg(test)]
#[path = "remote_control_test.rs"]
mod tests;
