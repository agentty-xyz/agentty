//! Cryptographic randomness for pairing codes, device identifiers, and
//! tokens.

/// Source of cryptographically secure random bytes.
pub(crate) trait Entropy: Send + Sync {
    /// Fills `bytes` and returns whether the source succeeded.
    fn fill(&self, bytes: &mut [u8]) -> bool;
}

/// Operating-system randomness.
pub(crate) struct OsEntropy;

impl Entropy for OsEntropy {
    fn fill(&self, bytes: &mut [u8]) -> bool {
        getrandom::fill(bytes).is_ok()
    }
}

/// Returns `byte_count` random bytes as lowercase hexadecimal, or `None`
/// when the source fails.
pub(crate) fn random_hex(entropy: &dyn Entropy, byte_count: usize) -> Option<String> {
    let mut bytes = vec![0; byte_count];

    entropy.fill(&mut bytes).then(|| hex::encode(bytes))
}

#[cfg(test)]
#[path = "entropy_test.rs"]
mod tests;
