use crate::{KEY_LENGTH, NoiseError, PublicKey, StaticKeypair};

#[test]
fn public_key_round_trips_through_bytes_and_hex() {
    // Arrange
    let bytes = [7; KEY_LENGTH];

    // Act
    let key = PublicKey::from_bytes(&bytes).expect("key should load");
    let reloaded = PublicKey::from_hex(&key.to_hex()).expect("hex should load");

    // Assert
    assert_eq!(key.as_bytes(), &bytes);
    assert_eq!(key.to_hex(), "07".repeat(KEY_LENGTH));
    assert_eq!(reloaded, key);
}

#[test]
fn public_key_rejects_wrong_length_and_bad_hex() {
    // Arrange
    let short = [7; KEY_LENGTH - 1];

    // Act
    let from_short = PublicKey::from_bytes(&short);
    let from_short_hex = PublicKey::from_hex("0707");
    let from_text = PublicKey::from_hex("not hex");

    // Assert
    assert_eq!(from_short, Err(NoiseError::InvalidKey));
    assert_eq!(from_short_hex, Err(NoiseError::InvalidKey));
    assert_eq!(from_text, Err(NoiseError::InvalidKey));
}

#[test]
fn fingerprint_is_the_sha256_digest_of_the_key() {
    // Arrange
    let key = PublicKey::from_bytes(&[0; KEY_LENGTH]).expect("key should load");

    // Act
    let fingerprint = key.fingerprint();

    // Assert
    assert_eq!(
        fingerprint,
        "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
    );
}

#[test]
fn generated_keypairs_differ_and_restore_from_their_parts() {
    // Arrange
    let first = StaticKeypair::generate().expect("keypair should generate");
    let second = StaticKeypair::generate().expect("keypair should generate");

    // Act
    let restored = StaticKeypair::from_parts(first.private_bytes(), first.public().as_bytes());

    // Assert
    assert_ne!(first, second);
    assert_eq!(restored, Ok(first));
}

#[test]
fn from_parts_rejects_halves_of_the_wrong_length() {
    // Arrange
    let key = [1; KEY_LENGTH];
    let short = [1; KEY_LENGTH - 1];

    // Act
    let short_private = StaticKeypair::from_parts(&short, &key);
    let short_public = StaticKeypair::from_parts(&key, &short);

    // Assert
    assert_eq!(short_private, Err(NoiseError::InvalidKey));
    assert_eq!(short_public, Err(NoiseError::InvalidKey));
}

#[test]
fn debug_output_omits_the_private_key() {
    // Arrange
    let keypair = StaticKeypair::from_parts(&[0xAB; KEY_LENGTH], &[0xCD; KEY_LENGTH])
        .expect("keypair should load");

    // Act
    let debug = format!("{keypair:?}");

    // Assert
    assert!(debug.contains(&"cd".repeat(KEY_LENGTH)));
    assert!(!debug.contains(&"ab".repeat(KEY_LENGTH)));
}
