//! End-to-end encrypted frame transport over a carrier the relay can read.

use ag_noise::{Handshake, PublicKey, Session, StaticKeypair};

use crate::transport::{FrameTransport, Incoming};

/// Largest frame accepted from a device, in bytes.
const MAX_FRAME_BYTES: usize = crate::server::MAX_MESSAGE_SIZE;

/// Ordered, reliable carrier of binary messages, such as a relay stream.
/// Whoever operates the carrier can read and drop what it carries.
pub(crate) trait MessageCarrier {
    /// Waits for the next message; `None` once the carrier ends or breaks.
    ///
    /// Must be cancel-safe, as [`FrameTransport::receive`] requires.
    async fn receive(&mut self) -> Option<Vec<u8>>;

    /// Sends one message. A failed send surfaces as the next receive
    /// returning `None`.
    async fn send(&mut self, message: Vec<u8>);

    /// Ends the carrier.
    async fn close(&mut self);
}

/// Frame transport that encrypts every frame for one device.
pub(crate) struct EncryptedTransport<Carrier> {
    carrier: Carrier,
    session: Session,
}

impl<Carrier: MessageCarrier> EncryptedTransport<Carrier> {
    /// Runs the instance side of the device handshake on `carrier`.
    ///
    /// Returns the transport with the device key the handshake names, or
    /// `None` when the handshake fails. Anyone who captured a device's first
    /// handshake message can replay it, so the key is proven only once the
    /// transport receives its first frame.
    pub(crate) async fn accept(
        mut carrier: Carrier,
        instance: &StaticKeypair,
    ) -> Option<(Self, PublicKey)> {
        let mut handshake = Handshake::device_responder(instance).ok()?;
        let first = carrier.receive().await?;
        handshake.read_message(&first).ok()?;
        let device = handshake.remote_public_key()?;
        carrier.send(handshake.write_message().ok()?).await;
        let session = handshake.into_session(MAX_FRAME_BYTES).ok()?;

        Some((Self { carrier, session }, device))
    }
}

impl<Carrier: MessageCarrier> FrameTransport for EncryptedTransport<Carrier> {
    async fn receive(&mut self) -> Incoming {
        loop {
            let Some(message) = self.carrier.receive().await else {
                return Incoming::Closed;
            };
            match self.session.open(&message) {
                Ok(Some(frame)) => {
                    return String::from_utf8(frame).map_or(Incoming::Binary, Incoming::Text);
                }
                Ok(None) => {}
                Err(_) => return Incoming::Closed,
            }
        }
    }

    async fn send(&mut self, text: String) {
        // Sealing fails only once the session is spent; the device then
        // sees the stream end.
        for message in self.session.seal(text.as_bytes()).unwrap_or_default() {
            self.carrier.send(message).await;
        }
    }

    /// The carrier has no close reason the device could trust, so a
    /// rejected device learns the reason from the frame sent before this.
    async fn close(&mut self) {
        self.carrier.close().await;
    }
}

#[cfg(test)]
#[path = "encrypted_test.rs"]
mod tests;
