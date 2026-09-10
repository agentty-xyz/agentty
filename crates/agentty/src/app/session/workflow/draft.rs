//! Draft-session persistence helpers for staged attachment metadata.

use std::path::{Path, PathBuf};

use crate::domain::session::SESSION_DATA_DIR;
use crate::domain::turn_prompt::TurnPromptAttachment;
use crate::infra::fs::{FsClient, FsError};

/// Metadata filename used for staged draft-session image attachments.
const STAGED_DRAFT_ATTACHMENT_FILE: &str = "draft_attachment.json";

/// Returns the metadata file path storing staged draft-session attachments for
/// one session.
pub(super) fn staged_draft_attachment_path(base: &Path, session_id: &str) -> PathBuf {
    base.join(session_id)
        .join(SESSION_DATA_DIR)
        .join(STAGED_DRAFT_ATTACHMENT_FILE)
}

/// Loads persisted staged draft-session attachments for one session.
///
/// Invalid or missing metadata is treated as empty so stale files do not
/// block session loading.
pub(super) async fn load_staged_draft_attachments(
    fs_client: &dyn FsClient,
    base: &Path,
    session_id: &str,
) -> Vec<TurnPromptAttachment> {
    let attachment_path = staged_draft_attachment_path(base, session_id);
    let Ok(attachment_bytes) = fs_client.read_file(attachment_path).await else {
        return Vec::new();
    };

    serde_json::from_slice(&attachment_bytes).unwrap_or_default()
}

/// Persists the staged draft-session attachment list for one session.
///
/// An empty slice removes the metadata file and its dedicated
/// `SESSION_DATA_DIR` directory. When attachments are present, the session
/// metadata directory is created before the JSON payload is written so draft
/// staging still works after external cleanup removed the folder.
///
/// # Errors
/// Returns an error if the attachment metadata cannot be serialized or
/// written.
pub(super) async fn store_staged_draft_attachments(
    fs_client: &dyn FsClient,
    base: &Path,
    session_id: &str,
    attachments: &[TurnPromptAttachment],
) -> Result<(), FsError> {
    let attachment_path = staged_draft_attachment_path(base, session_id);
    if attachments.is_empty() {
        fs_client.remove_file(attachment_path).await?;

        let session_data_dir = base.join(session_id).join(SESSION_DATA_DIR);
        if fs_client.is_dir(session_data_dir.clone()) {
            fs_client.remove_dir_all(session_data_dir).await?;
        }

        return Ok(());
    }

    let Some(parent_dir) = attachment_path.parent() else {
        return Err(FsError::Io(std::io::Error::other(
            "staged draft attachment path is missing a parent directory",
        )));
    };
    fs_client.create_dir_all(parent_dir.to_path_buf()).await?;

    let serialized_attachments = serde_json::to_vec(attachments)
        .map_err(|error| FsError::Io(std::io::Error::other(error)))?;

    fs_client
        .write_file(attachment_path, serialized_attachments)
        .await
}

#[cfg(test)]
#[path = "draft_test.rs"]
mod tests;
