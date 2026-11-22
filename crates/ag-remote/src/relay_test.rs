use std::time::Duration;

use ag_noise::PublicKey;

use super::{INITIAL_RETRY_DELAY, retry_delay_after};
use crate::relay::pairing_link;

#[test]
fn pairing_link_points_at_the_relay_page_and_keeps_secrets_in_the_fragment() {
    // Arrange
    let key = PublicKey::from_bytes(&[0xAB; 32]).expect("key should load");

    // Act
    let secure = pairing_link("wss://relay.example/", &key, "ABCD-EFGH");
    let plain = pairing_link("ws://127.0.0.1:8787", &key, "ABCD-EFGH");

    // Assert
    assert_eq!(
        secure,
        format!(
            "https://relay.example/#relay=wss%3A%2F%2Frelay%2Eexample&key={}&code=ABCD%2DEFGH",
            "ab".repeat(32)
        )
    );
    assert!(plain.starts_with("http://127.0.0.1:8787/#relay=ws%3A%2F%2F127%2E0%2E0%2E1%3A8787&"));
}

#[test]
fn retry_delay_resets_after_a_registered_link_and_keeps_growing_otherwise() {
    // Arrange
    let grown = Duration::from_secs(8);

    // Act
    let after_registered = retry_delay_after(grown, true);
    let after_failure = retry_delay_after(grown, false);

    // Assert
    assert_eq!(after_registered, INITIAL_RETRY_DELAY);
    assert_eq!(after_failure, grown);
}
