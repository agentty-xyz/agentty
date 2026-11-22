//! Paired remote-control device persistence.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::DbError;
use crate::timestamp::TimestampSource;

/// One paired remote-control device, without its token hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteDeviceRow {
    /// Pairing timestamp in Unix seconds.
    pub created_at: i64,
    /// Stable device identifier.
    pub id: String,
    /// Last authenticated connection timestamp in Unix seconds, when any.
    pub last_seen_at: Option<i64>,
    /// User-visible device name supplied during pairing.
    pub name: String,
}

/// Paired-device persistence boundary used by remote-control hosts.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait RemoteDeviceRepository: Send + Sync {
    /// Removes one paired device; an unknown identifier is a no-op.
    async fn delete_device(&self, id: &str) -> Result<(), DbError>;

    /// Finds the device whose token hashes to `token_hash`.
    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDeviceRow>, DbError>;

    /// Persists one newly paired device, stamped with the current time.
    ///
    /// Fails when `id` or `token_hash` already belongs to a device.
    async fn insert_device(&self, id: &str, name: &str, token_hash: &str) -> Result<(), DbError>;

    /// Lists paired devices in pairing order.
    async fn list_devices(&self) -> Result<Vec<RemoteDeviceRow>, DbError>;

    /// Records an authenticated connection for one device; an unknown
    /// identifier is a no-op.
    async fn touch_device(&self, id: &str) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`RemoteDeviceRepository`].
#[derive(Clone)]
pub(crate) struct SqliteRemoteDeviceRepository {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteRemoteDeviceRepository {
    /// Creates a device repository backed by the provided pool and timestamp
    /// source.
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }
}

#[async_trait]
impl RemoteDeviceRepository for SqliteRemoteDeviceRepository {
    async fn delete_device(&self, id: &str) -> Result<(), DbError> {
        sqlx::query!(
            r"
DELETE FROM remote_device
WHERE id = ?
",
            id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDeviceRow>, DbError> {
        let row = sqlx::query_as!(
            RemoteDeviceRow,
            r#"
SELECT id AS "id!: String", name, created_at, last_seen_at
FROM remote_device
WHERE token_hash = ?
"#,
            token_hash
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    async fn insert_device(&self, id: &str, name: &str, token_hash: &str) -> Result<(), DbError> {
        let created_at = self.timestamp_source.now_timestamp_seconds();
        sqlx::query!(
            r"
INSERT INTO remote_device (id, name, token_hash, created_at)
VALUES (?, ?, ?, ?)
",
            id,
            name,
            token_hash,
            created_at
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn list_devices(&self) -> Result<Vec<RemoteDeviceRow>, DbError> {
        let rows = sqlx::query_as!(
            RemoteDeviceRow,
            r#"
SELECT id AS "id!: String", name, created_at, last_seen_at
FROM remote_device
ORDER BY created_at, id
"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    async fn touch_device(&self, id: &str) -> Result<(), DbError> {
        let last_seen_at = self.timestamp_source.now_timestamp_seconds();
        sqlx::query!(
            r"
UPDATE remote_device
SET last_seen_at = ?
WHERE id = ?
",
            last_seen_at,
            id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "remote_device_test.rs"]
mod tests;
