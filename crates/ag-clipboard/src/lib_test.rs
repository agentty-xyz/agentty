use std::path::PathBuf;
use std::process::Command;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::error::ClipboardError;
use crate::image_data::{ClipboardImage, RgbaImageData};
use crate::{Clipboard, DISABLE_CLIPBOARD_ENV, backend};

const DISABLED_CONSTRUCTOR_CHILD_ENV: &str = "AGENTTY_TEST_DISABLED_CONSTRUCTOR_CHILD";
const RED_PIXEL_RGBA: [u8; 4] = [255, 0, 0, 255];

/// In-memory backend that serves fixed text, files, and one image payload.
struct TestClipboardBackend {
    image: ClipboardImage,
}

impl TestClipboardBackend {
    /// Returns a backend whose image payload is a 1x1 red PNG.
    fn with_png() -> Self {
        Self {
            image: ClipboardImage::Png(red_pixel_png_bytes()),
        }
    }
}

impl backend::ClipboardBackend for TestClipboardBackend {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Ok("test clipboard text".to_string())
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        Ok(vec![PathBuf::from("/tmp/test.txt")])
    }

    fn read_image(&mut self) -> Result<ClipboardImage, ClipboardError> {
        Ok(self.image.clone())
    }
}

#[test]
fn test_new_respects_disable_environment_variable_in_child_process() {
    if std::env::var_os(DISABLED_CONSTRUCTOR_CHILD_ENV).is_some() {
        // Arrange
        let expected_environment_variable = DISABLE_CLIPBOARD_ENV;

        // Act
        let result = Clipboard::new();

        // Assert
        assert!(matches!(
            result,
            Err(ClipboardError::Unavailable { reason })
                if reason.contains(expected_environment_variable)
        ));

        return;
    }

    // Arrange
    let current_test_binary =
        std::env::current_exe().expect("current test binary path should be available");

    // Act
    let output = Command::new(current_test_binary)
        .arg("--exact")
        .arg("tests::test_new_respects_disable_environment_variable_in_child_process")
        .arg("--nocapture")
        .env(DISABLED_CONSTRUCTOR_CHILD_ENV, "1")
        .env(DISABLE_CLIPBOARD_ENV, "1")
        .output()
        .expect("disabled constructor child test should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Assert
    assert!(
        output.status.success(),
        "disabled constructor child test failed: {stderr}"
    );
}

#[test]
fn test_new_with_backend_respects_disabled_error() {
    // Arrange
    let disabled_error = ClipboardError::Unavailable {
        reason: "test clipboard is disabled".to_string(),
    };
    let backend_factory = unavailable_backend_factory;

    // Act
    let result = Clipboard::new_with_backend(Some(disabled_error), backend_factory);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason == "test clipboard is disabled"
    ));
}

#[test]
fn test_new_with_backend_accepts_infallible_backend() {
    // Arrange
    let backend_factory =
        || Box::new(TestClipboardBackend::with_png()) as Box<dyn backend::ClipboardBackend>;

    // Act
    let mut clipboard = Clipboard::new_with_backend(None, backend_factory)
        .expect("infallible test backend should initialize");
    let text = clipboard
        .read_text()
        .expect("test backend should return text");
    let paths = clipboard
        .read_file_list()
        .expect("test backend should return a file list");
    let image = clipboard
        .read_image()
        .expect("test backend should return an image");

    // Assert
    assert_eq!(text, "test clipboard text");
    assert_eq!(paths, vec![PathBuf::from("/tmp/test.txt")]);
    assert_eq!(image, ClipboardImage::Png(red_pixel_png_bytes()));
}

#[test]
fn test_read_image_rgba_decodes_png_payload() {
    // Arrange
    let mut clipboard = Clipboard::from_backend(Box::new(TestClipboardBackend::with_png()));

    // Act
    let image_data = clipboard
        .read_image_rgba()
        .expect("PNG payload should decode");

    // Assert
    assert_eq!(image_data, red_pixel_image_data());
}

#[test]
fn test_read_image_rgba_returns_decoded_payload_unchanged() {
    // Arrange
    let mut clipboard = Clipboard::from_backend(Box::new(TestClipboardBackend {
        image: ClipboardImage::Rgba(red_pixel_image_data()),
    }));

    // Act
    let image_data = clipboard
        .read_image_rgba()
        .expect("RGBA payload should be returned");

    // Assert
    assert_eq!(image_data, red_pixel_image_data());
}

#[test]
fn test_from_backend_reads_through_injected_backend() {
    // Arrange
    let backend = Box::new(TestClipboardBackend::with_png());

    // Act
    let mut clipboard = Clipboard::from_backend(backend);
    let text = clipboard
        .read_text()
        .expect("test backend should return text");

    // Assert
    assert_eq!(text, "test clipboard text");
}

#[test]
fn test_new_with_backend_propagates_backend_error() {
    // Arrange
    let backend_factory = unavailable_backend_factory;

    // Act
    let result = Clipboard::new_with_backend(None, backend_factory);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Unavailable { reason })
            if reason == "test backend is unavailable"
    ));
}

fn unavailable_backend_factory() -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
    Err(ClipboardError::Unavailable {
        reason: "test backend is unavailable".to_string(),
    })
}

fn red_pixel_png_bytes() -> Vec<u8> {
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&RED_PIXEL_RGBA, 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG should encode");

    png_bytes
}

fn red_pixel_image_data() -> RgbaImageData {
    RgbaImageData {
        height: 1,
        rgba_bytes: RED_PIXEL_RGBA.to_vec(),
        width: 1,
    }
}
