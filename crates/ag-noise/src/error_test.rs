use crate::NoiseError;

#[test]
fn snow_errors_become_protocol_failures_without_their_cause() {
    // Arrange
    let cause = snow::Error::Decrypt;

    // Act
    let error = NoiseError::from(cause);

    // Assert
    assert_eq!(error, NoiseError::Protocol);
    assert_eq!(error.to_string(), "Noise protocol failure");
}

#[test]
fn errors_describe_their_kind() {
    // Arrange
    let errors = [NoiseError::FrameTooLarge, NoiseError::InvalidKey];

    // Act
    let messages = errors.map(|error| error.to_string());

    // Assert
    assert_eq!(messages, ["Frame is too large", "Invalid key"]);
}
