use std::time::Duration;

use super::{INITIAL_RETRY_DELAY, retry_delay_after};

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
