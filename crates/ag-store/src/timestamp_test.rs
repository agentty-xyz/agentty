use crate::timestamp::{TimestampSource, system_timestamp_source};

#[test]
fn closure_timestamp_source_returns_injected_value() {
    // Arrange
    let timestamp_source = || 123;

    // Act
    let timestamp = timestamp_source.now_timestamp_seconds();

    // Assert
    assert_eq!(timestamp, 123);
}

#[test]
fn system_timestamp_source_returns_a_post_epoch_value() {
    // Arrange
    let timestamp_source = system_timestamp_source();

    // Act
    let timestamp = timestamp_source.now_timestamp_seconds();

    // Assert
    assert!(timestamp > 0);
}
