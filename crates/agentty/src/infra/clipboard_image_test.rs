use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_clipboard::{Clipboard, ClipboardBackend, RgbaImageData};

use super::{
    ClipboardError, ClipboardImageClient, ClipboardOpener, ClipboardPayload,
    RealClipboardImageClient, build_clipboard_image_path, canonicalize_persisted_image_path,
    clipboard_image_directory, encode_clipboard_image_to_png, is_png_path,
    normalize_clipboard_image_error, persist_clipboard_payload, read_clipboard_payload,
};
use crate::infra::clock::Clock;
use crate::infra::{fs, home};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

struct FixedClock {
    system_time: std::time::SystemTime,
}

/// In-memory clipboard source; `None` fields report missing content.
#[derive(Clone, Default)]
struct FakeClipboardBackend {
    file_list: Option<Vec<PathBuf>>,
    image: Option<RgbaImageData>,
    text: Option<&'static str>,
}

impl FakeClipboardBackend {
    /// Returns an opener that yields a fresh clipboard over this content.
    fn opener(self) -> ClipboardOpener {
        Arc::new(move || Ok(Clipboard::from_backend(Box::new(self.clone()))))
    }
}

impl ClipboardBackend for FakeClipboardBackend {
    fn read_text(&mut self) -> Result<String, ag_clipboard::ClipboardError> {
        self.text
            .map(str::to_string)
            .ok_or(ag_clipboard::ClipboardError::ContentUnavailable)
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ag_clipboard::ClipboardError> {
        self.file_list
            .clone()
            .ok_or(ag_clipboard::ClipboardError::ContentUnavailable)
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ag_clipboard::ClipboardError> {
        self.image
            .clone()
            .ok_or(ag_clipboard::ClipboardError::ContentUnavailable)
    }
}

/// Filesystem operation that a persistence test makes fail.
#[derive(Clone, Copy, Debug)]
enum FailingFsOperation {
    CreateDirectory,
    ReadSource,
    WriteImage,
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
    assert!(image_path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();

        name.starts_with("image-002-42-") && name.ends_with(".png")
    }));
}

