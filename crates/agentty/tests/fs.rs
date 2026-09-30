//! Public filesystem prefix-read contract used for bounded configuration input.

use agentty::infra::fs::{FsClient, FsError, RealFsClient};
use tempfile::tempdir;

#[tokio::test]
async fn prefix_reads_bound_large_files_and_handle_exact_or_early_eof() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let file = directory.path().join("configuration");
    let contents = vec![b'x'; 1_048_576];
    std::fs::write(&file, &contents).expect("large file");
    let empty = directory.path().join("empty");
    std::fs::write(&empty, []).expect("empty file");

    // Act / Assert
    for (limit, expected) in [
        (0, 0),
        (4, 4),
        (65_537, 65_537),
        (1_048_576, 1_048_576),
        (1_048_577, 1_048_576),
    ] {
        let bytes = RealFsClient
            .read_file_prefix(file.clone(), limit)
            .await
            .expect("prefix");
        assert_eq!(bytes, contents[..expected]);
    }
    assert_eq!(
        RealFsClient
            .read_file_prefix(empty, 65_537)
            .await
            .expect("empty prefix"),
        Vec::<u8>::new()
    );
}

#[tokio::test]
async fn prefix_reads_preserve_open_and_read_errors() {
    // Arrange
    let directory = tempdir().expect("temporary directory");

    // Act
    let missing = RealFsClient
        .read_file_prefix(directory.path().join("missing"), 65_537)
        .await;
    let unreadable = RealFsClient
        .read_file_prefix(directory.path().to_path_buf(), 65_537)
        .await;

    // Assert
    assert!(
        matches!(missing, Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(unreadable.is_err());
}
