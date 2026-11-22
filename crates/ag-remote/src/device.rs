//! Paired-device records and the host persistence port.

use ag_noise::PublicKey;
use async_trait::async_trait;
use sha2::{Digest, Sha256};

/// Starts the digest input of a device public key. `0xFF` never occurs in
/// UTF-8 text, so no token shares this prefix.
const KEY_HASH_PREFIX: &[u8] = b"\xFFagentty-device-key";

/// One paired device, as the host stores it. Tokens are never retained.
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
    /// Lowercase hexadecimal SHA-256 digest of the device credential: its
    /// token, or its public key for a device paired through a relay.
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

    /// Finds the device whose credential digest is `token_hash`.
    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError>;

    /// Persists one newly paired device.
    async fn insert_device(&self, device: NewRemoteDevice) -> Result<(), DeviceStoreError>;

    /// Records that `device_id` authenticated now.
    async fn touch_device(&self, device_id: &str) -> Result<(), DeviceStoreError>;

    /// Removes one device so its token stops authenticating.
    async fn delete_device(&self, device_id: &str) -> Result<(), DeviceStoreError>;
}

/// Returns the lowercase hexadecimal SHA-256 digest that stores `token`.
pub(crate) fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Returns the digest that stores the public key of a device paired through
/// a relay.
///
/// The digest input starts with a byte that no token can contain, because
/// tokens arrive as text. A key digest therefore never equals a token
/// digest, and presenting a public key as a token authenticates nothing.
pub(crate) fn key_hash(public_key: &PublicKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(KEY_HASH_PREFIX);
    hasher.update(public_key.as_bytes());

    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[path = "device_test.rs"]
mod tests;
