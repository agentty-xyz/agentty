use std::fmt;

/// Error returned by platform clipboard backends.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// Clipboard access is unavailable on the current platform or display
    /// server.
    #[error("Clipboard backend is unavailable: {reason}")]
    Unavailable {
        /// Human-readable backend availability reason.
        reason: String,
    },

    /// The clipboard does not contain the requested payload type.
    #[error("Clipboard content is unavailable")]
    ContentUnavailable,

    /// The selected backend does not support the requested operation.
    #[error("Clipboard operation is unsupported: {operation}")]
    Unsupported {
        /// Operation name.
        operation: &'static str,
    },

    /// The backend failed while talking to the platform clipboard service.
    #[error("Clipboard backend failed: {reason}")]
    Backend {
        /// Human-readable backend failure reason.
        reason: String,
    },

    /// Clipboard image bytes could not be decoded into RGBA pixels.
    #[error("Clipboard image conversion failed: {reason}")]
    ImageConversion {
        /// Human-readable image conversion reason.
        reason: String,
    },
}

impl ClipboardError {
    #[cfg(target_os = "linux")]
    pub(crate) fn backend(context: &str, error: impl fmt::Display) -> Self {
        Self::Backend {
            reason: format!("{context}: {error}"),
        }
    }

    pub(crate) fn image_conversion(context: &str, error: impl fmt::Display) -> Self {
        Self::ImageConversion {
            reason: format!("{context}: {error}"),
        }
    }
}
