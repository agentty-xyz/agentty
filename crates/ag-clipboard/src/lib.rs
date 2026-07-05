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
    #[cfg(target_os = "macos")]
    pub fn new() -> Result<Self, ClipboardError> {
        if let Some(error) = Self::disabled_by_env() {
            return Err(error);
        }

        Ok(Self {
            backend: backend::new_backend(),
        })
    }

    /// Opens the best clipboard backend available on the current platform.
    ///
    /// # Errors
    /// Returns [`ClipboardError::Unavailable`] when no supported clipboard
    /// backend is available for the current display server or operating
    /// system.
    #[cfg(not(target_os = "macos"))]
    pub fn new() -> Result<Self, ClipboardError> {
        if let Some(error) = Self::disabled_by_env() {
            return Err(error);
        }

        Ok(Self {
            backend: backend::new_backend()?,
        })
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

    fn disabled_by_env() -> Option<ClipboardError> {
        std::env::var_os(DISABLE_CLIPBOARD_ENV).map(|_| ClipboardError::Unavailable {
            reason: format!("clipboard access is disabled by `{DISABLE_CLIPBOARD_ENV}`"),
        })
    }
}
