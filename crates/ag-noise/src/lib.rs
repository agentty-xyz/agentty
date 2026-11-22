//! Noise handshakes and encrypted sessions for Agentty remote control.
//!
//! The crate performs no I/O: a [`Handshake`] produces and consumes handshake
//! messages, and the [`Session`] it becomes encrypts and decrypts frames. The
//! caller moves the bytes over any ordered, reliable carrier.
//!
//! Two fixed patterns exist, each identified by its constructor pair:
//!
//! - **Device to instance** ([`Handshake::device_initiator`],
//!   [`Handshake::device_responder`]) uses Noise `IK`. The device already knows
//!   the instance key. The device writes one message, the instance reads it and
//!   writes one back, and both sides then hold a [`Session`]. The instance
//!   learns the device key from the first message.
//! - **Instance to relay** ([`Handshake::relay_initiator`],
//!   [`Handshake::relay_responder`]) uses Noise `XN`. The instance writes, the
//!   relay answers, and the instance writes again. After reading that third
//!   message the relay knows the instance holds the private half of
//!   [`Handshake::remote_public_key`].
//!
//! A [`Session`] splits a frame into Noise messages that must arrive in the
//! order they were produced, and refuses frames above its configured size.

mod error;
mod handshake;
mod key;
mod session;

pub use error::NoiseError;
pub use handshake::Handshake;
pub use key::{KEY_LENGTH, PublicKey, StaticKeypair};
pub use session::Session;
