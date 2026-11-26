use std::time::Duration;

use super::{
    PAIRING_CODE_ALPHABET, PAIRING_CODE_LIFETIME, PAIRING_CODE_MAX_FAILURES, PairingCode,
    PairingState, UNBIASED_BYTE_LIMIT, random_code,
};
use crate::entropy::OsEntropy;
use crate::source_support::{FailingEntropy, ReplayEntropy};

/// Issues a code from bytes `0..8`, which map to `ABCDEFGH`.
fn issue_alphabet_prefix(state: &mut PairingState) -> PairingCode {
    state
        .issue(&ReplayEntropy::new((0..8).collect()))
        .expect("replayed entropy should issue a code")
}

#[test]
fn issue_formats_code_from_unambiguous_alphabet() {
    // Arrange
    let mut state = PairingState::default();

    // Act
    let code = state
        .issue(&OsEntropy)
        .expect("OS entropy should be available");

    // Assert
    assert_eq!(code.expires_in, Duration::from_secs(600));
    assert_eq!(code.code.len(), 9);
    assert_eq!(code.code.as_bytes()[4], b'-');
    assert!(
        code.code
            .chars()
            .filter(|character| *character != '-')
            .all(|character| PAIRING_CODE_ALPHABET.contains(&(character as u8)))
    );
}

#[test]
fn issue_skips_bytes_that_would_bias_the_alphabet() {
    // Arrange
    let mut state = PairingState::default();
    let first_biased = u8::try_from(UNBIASED_BYTE_LIMIT).expect("limit should fit in a byte");
    let mut bytes = vec![first_biased, 255];
    bytes.extend(0..8);
    let entropy = ReplayEntropy::new(bytes);

    // Act
    let code = state
        .issue(&entropy)
        .expect("replayed entropy should issue");

    // Assert
    assert_eq!(code.code, "ABCD-EFGH");
}

#[test]
fn random_code_draws_the_requested_length_without_bias() {
    // Arrange
    let first_biased = u8::try_from(UNBIASED_BYTE_LIMIT).expect("limit should fit in a byte");
    let entropy = ReplayEntropy::new(vec![first_biased, 8, 9, 10, 11]);

    // Act
    let confirmation = random_code(&entropy, 4);
    let failed = random_code(&FailingEntropy, 4);

    // Assert
    assert_eq!(confirmation.as_deref(), Some("JKMN"));
    assert_eq!(failed, None);
}

#[test]
fn issue_reports_entropy_failure_and_keeps_no_code() {
    // Arrange
    let mut state = PairingState::default();

    // Act
    let code = state.issue(&FailingEntropy);
    let redeemed = state.redeem("ABCD-EFGH");

    // Assert
    assert_eq!(code, None);
    assert!(!redeemed);
}

#[test]
fn redeem_accepts_case_dash_and_whitespace_variants_once() {
    // Arrange
    let mut state = PairingState::default();
    issue_alphabet_prefix(&mut state);

    // Act
    let first = state.redeem(" abcd efgh ");
    let second = state.redeem("ABCD-EFGH");

    // Assert
    assert!(first);
    assert!(!second);
}

#[test]
fn redeem_without_active_code_fails() {
    // Arrange
    let mut state = PairingState::default();

    // Act
    let redeemed = state.redeem("ABCDEFGH");

    // Assert
    assert!(!redeemed);
}

#[test]
fn issue_replaces_the_active_code() {
    // Arrange
    let mut state = PairingState::default();
    issue_alphabet_prefix(&mut state);
    let replacement = state
        .issue(&ReplayEntropy::new((8..16).collect()))
        .expect("replayed entropy should issue");

    // Act
    let old = state.redeem("ABCDEFGH");
    let new = state.redeem(&replacement.code);

    // Assert
    assert_eq!(replacement.code, "JKMN-PQRS");
    assert!(!old);
    assert!(new);
}

#[tokio::test(start_paused = true)]
async fn redeem_discards_expired_code() {
    // Arrange
    let mut state = PairingState::default();
    issue_alphabet_prefix(&mut state);
    tokio::time::advance(PAIRING_CODE_LIFETIME).await;

    // Act
    let redeemed = state.redeem("ABCDEFGH");

    // Assert
    assert!(!redeemed);
    assert!(state.active.is_none());
}

#[tokio::test(start_paused = true)]
async fn redeem_accepts_code_just_before_expiry() {
    // Arrange
    let mut state = PairingState::default();
    issue_alphabet_prefix(&mut state);
    tokio::time::advance(PAIRING_CODE_LIFETIME.saturating_sub(Duration::from_millis(1))).await;

    // Act
    let redeemed = state.redeem("ABCDEFGH");

    // Assert
    assert!(redeemed);
}

#[test]
fn repeated_failures_burn_the_code() {
    // Arrange
    let mut state = PairingState::default();
    issue_alphabet_prefix(&mut state);
    for _ in 0..PAIRING_CODE_MAX_FAILURES - 1 {
        assert!(!state.redeem("WRONG"));
    }
    assert!(state.active.is_some());

    // Act
    let last_failure = state.redeem("WRONG");
    let correct_after_burn = state.redeem("ABCDEFGH");

    // Assert
    assert!(!last_failure);
    assert!(!correct_after_burn);
}
