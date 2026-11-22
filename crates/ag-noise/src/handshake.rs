//! Handshake state machines for the two remote-control patterns.

use crate::error::NoiseError;
use crate::key::{PublicKey, StaticKeypair};
use crate::session::Session;

/// Noise protocol between a device and an instance.
pub(crate) const DEVICE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";
/// Largest Noise message, in bytes.
pub(crate) const MAX_MESSAGE_BYTES: usize = 65_535;
/// Noise protocol between an instance and a relay.
const RELAY_PATTERN: &str = "Noise_XN_25519_ChaChaPoly_SHA256";
/// Context mixed into device handshakes so they cannot be replayed as
/// relay handshakes or against another protocol version.
const DEVICE_PROLOGUE: &[u8] = b"agentty-remote-device/1";
/// Context mixed into relay handshakes.
const RELAY_PROLOGUE: &[u8] = b"agentty-remote-relay/1";

/// One side of a handshake in progress.
///
/// Call [`Handshake::write_message`] and [`Handshake::read_message`] in the
/// order the crate documentation gives for the pattern, then turn a finished
/// handshake into a [`Session`].
pub struct Handshake {
    state: Box<snow::HandshakeState>,
}

impl Handshake {
    /// Starts the device side of a device-to-instance handshake.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the handshake cannot be set up.
    pub fn device_initiator(
        device: &StaticKeypair,
        instance: &PublicKey,
    ) -> Result<Self, NoiseError> {
        Self::new(
            snow::Builder::new(DEVICE_PATTERN.parse()?)
                .prologue(DEVICE_PROLOGUE)?
                .local_private_key(device.private_bytes())?
                .remote_public_key(instance.as_bytes())?
                .build_initiator(),
        )
    }

    /// Starts the instance side of a device-to-instance handshake.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the handshake cannot be set up.
    pub fn device_responder(instance: &StaticKeypair) -> Result<Self, NoiseError> {
        Self::new(
            snow::Builder::new(DEVICE_PATTERN.parse()?)
                .prologue(DEVICE_PROLOGUE)?
                .local_private_key(instance.private_bytes())?
                .build_responder(),
        )
    }

    /// Starts the instance side of an instance-to-relay handshake.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the handshake cannot be set up.
    pub fn relay_initiator(instance: &StaticKeypair) -> Result<Self, NoiseError> {
        Self::new(
            snow::Builder::new(RELAY_PATTERN.parse()?)
                .prologue(RELAY_PROLOGUE)?
                .local_private_key(instance.private_bytes())?
                .build_initiator(),
        )
    }

    /// Starts the relay side of an instance-to-relay handshake.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the handshake cannot be set up.
    pub fn relay_responder() -> Result<Self, NoiseError> {
        Self::new(
            snow::Builder::new(RELAY_PATTERN.parse()?)
                .prologue(RELAY_PROLOGUE)?
                .build_responder(),
        )
    }

    /// Produces this side's next handshake message.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when it is not this side's turn.
    pub fn write_message(&mut self) -> Result<Vec<u8>, NoiseError> {
        let mut message = vec![0; MAX_MESSAGE_BYTES];
        let length = self.state.write_message(&[], &mut message)?;
        message.truncate(length);

        Ok(message)
    }

    /// Consumes the peer's next handshake message.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the message fails authentication
    /// or it is not the peer's turn.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), NoiseError> {
        let mut payload = vec![0; MAX_MESSAGE_BYTES];
        self.state.read_message(message, &mut payload)?;

        Ok(())
    }

    /// Whether every handshake message has been exchanged.
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The peer's static key, once the handshake has revealed it.
    pub fn remote_public_key(&self) -> Option<PublicKey> {
        let key = self.state.get_remote_static()?;

        PublicKey::from_bytes(key).ok()
    }

    /// Turns a finished handshake into a session that refuses received
    /// frames above `max_frame_bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the handshake is not finished.
    pub fn into_session(self, max_frame_bytes: usize) -> Result<Session, NoiseError> {
        let transport = self.state.into_transport_mode()?;

        Ok(Session::new(transport, max_frame_bytes))
    }

    /// Wraps the outcome of building a handshake state.
    fn new(state: Result<snow::HandshakeState, snow::Error>) -> Result<Self, NoiseError> {
        let state = Box::new(state?);

        Ok(Self { state })
    }
}

#[cfg(test)]
#[path = "handshake_test.rs"]
mod tests;
