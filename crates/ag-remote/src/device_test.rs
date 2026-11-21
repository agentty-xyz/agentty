use super::{DeviceStoreError, token_hash};

#[test]
fn token_hash_is_lowercase_sha256_hex() {
    // Arrange
    let token = "abc";

    // Act
    let hash = token_hash(token);

    // Assert
    assert_eq!(
        hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
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
