use ag_noise::PublicKey;

use super::{DeviceStoreError, key_hash};

#[test]
fn key_hash_is_the_prefixed_sha256_hex_that_stored_devices_carry() {
    // Arrange
    let public_key = PublicKey::from_bytes(&[7; 32]).expect("key should load");

    // Act
    let hash = key_hash(&public_key);

    // Assert
    assert_eq!(
        hash,
        "2af71bb3c35db895cab8f7626da7b9d92e7b600a0666d2a5fe0c8aed22ab7480"
    );
}

#[test]
fn device_store_error_displays_its_message() {
    // Arrange
    let error = DeviceStoreError("disk full".to_string());

    // Act
    let message = error.to_string();

    // Assert
    assert_eq!(message, "disk full");
}
