//! One-time pairing codes that let a new device obtain a token.

use std::time::Duration;

use tokio::time::Instant;

use crate::entropy::Entropy;

/// Characters a pairing code draws from, omitting look-alikes `I`, `L`, `O`,
/// `0`, and `1`.
const PAIRING_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// Number of significant characters in one pairing code.
const PAIRING_CODE_LENGTH: usize = 8;
/// How long an issued code stays redeemable.
const PAIRING_CODE_LIFETIME: Duration = Duration::from_mins(10);
/// Failed redemptions that burn the active code.
const PAIRING_CODE_MAX_FAILURES: u32 = 5;
/// Bytes below this value map onto the alphabet without modulo bias.
const UNBIASED_BYTE_LIMIT: usize = 256 / PAIRING_CODE_ALPHABET.len() * PAIRING_CODE_ALPHABET.len();

/// One issued pairing code, in display form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingCode {
    /// Code to show the user, formatted as `XXXX-XXXX`.
    pub code: String,
    /// Time remaining before the code stops being redeemable.
    pub expires_in: Duration,
}

/// The single active pairing code, if any.
#[derive(Default)]
pub(crate) struct PairingState {
    active: Option<ActivePairingCode>,
}

impl PairingState {
    /// Replaces any active code with a fresh one, or returns `None` when the
    /// entropy source fails.
    pub(crate) fn issue(&mut self, entropy: &dyn Entropy) -> Option<PairingCode> {
        let code = random_code(entropy, PAIRING_CODE_LENGTH)?;
        let display_code = format!(
            "{}-{}",
            &code[..PAIRING_CODE_LENGTH / 2],
            &code[PAIRING_CODE_LENGTH / 2..]
        );
        self.active = Some(ActivePairingCode {
            code,
            expires_at: Instant::now() + PAIRING_CODE_LIFETIME,
            failures: 0,
        });

        Some(PairingCode {
            code: display_code,
            expires_in: PAIRING_CODE_LIFETIME,
        })
    }

    /// Consumes the active code when `candidate` matches it.
    ///
    /// Matching ignores case, dashes, and whitespace. Expired codes are
    /// discarded, and a code is burned after repeated failures.
    pub(crate) fn redeem(&mut self, candidate: &str) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if Instant::now() >= active.expires_at {
            self.active = None;

            return false;
        }
        if normalized_code(candidate) == active.code {
            self.active = None;

            return true;
        }

        active.failures += 1;
        if active.failures >= PAIRING_CODE_MAX_FAILURES {
            self.active = None;
        }

        false
    }
}

/// Code material and redemption limits for the active code.
struct ActivePairingCode {
    code: String,
    expires_at: Instant,
    failures: u32,
}

/// Draws an unbiased code of `length` characters from the alphabet, or
/// returns `None` when the entropy source fails.
pub(crate) fn random_code(entropy: &dyn Entropy, length: usize) -> Option<String> {
    let mut code = String::with_capacity(length);
    let mut bytes = vec![0; length * 2];

    while code.len() < length {
        if !entropy.fill(&mut bytes) {
            return None;
        }
        for &byte in &bytes {
            if usize::from(byte) < UNBIASED_BYTE_LIMIT && code.len() < length {
                let index = usize::from(byte) % PAIRING_CODE_ALPHABET.len();
                code.push(char::from(PAIRING_CODE_ALPHABET[index]));
            }
        }
    }

    Some(code)
}

/// Uppercases `candidate` and drops dashes and whitespace.
fn normalized_code(candidate: &str) -> String {
    candidate
        .chars()
        .filter(|character| *character != '-' && !character.is_whitespace())
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
#[path = "pairing_test.rs"]
mod tests;
