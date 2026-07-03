use super::ClipboardBackend;
use crate::ClipboardError;

pub(crate) fn new_backend() -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
    Err(ClipboardError::Unavailable {
        reason: format!(
            "clipboard access is unsupported on {}",
            std::env::consts::OS
        ),
    })
}
