use super::super::SyncSessionStartError;
use super::support::create_passthrough_mock_fs_client;
use crate::app::SessionManager;

#[test]
fn test_session_title_from_commit_message() {
    // Arrange
    let commit_message = "Refine merge flow\n\n- Update title handling";

    // Act
    let title = SessionManager::session_title_from_commit_message(commit_message);

    // Assert
    assert_eq!(title, "Refine merge flow");
}

#[test]
fn test_session_title_from_commit_message_skips_blank_prefix() {
    // Arrange
    let commit_message = "\n\nRefine merge flow\n\n- Update title handling";

    // Act
    let title = SessionManager::session_title_from_commit_message(commit_message);

    // Assert
    assert_eq!(title, "Refine merge flow");
}

#[test]
fn test_session_title_from_commit_message_empty_uses_fallback() {
    // Arrange
    let commit_message = "  \n";

    // Act
    let title = SessionManager::session_title_from_commit_message(commit_message);

    // Assert
    assert_eq!(title, "Apply session updates");
}

#[test]
fn test_detail_message_for_uncommitted_changes_uses_sentence_lines() {
    // Arrange
    let sync_error = SyncSessionStartError::MainHasUncommittedChanges {
        default_branch: "main".to_string(),
    };

    // Act
    let detail_message = sync_error.detail_message();

    // Assert
    assert_eq!(
        detail_message,
        "Sync cannot run while `main` has uncommitted changes.\nCommit or stash changes in \
         `main`, then try again."
    );
}

#[tokio::test]
async fn test_conflicted_file_fingerprint_changes_with_file_content() {
    // Arrange
    let fs_client = create_passthrough_mock_fs_client();
    let temp_dir = std::env::temp_dir().join(format!(
        "agentty_fp_content_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    let file_path = temp_dir.join("conflict.rs");
    let files = vec!["conflict.rs".to_string()];

    // Act
    std::fs::write(&file_path, "<<<<<<< HEAD\nfoo\n=======\nbar\n>>>>>>>")
        .expect("write initial content");
    let fingerprint_before =
        SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
    std::fs::write(
        &file_path,
        "<<<<<<< HEAD\nfoo_patched\n=======\nbar\n>>>>>>>",
    )
    .expect("write patched content");
    let fingerprint_after =
        SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

    // Assert — partial progress changes the fingerprint
    assert_ne!(fingerprint_before, fingerprint_after);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_conflicted_file_fingerprint_stable_for_unchanged_content() {
    // Arrange
    let fs_client = create_passthrough_mock_fs_client();
    let temp_dir = std::env::temp_dir().join(format!(
        "agentty_fp_stable_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    std::fs::write(temp_dir.join("conflict.rs"), "same content").expect("write file");
    let files = vec!["conflict.rs".to_string()];

    // Act
    let fingerprint_a =
        SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
    let fingerprint_b =
        SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

    // Assert — identical content produces identical fingerprint
    assert_eq!(fingerprint_a, fingerprint_b);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_conflicted_file_fingerprint_order_independent() {
    // Arrange
    let fs_client = create_passthrough_mock_fs_client();
    let temp_dir = std::env::temp_dir().join(format!(
        "agentty_fp_order_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    std::fs::write(temp_dir.join("a.rs"), "content a").expect("write a.rs");
    std::fs::write(temp_dir.join("b.rs"), "content b").expect("write b.rs");

    // Act
    let fingerprint_ab = SessionManager::conflicted_file_fingerprint(
        &fs_client,
        &temp_dir,
        &["a.rs".to_string(), "b.rs".to_string()],
    )
    .await;
    let fingerprint_ba = SessionManager::conflicted_file_fingerprint(
        &fs_client,
        &temp_dir,
        &["b.rs".to_string(), "a.rs".to_string()],
    )
    .await;

    // Assert — order of file list does not affect the fingerprint
    assert_eq!(fingerprint_ab, fingerprint_ba);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_conflicted_file_fingerprint_missing_file_is_stable() {
    // Arrange — reference a file that does not exist on disk
    let fs_client = create_passthrough_mock_fs_client();
    let temp_dir = std::env::temp_dir().join(format!(
        "agentty_fp_missing_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    let files = vec!["nonexistent.rs".to_string()];

    // Act — should not panic; missing files are silently skipped
    let fingerprint_a =
        SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
    let fingerprint_b =
        SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

    // Assert — deterministic even when file is absent
    assert_eq!(fingerprint_a, fingerprint_b);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_format_conflicted_file_list_returns_bulleted_lines() {
    // Arrange
    let conflicted_files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];

    // Act
    let summary = SessionManager::format_conflicted_file_list(&conflicted_files);

    // Assert
    assert_eq!(summary, "- src/main.rs\n- src/lib.rs");
}
