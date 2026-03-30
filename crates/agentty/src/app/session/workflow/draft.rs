//! Draft-session persistence helpers for staged attachment metadata.

use std::path::{Path, PathBuf};

use crate::domain::session::SESSION_DATA_DIR;
use crate::infra::channel::TurnPromptAttachment;
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
    let attachment_bytes = match fs_client.read_file(attachment_path).await {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };

    serde_json::from_slice(&attachment_bytes).unwrap_or_default()
}

/// Persists the staged draft-session attachment list for one session.
///
/// An empty slice removes the metadata file entirely. When attachments are
/// present, the session metadata directory is created before the JSON payload
/// is written so draft staging still works after external cleanup removed the
/// folder.
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
        return fs_client.remove_file(attachment_path).await;
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
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::infra::fs::RealFsClient;

    #[tokio::test]
    async fn test_store_and_load_staged_draft_attachments_round_trip() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let fs_client = RealFsClient;
        let session_data_dir = temp_dir.path().join("session-1").join(SESSION_DATA_DIR);
        tokio::fs::create_dir_all(&session_data_dir)
            .await
            .expect("failed to create session data dir");
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: temp_dir.path().join("image-001.png"),
        }];

        // Act
        store_staged_draft_attachments(&fs_client, temp_dir.path(), "session-1", &attachments)
            .await
            .expect("failed to store attachments");
        let loaded_attachments =
            load_staged_draft_attachments(&fs_client, temp_dir.path(), "session-1").await;

        // Assert
        assert_eq!(loaded_attachments, attachments);
    }

    #[tokio::test]
    async fn test_store_staged_draft_attachments_empty_slice_removes_metadata_file() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let fs_client = RealFsClient;
        let session_data_dir = temp_dir.path().join("session-1").join(SESSION_DATA_DIR);
        tokio::fs::create_dir_all(&session_data_dir)
            .await
            .expect("failed to create session data dir");
        let attachment_path = staged_draft_attachment_path(temp_dir.path(), "session-1");
        tokio::fs::write(&attachment_path, b"[]")
            .await
            .expect("failed to seed attachment file");

        // Act
        store_staged_draft_attachments(&fs_client, temp_dir.path(), "session-1", &[])
            .await
            .expect("failed to clear attachments");

        // Assert
        assert!(!attachment_path.exists());
    }

    #[tokio::test]
    /// Ensures attachment staging recreates the metadata directory before
    /// writing JSON state.
    async fn test_store_staged_draft_attachments_creates_missing_metadata_directory() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let fs_client = RealFsClient;
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: temp_dir.path().join("image-001.png"),
        }];
        let attachment_path = staged_draft_attachment_path(temp_dir.path(), "session-1");

        // Act
        store_staged_draft_attachments(&fs_client, temp_dir.path(), "session-1", &attachments)
            .await
            .expect("failed to store attachments");

        // Assert
        assert!(attachment_path.exists());
        let loaded_attachments =
            load_staged_draft_attachments(&fs_client, temp_dir.path(), "session-1").await;
        assert_eq!(loaded_attachments, attachments);
    }
}
