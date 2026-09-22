use super::MouseSupport;

#[test]
fn default_enables_mouse_capture() {
    // Arrange & Act
    let mouse_support = MouseSupport::default();

    // Assert
    assert!(mouse_support.is_enabled());
}

#[test]
fn from_enabled_maps_switch_values() {
    // Arrange & Act
    let enabled = MouseSupport::from_enabled(true);
    let disabled = MouseSupport::from_enabled(false);

    // Assert
    assert_eq!(enabled, MouseSupport::Enabled);
    assert_eq!(disabled, MouseSupport::Disabled);
}

#[test]
fn parse_persisted_round_trips_wire_values() {
    // Arrange
    let variants = [MouseSupport::Enabled, MouseSupport::Disabled];

    // Act & Assert
    for variant in variants {
        assert_eq!(
            MouseSupport::parse_persisted(variant.as_str()),
            Some(variant)
        );
    }
}

#[test]
fn parse_persisted_rejects_unknown_values() {
    // Arrange
    let stored_value = "sometimes";

    // Act
    let parsed = MouseSupport::parse_persisted(stored_value);

    // Assert
    assert_eq!(parsed, None);
}
