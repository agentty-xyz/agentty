//! Paired-device records and the host persistence port.

use ag_noise::PublicKey;
use async_trait::async_trait;
use sha2::{Digest, Sha256};

/// Starts the digest input of a device public key. Every stored device
/// digest depends on it, so changing it unpairs every device. `0xFF` never
/// occurs in UTF-8 text, so no digest of a text value equals a key digest.
const KEY_HASH_PREFIX: &[u8] = b"\xFFagentty-device-key";

/// One paired device, as the host stores it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteDevice {
    /// Pairing timestamp in Unix seconds.
    pub created_at: i64,
    /// Stable device identifier.
    pub id: String,
    /// Last successful authentication timestamp in Unix seconds.
    pub last_seen_at: Option<i64>,
    /// Name the device chose while pairing.
    pub name: String,
}

/// A device record to persist after successful pairing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewRemoteDevice {
    /// Stable device identifier.
    pub id: String,
    /// Name the device chose while pairing.
    pub name: String,
    /// Lowercase hexadecimal SHA-256 digest of the device's public key, the
    /// credential [`DeviceStore::find_device_by_token_hash`] looks up.
    pub token_hash: String,
}

/// Failure reported by a [`DeviceStore`] implementation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct DeviceStoreError(pub String);

/// Host persistence for paired devices.
///
/// Implementations own timestamps: `insert_device` records the pairing time
/// and `touch_device` records the latest authentication.
#[async_trait]
pub trait DeviceStore: Send + Sync {
    /// Lists every paired device.
    async fn list_devices(&self) -> Result<Vec<RemoteDevice>, DeviceStoreError>;

    /// Finds the device whose credential digest, its key digest, is
    /// `token_hash`.
    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError>;

    /// Persists one newly paired device.
    async fn insert_device(&self, device: NewRemoteDevice) -> Result<(), DeviceStoreError>;

    /// Records that `device_id` authenticated now.
    async fn touch_device(&self, device_id: &str) -> Result<(), DeviceStoreError>;

    /// Removes one device so its key stops authenticating.
    async fn delete_device(&self, device_id: &str) -> Result<(), DeviceStoreError>;
}

/// Returns the digest that stores the public key of a paired device.
pub(crate) fn key_hash(public_key: &PublicKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(KEY_HASH_PREFIX);
    hasher.update(public_key.as_bytes());

    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[path = "device_test.rs"]
mod tests;
