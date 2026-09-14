//! Write-ahead records independent of completed conversation history.

use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

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

pub(crate) fn content_hash(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn serialize_repository_root<S: serde::Serializer>(
    root: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(root.as_os_str().as_bytes())
}
