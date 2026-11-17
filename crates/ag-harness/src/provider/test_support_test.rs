use std::io;

use super::reqwest_source;

#[test]
fn returns_none_without_reqwest_in_source_chain() {
    // Arrange
    let error = io::Error::other("plain error");

    // Act
    let source = reqwest_source(&error);

    // Assert
    assert!(source.is_none());
}
