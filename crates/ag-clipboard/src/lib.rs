//! Narrow read-only clipboard access used by Agentty prompt image capture.

mod backend;
mod error;
mod format;
mod image_data;
mod uri;

use std::path::PathBuf;

pub use error::ClipboardError;
pub use image_data::RgbaImageData;

const DISABLE_CLIPBOARD_ENV: &str = "AGENTTY_DISABLE_CLIPBOARD";

/// System clipboard reader for text, copied files, and RGBA image data.
pub struct Clipboard {
    backend: Box<dyn backend::ClipboardBackend>,
}

impl Clipboard {
    /// Opens the best clipboard backend available on the current platform.
    ///
    /// # Errors
    /// Returns [`ClipboardError::Unavailable`] when no supported clipboard
    /// backend is available for the current display server or operating
    /// system.
    pub fn new() -> Result<Self, ClipboardError> {
        Self::new_with_backend(Self::disabled_by_env(), backend::new_backend)
    }

    /// Reads clipboard text.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the clipboard has no text or the
    /// backend cannot complete the read.
    pub fn read_text(&mut self) -> Result<String, ClipboardError> {
        self.backend.read_text()
    }

    /// Reads copied filesystem paths from the clipboard.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the clipboard has no file-list payload
    /// or the backend cannot complete the read.
    pub fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        self.backend.read_file_list()
    }

    /// Reads clipboard image data as RGBA pixels.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the clipboard has no image payload,
    /// image decoding fails, or the backend cannot complete the read.
    pub fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        self.backend.read_image_rgba()
    }

    fn new_with_backend<B>(
        disabled_error: Option<ClipboardError>,
        backend_factory: impl FnOnce() -> B,
    ) -> Result<Self, ClipboardError>
    where
        B: IntoBackendResult,
    {
        if let Some(error) = disabled_error {
            return Err(error);
        }

        let backend = backend_factory().into_backend_result()?;

        Ok(Self { backend })
    }

    fn disabled_by_env() -> Option<ClipboardError> {
        std::env::var_os(DISABLE_CLIPBOARD_ENV).map(|_| ClipboardError::Unavailable {
            reason: format!("clipboard access is disabled by `{DISABLE_CLIPBOARD_ENV}`"),
        })
    }
}

trait IntoBackendResult {
    fn into_backend_result(self) -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError>;
}

impl IntoBackendResult for Box<dyn backend::ClipboardBackend> {
    fn into_backend_result(self) -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
        Ok(self)
    }
}

impl IntoBackendResult for Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
    fn into_backend_result(self) -> Result<Box<dyn backend::ClipboardBackend>, ClipboardError> {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const DISABLED_CONSTRUCTOR_CHILD_ENV: &str = "AGENTTY_TEST_DISABLED_CONSTRUCTOR_CHILD";

    struct TestClipboardBackend;

    impl backend::ClipboardBackend for TestClipboardBackend {
        fn read_text(&mut self) -> Result<String, ClipboardError> {
            Ok("test clipboard text".to_string())
        }

        fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
            Ok(vec![PathBuf::from("/tmp/test.txt")])
        }

        fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
            Ok(RgbaImageData {
                height: 1,
                rgba_bytes: vec![0, 0, 0, 255],
                width: 1,
            })
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
            || Box::new(TestClipboardBackend) as Box<dyn backend::ClipboardBackend>;

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
            .read_image_rgba()
            .expect("test backend should return image data");

        // Assert
        assert_eq!(text, "test clipboard text");
        assert_eq!(paths, vec![PathBuf::from("/tmp/test.txt")]);
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 1);
        assert_eq!(image.rgba_bytes, vec![0, 0, 0, 255]);
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
}
