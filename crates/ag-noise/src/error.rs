//! Errors reported by handshakes and sessions.

/// Failure of a Noise handshake or session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NoiseError {
    /// A received frame exceeded the session's size limit.
    #[error("Frame is too large")]
    FrameTooLarge,
    /// A key did not have the expected length or encoding.
    #[error("Invalid key")]
    InvalidKey,
    /// The peer's bytes failed authentication or broke the protocol.
    #[error("Noise protocol failure")]
    Protocol,
}

impl From<snow::Error> for NoiseError {
    /// Drops the cause, which callers must not echo to an untrusted peer.
    fn from(_: snow::Error) -> Self {
        Self::Protocol
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
