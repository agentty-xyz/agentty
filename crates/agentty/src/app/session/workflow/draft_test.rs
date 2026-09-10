use tempfile::tempdir;

use super::{
    load_staged_draft_attachments, staged_draft_attachment_path, store_staged_draft_attachments,
};
use crate::domain::session::SESSION_DATA_DIR;
use crate::domain::turn_prompt::TurnPromptAttachment;
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
async fn test_store_staged_draft_attachments_empty_slice_removes_metadata_directory() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let fs_client = RealFsClient;
    let session_root = temp_dir.path().join("session-1");
    let session_data_dir = session_root.join(SESSION_DATA_DIR);
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
    assert!(session_root.exists());
    assert!(!session_data_dir.exists());
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

#[tokio::test]
/// Ensures clearing staged attachments preserves unrelated files stored
/// under the session root.
async fn test_store_staged_draft_attachments_empty_slice_preserves_other_session_files() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let fs_client = RealFsClient;
    let session_root = temp_dir.path().join("session-1");
    let session_data_dir = session_root.join(SESSION_DATA_DIR);
    let unrelated_file = session_root.join("notes.txt");
    tokio::fs::create_dir_all(&session_data_dir)
        .await
        .expect("failed to create session data dir");
    tokio::fs::write(&unrelated_file, b"keep me")
        .await
        .expect("failed to seed unrelated file");
    let attachment_path = staged_draft_attachment_path(temp_dir.path(), "session-1");
    tokio::fs::write(&attachment_path, b"[]")
        .await
        .expect("failed to seed attachment file");

    // Act
    store_staged_draft_attachments(&fs_client, temp_dir.path(), "session-1", &[])
        .await
        .expect("failed to clear attachments");

    // Assert
    assert!(unrelated_file.exists());
    assert!(!session_data_dir.exists());
}