#[test]
fn test_build_clipboard_image_path_stays_unique_within_one_clock_millisecond() {
    // Arrange
    let session_id = "session-123";
    let clock = FixedClock {
        system_time: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(42),
    };

    // Act
    let first_path =
        build_clipboard_image_path(session_id, 1, &clock).expect("image path should resolve");
    let second_path =
        build_clipboard_image_path(session_id, 1, &clock).expect("image path should resolve");

    // Assert
    assert_ne!(first_path, second_path);
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

#[test]
fn test_normalize_clipboard_image_error_maps_path_and_session_errors() {
    // Arrange
    let cases = [
        (
            ClipboardError::PngPathNotFound,
            "Clipboard PNG path does not exist.",
        ),
        (
            ClipboardError::EmptySessionId,
            "Session id is missing for clipboard image temp storage",
        ),
    ];

    for (error, expected_status) in cases {
        // Act
        let normalized_error = normalize_clipboard_image_error(&error);

        // Assert
        assert_eq!(normalized_error, expected_status);
    }
}

/// Verifies the production client copies a PNG file named in the clipboard
/// file list into the session image directory.
#[tokio::test]
async fn test_persist_clipboard_image_copies_png_from_clipboard_file_list() {
    // Arrange
    let source_image_path = PathBuf::from("/tmp/copied.png");
    let resolved_image_path = PathBuf::from("/resolved/image.png");
    let expected_directory =
        clipboard_image_directory("session-123").expect("image directory should resolve");
    let clipboard = FakeClipboardBackend {
        file_list: Some(vec![
            PathBuf::from("/tmp/notes.txt"),
            source_image_path.clone(),
        ]),
        ..FakeClipboardBackend::default()
    };
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
    let expected_source_path = source_image_path.clone();
    fs_client
        .expect_is_file()
        .once()
        .returning(move |path| path == expected_source_path);
    fs_client.expect_read_file().once().returning(move |path| {
        let source_image_path = source_image_path.clone();
        Box::pin(async move {
            assert_eq!(path, source_image_path);

            Ok(b"copied-png".to_vec())
        })
    });
    fs_client
        .expect_write_file()
        .once()
        .returning(|_, contents| {
            Box::pin(async move {
                assert_eq!(contents, b"copied-png");

                Ok(())
            })
        });
    let canonical_path = resolved_image_path.clone();
    fs_client.expect_canonicalize().once().returning(move |_| {
        let canonical_path = canonical_path.clone();
        Box::pin(async move { Ok(canonical_path) })
    });
    let client = RealClipboardImageClient::with_clipboard_opener(
        Arc::new(FixedClock {
            system_time: std::time::SystemTime::UNIX_EPOCH,
        }),
        Arc::new(fs_client),
        clipboard.opener(),
    );

    // Act
    let persisted_image = client
        .persist_clipboard_image("session-123".to_string(), 1)
        .await
        .expect("copied PNG should persist");

    // Assert
    assert_eq!(persisted_image.local_image_path, resolved_image_path);
}

/// Verifies image data wins over clipboard text when no copied PNG file is
/// present.
#[tokio::test]
async fn test_read_clipboard_payload_encodes_image_without_copied_png_file() {
    // Arrange
    let clipboard = FakeClipboardBackend {
        file_list: Some(vec![PathBuf::from("/tmp/notes.txt")]),
        image: Some(RgbaImageData {
            height: 1,
            rgba_bytes: vec![255, 0, 0, 255],
            width: 1,
        }),
        text: Some("/tmp/ignored.png"),
    };

    // Act
    let payload = read_clipboard_payload(clipboard.opener()).await;

    // Assert
    assert!(matches!(
        payload,
        Ok(ClipboardPayload::EncodedPng(bytes)) if bytes.starts_with(PNG_SIGNATURE)
    ));
}

/// Verifies clipboard text naming a PNG path is the last-resort payload.
#[tokio::test]
async fn test_read_clipboard_payload_uses_png_path_from_text() {
    // Arrange
    let clipboard = FakeClipboardBackend {
        text: Some("  /tmp/screenshot.PNG\n"),
        ..FakeClipboardBackend::default()
    };

    // Act
    let payload = read_clipboard_payload(clipboard.opener()).await;

    // Assert
    assert!(matches!(
        payload,
        Ok(ClipboardPayload::ExistingPngPath(path)) if path == Path::new("/tmp/screenshot.PNG")
    ));
}

/// Verifies missing text and non-PNG text both report that no image exists.
#[tokio::test]
async fn test_read_clipboard_payload_reports_no_image_without_png_content() {
    // Arrange
    let cases = [
        FakeClipboardBackend::default(),
        FakeClipboardBackend {
            text: Some("plain text"),
            ..FakeClipboardBackend::default()
        },
    ];

    for clipboard in cases {
        // Act
        let payload = read_clipboard_payload(clipboard.opener()).await;

        // Assert
        assert!(matches!(payload, Err(ClipboardError::NoImage)));
    }
}

/// Verifies clipboard open failures keep the backend reason.
#[tokio::test]
async fn test_read_clipboard_payload_reports_unavailable_clipboard() {
    // Arrange
    let open_clipboard: ClipboardOpener = Arc::new(|| {
        Err(ag_clipboard::ClipboardError::Unavailable {
            reason: "no display".to_string(),
        })
    });

    // Act
    let payload = read_clipboard_payload(open_clipboard).await;

    // Assert
    assert!(matches!(
        payload,
        Err(ClipboardError::Unavailable { reason })
            if reason == "Clipboard backend is unavailable: no display"
    ));
}

/// Verifies clipboard images the PNG encoder rejects surface as encode
/// failures.
#[tokio::test]
async fn test_read_clipboard_payload_reports_unencodable_image() {
    // Arrange
    let clipboard = FakeClipboardBackend {
        image: Some(RgbaImageData {
            height: 1,
            rgba_bytes: Vec::new(),
            width: 0,
        }),
        ..FakeClipboardBackend::default()
    };

    // Act
    let payload = read_clipboard_payload(clipboard.opener()).await;

    // Assert
    assert!(matches!(payload, Err(ClipboardError::ImageEncode(_))));
}

#[test]
fn test_encode_clipboard_image_to_png_writes_png_signature() {
    // Arrange
    let rgba_bytes = [0, 255, 0, 255];

    // Act
    let encoded_png =
        encode_clipboard_image_to_png(&rgba_bytes, 1, 1).expect("one pixel should encode");

    // Assert
    assert!(encoded_png.starts_with(PNG_SIGNATURE));
}

/// Verifies each filesystem failure during persistence keeps its context.
#[tokio::test]
async fn test_persist_clipboard_payload_reports_filesystem_failures() {
    // Arrange
    let cases = [
        (
            FailingFsOperation::CreateDirectory,
            "Failed to create clipboard image directory",
        ),
        (
            FailingFsOperation::ReadSource,
            "Failed to read clipboard PNG file",
        ),
        (
            FailingFsOperation::WriteImage,
            "Failed to write pasted image PNG",
        ),
    ];

    for (failing_operation, expected_context) in cases {
        let fs_client = failing_fs_client(failing_operation);

        // Act
        let result = persist_clipboard_payload(
            &fs_client,
            PathBuf::from("/tmp/agentty/image.png").as_path(),
            ClipboardPayload::ExistingPngPath(PathBuf::from("/tmp/source.png")),
        )
        .await;

        // Assert
        assert!(
            matches!(
                &result,
                Err(ClipboardError::Persist { context, .. }) if *context == expected_context
            ),
            "{failing_operation:?} should fail with `{expected_context}`, got {result:?}"
        );
    }
}

/// Builds a filesystem mock whose operations succeed up to
/// `failing_operation`, which fails with an I/O error.
fn failing_fs_client(failing_operation: FailingFsOperation) -> fs::MockFsClient {
    let mut fs_client = fs::MockFsClient::new();
    let create_fails = matches!(failing_operation, FailingFsOperation::CreateDirectory);
    let read_fails = matches!(failing_operation, FailingFsOperation::ReadSource);
    fs_client.expect_create_dir_all().returning(move |_| {
        Box::pin(async move {
            if create_fails {
                return Err(fs::FsError::Io(std::io::Error::other("create failed")));
            }

            Ok(())
        })
    });
    fs_client.expect_is_file().returning(|_| true);
    fs_client.expect_read_file().returning(move |_| {
        Box::pin(async move {
            if read_fails {
                return Err(fs::FsError::Io(std::io::Error::other("read failed")));
            }

            Ok(b"png".to_vec())
        })
    });
    fs_client.expect_write_file().returning(|_, _| {
        Box::pin(async { Err(fs::FsError::Io(std::io::Error::other("write failed"))) })
    });

    fs_client
}
