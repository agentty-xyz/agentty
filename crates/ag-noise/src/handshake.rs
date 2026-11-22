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
/// handshake into a [`Session`]. A handshake that fails to read a message is
/// spent: every later call fails and it reports no peer key.
pub struct Handshake {
    /// Set once a received message failed; the state may then hold a peer
    /// key that was never proven.
    is_failed: bool,
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
    /// Returns [`NoiseError::Protocol`] when it is not this side's turn or
    /// the handshake already failed.
    pub fn write_message(&mut self) -> Result<Vec<u8>, NoiseError> {
        self.check_usable()?;
        let mut message = vec![0; MAX_MESSAGE_BYTES];
        let length = self.state.write_message(&[], &mut message)?;
        message.truncate(length);

        Ok(message)
    }

    /// Consumes the peer's next handshake message.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the message fails authentication,
    /// it is not the peer's turn, or the handshake already failed. A failed
    /// read spends the handshake.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), NoiseError> {
        self.check_usable()?;
        let mut payload = vec![0; MAX_MESSAGE_BYTES];
        let read = self.state.read_message(message, &mut payload);
        self.is_failed = read.is_err();
        read?;

        Ok(())
    }

    /// Whether every handshake message has been exchanged.
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The peer's static key, once a successfully read message has revealed
    /// it; `None` after any failed read.
    ///
    /// In the device pattern the instance learns this key from the first
    /// message, which a third party can replay. Treat the device as present
    /// only after the first [`Session::open`] succeeds.
    pub fn remote_public_key(&self) -> Option<PublicKey> {
        if self.is_failed {
            return None;
        }
        let key = self.state.get_remote_static()?;

        PublicKey::from_bytes(key).ok()
    }

    /// Turns a finished handshake into a session that refuses received
    /// frames above `max_frame_bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when the handshake is not finished or
    /// already failed.
    pub fn into_session(self, max_frame_bytes: usize) -> Result<Session, NoiseError> {
        self.check_usable()?;
        let transport = self.state.into_transport_mode()?;

        Ok(Session::new(transport, max_frame_bytes))
    }

    /// Wraps the outcome of building a handshake state.
    fn new(state: Result<snow::HandshakeState, snow::Error>) -> Result<Self, NoiseError> {
        let state = Box::new(state?);

        Ok(Self {
            is_failed: false,
            state,
        })
    }

    /// Refuses to continue a handshake that failed to read a message.
    fn check_usable(&self) -> Result<(), NoiseError> {
        if self.is_failed {
            return Err(NoiseError::Protocol);
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "handshake_test.rs"]
mod tests;
