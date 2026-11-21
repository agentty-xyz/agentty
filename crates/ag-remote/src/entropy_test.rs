use super::{OsEntropy, random_hex};
use crate::test_support::FailingEntropy;

#[test]
fn random_hex_encodes_requested_byte_count() {
    // Arrange
    let entropy = OsEntropy;

    // Act
    let first = random_hex(&entropy, 16);
    let second = random_hex(&entropy, 16);

    // Assert
    let first = first.expect("OS entropy should be available");
    assert_eq!(first.len(), 32);
    assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
    assert_ne!(Some(first), second);
}

#[test]
fn random_hex_reports_entropy_failure() {
    // Arrange
    let entropy = FailingEntropy;

    // Act
    let value = random_hex(&entropy, 16);

    // Assert
    assert_eq!(value, None);
}
