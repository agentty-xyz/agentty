use std::path::PathBuf;

use super::{
    ClipboardError, ClipboardPayload, build_clipboard_image_path,
    canonicalize_persisted_image_path, clipboard_image_directory, is_png_path,
    normalize_clipboard_image_error, persist_clipboard_payload,
};
use crate::infra::clock::Clock;
use crate::infra::{fs, home};

struct FixedClock {
    system_time: std::time::SystemTime,
}

impl Clock for FixedClock {
    fn now_instant(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    fn now_system_time(&self) -> std::time::SystemTime {
        self.system_time
    }
}

#[test]
fn test_clipboard_image_directory_uses_agentty_tmp_path_for_session_id() {
    // Arrange
    let session_id = "session-123";
    let agentty_root = home::agentty_home();

    // Act
    let image_directory =
        clipboard_image_directory(session_id).expect("image directory should resolve");

    // Assert
    assert_eq!(
        image_directory,
        agentty_root.join("tmp").join("session-123").join("images")
    );
}

#[test]
fn test_build_clipboard_image_path_uses_png_extension_in_images_directory() {
    // Arrange
    let session_id = "session-123";
    let expected_directory = home::agentty_home()
        .join("tmp")
        .join("session-123")
        .join("images");
    let clock = FixedClock {
        system_time: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(42),
    };

    // Act
    let image_path =
        build_clipboard_image_path(session_id, 2, &clock).expect("image path should resolve");

    // Assert
    assert_eq!(image_path.parent(), Some(expected_directory.as_path()));
    assert!(
        image_path
            .file_name()
            .is_some_and(|name| { name.to_string_lossy() == "image-002-42.png" })
    );
}

#[test]
fn test_build_clipboard_image_path_rejects_pre_epoch_clock_values() {
    // Arrange
    let session_id = "session-123";
    let clock = FixedClock {
        system_time: std::time::SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(1),
    };

    // Act
    let result = build_clipboard_image_path(session_id, 2, &clock);

    // Assert
    assert!(matches!(result, Err(ClipboardError::SystemClock(_))));
}

#[test]
fn test_clipboard_image_directory_rejects_empty_session_id() {
    // Arrange
    let session_id = "";

    // Act
    let result = clipboard_image_directory(session_id);

    // Assert
    assert!(matches!(result, Err(ClipboardError::EmptySessionId)));
}

#[test]
fn test_is_png_path_accepts_png_extension_case_insensitively() {
    // Arrange
    let lowercase_path = PathBuf::from("/tmp/image.png");
    let uppercase_path = PathBuf::from("/tmp/image.PNG");

    // Act
    let accepts_lowercase = is_png_path(&lowercase_path);
    let accepts_uppercase = is_png_path(&uppercase_path);

    // Assert
    assert!(accepts_lowercase);
    assert!(accepts_uppercase);
}

#[test]
fn test_is_png_path_rejects_non_png_paths() {
    // Arrange
    let jpeg_path = PathBuf::from("/tmp/image.jpeg");
    let extensionless_path = PathBuf::from("/tmp/image");

    // Act
    let accepts_jpeg = is_png_path(&jpeg_path);
    let accepts_extensionless = is_png_path(&extensionless_path);

    // Assert
    assert!(!accepts_jpeg);
    assert!(!accepts_extensionless);
}

#[tokio::test]
async fn test_canonicalize_persisted_image_path_returns_absolute_file_path() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("temp dir should exist");
    let image_path = temp_dir.path().join("image.png");
    std::fs::write(&image_path, b"png").expect("image file should be written");
    let fs_client = fs::RealFsClient;

    // Act
    let canonicalized_path = canonicalize_persisted_image_path(&fs_client, &image_path)
        .await
        .expect("image path should canonicalize");

    // Assert
    assert_eq!(
        canonicalized_path,
        std::fs::canonicalize(&image_path).expect("std canonicalize should succeed")
    );
}

