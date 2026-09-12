//! Write-ahead records independent of completed conversation history.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use sqlx::SqlitePool;

use crate::session::{SessionError, TimestampSource};

/// A durable repository write intent and its acknowledged outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteRecord {
    /// Provider tool-call identifier; unique only within its model response.
    pub call_id: String,
    /// SHA-256 of the expected file, or `None` for a create operation.
    pub expected_hash: Option<String>,
    /// Stable identifier of this write intent within its database.
    pub id: i64,
    /// Repository-relative target path.
    pub path: String,
    /// Canonical native repository root, serialized as Unix path bytes for
    /// lossless host-side inspection.
    #[serde(serialize_with = "serialize_repository_root")]
    pub repository_root: PathBuf,
    /// SHA-256 of the intended resulting file.
    pub resulting_hash: String,
    /// Whether replacement returned success, failed, or never acknowledged.
    pub status: WriteStatus,
    /// Persistent turn position that requested the write.
    pub turn_position: i64,
}

/// Acknowledged filesystem outcome, preserved independently of turn success.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteStatus {
    /// Intent is durable, but no filesystem result was recorded.
    Pending,
    /// The filesystem acknowledged successful replacement.
    Applied,
    /// The filesystem returned an error; this does not prove the file is
    /// unchanged.
    Failed,
}

pub(crate) struct WriteRecordRow {
    call_id: String,
    expected_hash: Option<String>,
    id: i64,
    path: String,
    repository_root: Vec<u8>,
    resulting_hash: String,
    status: String,
    turn_position: i64,
}

impl WriteRecordRow {
    pub(crate) async fn load(
        pool: &SqlitePool,
        session_id: &str,
    ) -> Result<Vec<WriteRecord>, SessionError> {
        let rows = sqlx::query_as!(
            WriteRecordRow,
            r#"
SELECT w.id AS "id!", w.call_id, w.expected_hash, w.path, w.repository_root, w.resulting_hash,
       w.status, w.turn_position
FROM session_write w
WHERE w.session_id = ?
ORDER BY w.turn_position, w.id
"#,
            session_id
        )
        .fetch_all(pool)
        .await
        .map_err(|source| SessionError::QueryContext {
            operation: "load persistent writes",
            source,
        })?;

        rows.into_iter().map(Self::into_record).collect()
    }

    fn into_record(self) -> Result<WriteRecord, SessionError> {
        let repository_root = PathBuf::from(OsString::from_vec(self.repository_root));
        let status = match self.status.as_str() {
            "pending" => WriteStatus::Pending,
            "applied" => WriteStatus::Applied,
            "failed" => WriteStatus::Failed,
            _ => {
                return Err(SessionError::InvalidData {
                    reason: "invalid persistent write status".to_string(),
                });
            }
        };

        Ok(WriteRecord {
            call_id: self.call_id,
            expected_hash: self.expected_hash,
            id: self.id,
            path: self.path,
            repository_root,
            resulting_hash: self.resulting_hash,
            status,
            turn_position: self.turn_position,
        })
    }
}

pub(crate) struct WriteJournal {
    pub(crate) owner_token: Vec<u8>,
    pub(crate) pool: SqlitePool,
    pub(crate) session_id: String,
    pub(crate) timestamp_source: Arc<dyn TimestampSource>,
    pub(crate) turn_position: i64,
}

impl WriteJournal {
    pub(crate) async fn intent(
        &self,
        call_id: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        let root = root.as_os_str().as_bytes();
        let expected_hash = expected.map(content_hash);
        let resulting_hash = content_hash(resulting);
        let mut transaction =
            self.pool
                .begin()
                .await
                .map_err(|source| SessionError::QueryContext {
                    operation: "begin write intent",
                    source,
                })?;
        let now = self.timestamp_source.now_timestamp_seconds();
        let row = sqlx::query!(
            r#"
INSERT INTO session_write (
    session_id, turn_position, call_id, repository_root, path,
    expected_hash, resulting_hash, status
)
SELECT session_id, turn_position, ?, ?, ?, ?, ?, 'pending'
FROM session_turn
WHERE session_id = ? AND turn_position = ? AND owner_token = ?
  AND status = 'running' AND lease_expires_at > ?
RETURNING id AS "id!"
"#,
            call_id,
            root,
            path,
            expected_hash,
            resulting_hash,
            self.session_id,
            self.turn_position,
            self.owner_token,
            now
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|source| SessionError::QueryContext {
            operation: "persist write intent",
            source,
        })?
        .ok_or_else(|| SessionError::OwnershipLost {
            id: self.session_id.clone(),
            turn_position: self.turn_position,
        })?;

        // `RETURNING` can yield an ID before SQLite reports commit errors.
        transaction
            .commit()
            .await
            .map_err(|source| SessionError::QueryContext {
                operation: "commit write intent",
                source,
            })?;

        Ok(row.id)
    }

    pub(crate) async fn finish(&self, id: i64, applied: bool) -> Result<(), SessionError> {
        let status = if applied { "applied" } else { "failed" };
        sqlx::query!(
            "UPDATE session_write SET status = ? WHERE id = ? AND session_id = ?",
            status,
            id,
            self.session_id
        )
        .execute(&self.pool)
        .await
        .map_err(|source| SessionError::QueryContext {
            operation: "persist write outcome",
            source,
        })?;

        Ok(())
    }
}

pub(crate) fn content_hash(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn serialize_repository_root<S: serde::Serializer>(
    root: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(root.as_os_str().as_bytes())
}

#[cfg(test)]
#[path = "write_journal_test.rs"]
mod tests;
