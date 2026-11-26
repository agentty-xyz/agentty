//! Which instances may register: the policy an embedder supplies.

use async_trait::async_trait;

/// Decides whether an instance may register with the relay.
///
/// The relay asks once per registration, after the instance has proven the
/// key behind its identifier with the relay handshake and before it is
/// registered, so an implementation never sees an unproven identifier. An
/// admitted instance stays registered until its control connection ends.
///
/// An error, or no answer within [`crate::RelayLimits::admission_timeout`],
/// refuses the instance. The relay remembers no decision: the next
/// registration of the same instance asks again.
#[async_trait]
pub trait AdmissionPolicy: Send + Sync {
    /// Returns whether the instance identified by `instance_id` may
    /// register. The identifier is the lowercase hexadecimal SHA-256
    /// fingerprint of the instance's public key.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when no decision can be made, such as when
    /// a store the policy consults is unreachable.
    async fn admit(&self, instance_id: &str) -> Result<bool, AdmissionError>;
}

/// Failure that keeps an [`AdmissionPolicy`] from deciding. The relay
/// refuses the instance and never logs or forwards the message.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct AdmissionError(pub String);

/// Policy that admits every instance: what [`crate::start`] uses.
pub(crate) struct AdmitAll;

#[async_trait]
impl AdmissionPolicy for AdmitAll {
    async fn admit(&self, _instance_id: &str) -> Result<bool, AdmissionError> {
        Ok(true)
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