/// Verifies clipboard payload persistence writes encoded PNG bytes through
/// the filesystem boundary.
#[tokio::test]
async fn test_persist_clipboard_payload_writes_png_bytes_with_fs_client() {
    // Arrange
    let image_output_path = PathBuf::from("/tmp/agentty/image.png");
    let expected_directory = image_output_path
        .parent()
        .expect("image path should have a parent")
        .to_path_buf();
    let mut fs_client = fs::MockFsClient::new();
    fs_client
        .expect_create_dir_all()
        .once()
        .returning(move |path| {
            let expected_directory = expected_directory.clone();
            Box::pin(async move {
                assert_eq!(path, expected_directory);

                Ok(())
            })
        });
    let expected_write_path = image_output_path.clone();
    fs_client
        .expect_write_file()
        .once()
        .returning(move |path, contents| {
            let image_output_path = expected_write_path.clone();
            Box::pin(async move {
                assert_eq!(path, image_output_path);
                assert_eq!(contents, b"png-bytes");

                Ok(())
            })
        });

    // Act
    let result = persist_clipboard_payload(
        &fs_client,
        image_output_path.as_path(),
        ClipboardPayload::EncodedPng(b"png-bytes".to_vec()),
    )
    .await;

    // Assert
    assert!(result.is_ok());
}

/// Verifies clipboard payload persistence rejects missing PNG source paths
/// before attempting a filesystem read.
#[tokio::test]
async fn test_persist_clipboard_payload_rejects_missing_png_source_path() {
    // Arrange
    let image_output_path = PathBuf::from("/tmp/agentty/image.png");
    let source_image_path = PathBuf::from("/tmp/source.png");
    let expected_directory = image_output_path
        .parent()
        .expect("image path should have a parent")
        .to_path_buf();
    let mut fs_client = fs::MockFsClient::new();
    fs_client
        .expect_create_dir_all()
        .once()
        .returning(move |path| {
            let expected_directory = expected_directory.clone();
            Box::pin(async move {
                assert_eq!(path, expected_directory);

                Ok(())
            })
        });
    fs_client.expect_is_file().once().returning(|_| false);
    fs_client.expect_read_file().times(0);
    fs_client.expect_write_file().times(0);

    // Act
    let result = persist_clipboard_payload(
        &fs_client,
        image_output_path.as_path(),
        ClipboardPayload::ExistingPngPath(source_image_path),
    )
    .await;

    // Assert
    assert!(matches!(result, Err(ClipboardError::PngPathNotFound)));
}

#[test]
fn test_normalize_clipboard_image_error_maps_unavailable_to_actionable_status() {
    // Arrange
    let error = ClipboardError::Unavailable {
        reason: "permission denied".to_string(),
    };

    // Act
    let normalized_error = normalize_clipboard_image_error(&error);

    // Assert
    assert_eq!(
        normalized_error,
        "Clipboard is unavailable. Try again after granting clipboard access."
    );
}

#[test]
fn test_normalize_clipboard_image_error_maps_missing_wl_paste_to_package_status() {
    // Arrange
    let error = ClipboardError::Unavailable {
        reason: "Clipboard backend is unavailable: Wayland clipboard image paste requires \
                 `wl-paste`; install the `wl-clipboard` package"
            .to_string(),
    };

    // Act
    let normalized_error = normalize_clipboard_image_error(&error);

    // Assert
    assert_eq!(
        normalized_error,
        "Wayland clipboard image paste requires wl-paste. Install the wl-clipboard package."
    );
}

#[test]
fn test_normalize_clipboard_image_error_maps_no_image_to_short_status() {
    // Arrange
    let error = ClipboardError::NoImage;

    // Act
    let normalized_error = normalize_clipboard_image_error(&error);

    // Assert
    assert_eq!(normalized_error, "Clipboard does not contain an image.");
}

#[test]
fn test_normalize_clipboard_image_error_maps_encode_failure_to_persist_status() {
    // Arrange
    let error = ClipboardError::ImageEncode(image::ImageError::IoError(std::io::Error::other(
        "encoder failed",
    )));

    // Act
    let normalized_error = normalize_clipboard_image_error(&error);

    // Assert
    assert_eq!(
        normalized_error,
        "Failed to persist pasted image from the clipboard."
    );
}

#[tokio::test]
async fn test_normalize_clipboard_image_error_maps_task_join_to_capture_status() {
    // Arrange
    let handle = tokio::spawn(std::future::pending::<()>());
    handle.abort();
    let error = ClipboardError::TaskJoin(handle.await.expect_err("should be cancelled"));

    // Act
    let normalized_error = normalize_clipboard_image_error(&error);

    // Assert
    assert_eq!(normalized_error, "Clipboard image capture failed.");
}
