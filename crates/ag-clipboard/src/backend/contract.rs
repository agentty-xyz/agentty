//! `ClipboardBackend` read contract implemented by each clipboard source.

use std::path::PathBuf;

use crate::{ClipboardError, ClipboardImage};

/// Declares [`ClipboardBackend`] with `$visibility`, so one definition is
/// crate-private by default and public under the `test-utils` feature.
macro_rules! declare_clipboard_backend {
    ($visibility:vis) => {
        /// Read operations implemented by one clipboard source.
        ///
        /// Platform backends implement this trait. With the `test-utils`
        /// feature, hosts can implement it for an in-memory source and wrap
        /// it with `Clipboard::from_backend`.
        $visibility trait ClipboardBackend {
            /// Reads clipboard text.
            ///
            /// # Errors
            /// Returns a [`ClipboardError`] when the source has no text or
            /// cannot complete the read.
            fn read_text(&mut self) -> Result<String, ClipboardError>;

            /// Reads copied filesystem paths.
            ///
            /// # Errors
            /// Returns a [`ClipboardError`] when the source has no file-list
            /// payload or cannot complete the read.
            fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError>;

            /// Reads one image payload, querying each image source at most
            /// once.
            ///
            /// Returns PNG payloads as their original encoded bytes without
            /// decoding pixel data; other formats are decoded to RGBA.
            ///
            /// # Errors
            /// Returns a [`ClipboardError`] when the source has no image
            /// payload, a PNG header is invalid, decoding fails, or the read
            /// cannot complete.
            fn read_image(&mut self) -> Result<ClipboardImage, ClipboardError>;
        }
    };
}

#[cfg(any(test, feature = "test-utils"))]
declare_clipboard_backend!(pub);
#[cfg(not(any(test, feature = "test-utils")))]
declare_clipboard_backend!(pub(crate));
