//! Static keys that identify an instance or a device.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::error::NoiseError;
use crate::handshake::DEVICE_PATTERN;

/// Length of a public or private key, in bytes.
pub const KEY_LENGTH: usize = 32;

/// X25519 public key that identifies an instance or a device.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PublicKey([u8; KEY_LENGTH]);

impl PublicKey {
    /// Reads a key from its raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::InvalidKey`] unless `bytes` holds exactly
    /// [`KEY_LENGTH`] bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NoiseError> {
        let bytes = bytes.try_into().map_err(|_| NoiseError::InvalidKey)?;

        Ok(Self(bytes))
    }

    /// Reads a key from the hexadecimal text [`PublicKey::to_hex`] writes.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::InvalidKey`] for text that is not hexadecimal
    /// or has another length.
    pub fn from_hex(text: &str) -> Result<Self, NoiseError> {
        let bytes = hex::decode(text).map_err(|_| NoiseError::InvalidKey)?;

        Self::from_bytes(&bytes)
    }

    /// Raw key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_LENGTH] {
        &self.0
    }

    /// Lowercase hexadecimal text of the key.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Stable public identifier derived from the key: the hexadecimal
    /// SHA-256 digest of its bytes.
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.0))
    }
}

/// Long-term key pair of an instance or a device.
#[derive(Clone, Eq, PartialEq)]
pub struct StaticKeypair {
    private: [u8; KEY_LENGTH],
    public: PublicKey,
}

impl StaticKeypair {
    /// Generates a key pair from the system's secure random source.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Protocol`] when no random source is available.
    pub fn generate() -> Result<Self, NoiseError> {
        let keypair = snow::Builder::new(DEVICE_PATTERN.parse()?).generate_keypair()?;

        Self::from_parts(&keypair.private, &keypair.public)
    }

    /// Restores a key pair from the bytes of [`StaticKeypair::private_bytes`]
    /// and its public key.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::InvalidKey`] unless both halves hold exactly
    /// [`KEY_LENGTH`] bytes.
    pub fn from_parts(private: &[u8], public: &[u8]) -> Result<Self, NoiseError> {
        let public = PublicKey::from_bytes(public)?;
        let private = private.try_into().map_err(|_| NoiseError::InvalidKey)?;

        Ok(Self { private, public })
    }

    /// Public half, safe to share.
    pub fn public(&self) -> &PublicKey {
        &self.public
    }

    /// Private half, for persistence only. Keep it secret.
    pub fn private_bytes(&self) -> &[u8; KEY_LENGTH] {
        &self.private
    }
}

impl fmt::Debug for StaticKeypair {
    /// Shows only the public half so the private key cannot reach logs.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticKeypair")
            .field("public", &self.public.to_hex())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "key_test.rs"]
mod tests;
