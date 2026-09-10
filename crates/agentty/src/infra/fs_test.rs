use tempfile::tempdir;

use super::RealFsClient;
use crate::infra::fs::FsClient;

#[tokio::test]
async fn startup_cleanup_scans_worktrees_without_following_links() {
    // Arrange
    let root = tempdir().expect("worktrees");
    let worktree = root.path().join("session");
    let archive = worktree.join(".agentty-replay-orphan");
    std::fs::create_dir_all(&archive).expect("archive");
    std::fs::write(archive.join(".gitignore"), "*\n").expect("marker");
    std::fs::write(archive.join("history.md"), "private").expect("history");
    let elsewhere = tempdir().expect("outside root");
    let linked_archive = elsewhere.path().join(".agentty-replay-keep");
    std::fs::create_dir(&linked_archive).expect("archive");
    std::fs::write(linked_archive.join(".gitignore"), "*\n").expect("marker");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("linked"))
        .expect("worktree symlink");
    let file = root.path().join("file");
    std::fs::write(&file, "preserve").expect("file");

    // Act
    RealFsClient
        .cleanup_agent_artifacts(root.path().to_owned())
        .await
        .expect("cleanup");
    let missing = RealFsClient
        .cleanup_agent_artifacts(root.path().join("missing"))
        .await;
    let invalid = RealFsClient.cleanup_agent_artifacts(file.clone()).await;

    // Assert
    assert_eq!(
        std::fs::read_to_string(archive.join("history.md")).expect("preserved history"),
        "private"
    );
    assert!(linked_archive.exists());
    assert!(file.exists());
    assert!(missing.is_ok());
    assert!(invalid.is_err());
}

/// Verifies `RealFsClient::read_file()` reads bytes through the async
/// filesystem adapter.
#[tokio::test]
async fn test_real_fs_client_read_file_reads_existing_file() {
    // Arrange
    let temp_dir = tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("example.txt");
    tokio::fs::write(&file_path, b"hello world")
        .await
        .expect("write file");
    let fs_client = RealFsClient;

    // Act
    let content = fs_client
        .read_file(file_path)
        .await
        .expect("read existing file");

    // Assert
    assert_eq!(content, b"hello world");
}

/// Verifies `RealFsClient::read_file()` surfaces read failures through the
/// async boundary.
#[tokio::test]
async fn test_real_fs_client_read_file_returns_error_for_missing_file() {
    // Arrange
    let temp_dir = tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("missing.txt");
    let fs_client = RealFsClient;

    // Act
    let error = fs_client
        .read_file(file_path)
        .await
        .expect_err("missing file should error");

    // Assert
    let message = error.to_string();
    assert!(message.contains("No such file") || message.contains("cannot find the path"));
}

/// Verifies `RealFsClient::is_file()` distinguishes files from
/// directories.
#[tokio::test]
async fn test_real_fs_client_is_file_returns_true_only_for_regular_files() {
    // Arrange
    let temp_dir = tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("example.txt");
    tokio::fs::write(&file_path, b"hello world")
        .await
        .expect("write file");
    let fs_client = RealFsClient;

    // Act
    let file_exists = fs_client.is_file(file_path);
    let directory_exists = fs_client.is_file(temp_dir.path().to_path_buf());

    // Assert
    assert!(file_exists);
    assert!(!directory_exists);
}

/// Verifies `RealFsClient::canonicalize()` resolves files to absolute
/// paths through the async filesystem boundary.
#[tokio::test]
async fn test_real_fs_client_canonicalize_returns_absolute_file_path() {
    // Arrange
    let temp_dir = tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("example.txt");
    tokio::fs::write(&file_path, b"hello world")
        .await
        .expect("write file");
    let fs_client = RealFsClient;

    // Act
    let canonicalized_path = fs_client
        .canonicalize(file_path.clone())
        .await
        .expect("canonicalize file");

    // Assert
    assert_eq!(
        canonicalized_path,
        std::fs::canonicalize(file_path).expect("std canonicalize should succeed")
    );
}

/// Verifies `RealFsClient::exists()` reports any existing filesystem
/// entry, including directories.
#[tokio::test]
async fn test_real_fs_client_exists_returns_true_for_directories() {
    // Arrange
    let temp_dir = tempdir().expect("create temp dir");
    let fs_client = RealFsClient;

    // Act
    let directory_exists = fs_client.exists(temp_dir.path().to_path_buf());
    let missing_path_exists = fs_client.exists(temp_dir.path().join("missing"));

    // Assert
    assert!(directory_exists);
    assert!(!missing_path_exists);
}
