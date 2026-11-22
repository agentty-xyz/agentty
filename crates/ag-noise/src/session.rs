//! Encrypted session that carries frames as ordered Noise messages.

use crate::error::NoiseError;
use crate::handshake::MAX_MESSAGE_BYTES;

/// Authentication tag appended to every Noise message, in bytes.
const TAG_BYTES: usize = 16;
/// Frame bytes carried by one Noise message, after the marker byte.
const CHUNK_BYTES: usize = MAX_MESSAGE_BYTES - TAG_BYTES - 1;
/// Marker of a message that ends its frame.
const FINAL_CHUNK: u8 = 1;
/// Marker of a message whose frame continues in the next message.
const MORE_CHUNKS: u8 = 0;

/// Encrypted channel established by a finished [`crate::Handshake`].
///
/// A frame of any size is split into Noise messages, each starting with a
/// marker byte that says whether the frame continues. Messages must be
/// delivered in the order they were produced.
pub struct Session {
    max_frame_bytes: usize,
    pending: Vec<u8>,
    transport: snow::TransportState,
}

impl Session {
    /// Wraps the transport state of a finished handshake.
    pub(crate) fn new(transport: snow::TransportState, max_frame_bytes: usize) -> Self {
        Self {
            max_frame_bytes,
            pending: Vec::new(),
            transport,
        }
    }

    /// Encrypts one frame into the messages to send, in order.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the session can no longer
    /// encrypt.
    pub fn seal(&mut self, frame: &[u8]) -> Result<Vec<Vec<u8>>, NoiseError> {
        let mut messages = Vec::new();
        let mut chunks = frame.chunks(CHUNK_BYTES).peekable();

        loop {
            let chunk = chunks.next().unwrap_or_default();
            let marker = if chunks.peek().is_some() {
                MORE_CHUNKS
            } else {
                FINAL_CHUNK
            };
            let mut payload = Vec::with_capacity(chunk.len() + 1);
            payload.push(marker);
            payload.extend_from_slice(chunk);
            let mut message = vec![0; payload.len() + TAG_BYTES];
            let length = self.transport.write_message(&payload, &mut message)?;
            message.truncate(length);
            messages.push(message);

            if marker == FINAL_CHUNK {
                return Ok(messages);
            }
        }
    }

    /// Decrypts one received message and returns the frame once its last
    /// message arrives.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] for a message that fails
    /// authentication or carries no valid marker, and
    /// [`NoiseError::FrameTooLarge`] when the frame exceeds the session's
    /// limit. The session must be dropped after any error.
    pub fn open(&mut self, message: &[u8]) -> Result<Option<Vec<u8>>, NoiseError> {
        let mut payload = vec![0; message.len()];
        let length = self.transport.read_message(message, &mut payload)?;
        payload.truncate(length);
        let Some((marker, chunk)) = payload.split_first() else {
            return Err(NoiseError::Protocol);
        };
        if self.pending.len() + chunk.len() > self.max_frame_bytes {
            return Err(NoiseError::FrameTooLarge);
        }
        self.pending.extend_from_slice(chunk);

        match *marker {
            FINAL_CHUNK => Ok(Some(std::mem::take(&mut self.pending))),
            MORE_CHUNKS => Ok(None),
            _ => Err(NoiseError::Protocol),
        }
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
