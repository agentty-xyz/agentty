use super::ColorTheme;

#[test]
fn parse_persisted_returns_current_theme() {
    // Arrange
    let stored_value = "current";

    // Act
    let theme = ColorTheme::parse_persisted(stored_value);

    // Assert
    assert_eq!(theme, Some(ColorTheme::Current));
}

#[test]
fn parse_persisted_returns_green_theme() {
    // Arrange
    let stored_value = "green";

    // Act
    let theme = ColorTheme::parse_persisted(stored_value);

    // Assert
    assert_eq!(theme, Some(ColorTheme::Green));
}

#[test]
fn parse_persisted_rejects_unknown_theme() {
    // Arrange
    let stored_value = "unknown";

    // Act
    let theme = ColorTheme::parse_persisted(stored_value);

    // Assert
    assert_eq!(theme, None);
}

#[test]
fn next_cycles_between_available_themes() {
    // Arrange
    let current_theme = ColorTheme::Current;
    let green_theme = ColorTheme::Green;
    let dark_horizon_theme = ColorTheme::DarkHorizon;

    // Act
    let next_theme = current_theme.next();
    let after_green_theme = green_theme.next();
    let wrapped_theme = dark_horizon_theme.next();

    // Assert
    assert_eq!(next_theme, ColorTheme::Green);
    assert_eq!(after_green_theme, ColorTheme::DarkHorizon);
    assert_eq!(wrapped_theme, ColorTheme::Current);
}

#[test]
fn parse_persisted_returns_dark_horizon_theme() {
    // Arrange
    let stored_value = "dark_horizon";

    // Act
    let theme = ColorTheme::parse_persisted(stored_value);

    // Assert
    assert_eq!(theme, Some(ColorTheme::DarkHorizon));
}
