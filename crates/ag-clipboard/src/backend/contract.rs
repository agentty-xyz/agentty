use std::path::PathBuf;

use crate::{ClipboardError, RgbaImageData};

/// Read operations implemented by one clipboard source.
///
/// Platform backends implement this trait; hosts can implement it to drive a
/// [`crate::Clipboard`] from an in-memory source through
/// [`crate::Clipboard::from_backend`].
pub trait ClipboardBackend {
    /// Reads clipboard text.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the source has no text or cannot
    /// complete the read.
    fn read_text(&mut self) -> Result<String, ClipboardError>;

    /// Reads copied filesystem paths.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the source has no file-list payload
    /// or cannot complete the read.
    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError>;

    /// Reads image data as RGBA pixels.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when the source has no image payload,
    /// image decoding fails, or the read cannot complete.
    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError>;
}
