use super::{Icon, QUEUED_ACTION_GLYPH, TACHYON_LOADER_GLYPH};

#[test]
fn test_as_str() {
    // Arrange & Act & Assert
    assert_eq!(Icon::ArrowDown.as_str(), "↓");
    assert_eq!(Icon::ArrowUp.as_str(), "↑");
    assert_eq!(Icon::Check.as_str(), "✓");
    assert_eq!(Icon::Cross.as_str(), "✗");
    assert_eq!(Icon::GitBranch.as_str(), "●");
    assert_eq!(Icon::Pending.as_str(), "·");
    assert_eq!(Icon::QueuedAction.as_str(), QUEUED_ACTION_GLYPH);
    assert_eq!(Icon::TachyonLoader.as_str(), TACHYON_LOADER_GLYPH);
    assert_eq!(Icon::Warn.as_str(), "!");
}

#[test]
fn test_current_spinner() {
    // Arrange & Act
    let icon = Icon::current_spinner();

    // Assert
    assert!(matches!(icon, Icon::Spinner));
    assert_eq!(icon.as_str(), TACHYON_LOADER_GLYPH);
}

#[test]
fn test_spinner_frame_from_millis() {
    // Arrange, Act, Assert
    assert_eq!(Icon::spinner_frame_from_millis(0), 0);
    assert_eq!(Icon::spinner_frame_from_millis(99), 0);
    assert_eq!(Icon::spinner_frame_from_millis(100), 1);
}

#[test]
fn test_spinner_uses_tachyon_loader_glyph() {
    // Arrange & Act & Assert
    assert_eq!(Icon::Spinner.as_str(), TACHYON_LOADER_GLYPH);
}

#[test]
fn test_display_matches_as_str() {
    // Arrange
    let icons = [
        Icon::ArrowDown,
        Icon::ArrowUp,
        Icon::Check,
        Icon::Cross,
        Icon::GitBranch,
        Icon::Pending,
        Icon::QueuedAction,
        Icon::TachyonLoader,
        Icon::Spinner,
        Icon::Warn,
    ];

    // Act & Assert
    for icon in icons {
        assert_eq!(format!("{icon}"), icon.as_str());
    }
}
