use std::str::FromStr;

use crate::model::permission::PermissionMode;

#[test]
fn test_from_str_accepts_supported_modes() {
    // Arrange
    let permission_modes = ["auto_edit", "auto_edit_address_comments", "read_only"];

    // Act
    let parsed_permission_modes = permission_modes.map(PermissionMode::from_str);

    // Assert
    assert_eq!(
        parsed_permission_modes,
        [
            Ok(PermissionMode::AutoEdit),
            Ok(PermissionMode::AutoEditAddressComments),
            Ok(PermissionMode::ReadOnly),
        ]
    );
}

#[test]
fn test_from_str_rejects_removed_permission_modes() {
    // Arrange
    let removed_mode = "autonomous";

    // Act
    let parsed_permission_mode = PermissionMode::from_str(removed_mode);

    // Assert
    assert_eq!(
        parsed_permission_mode,
        Err("Unknown permission mode: autonomous".to_string())
    );
}

#[test]
fn test_default_uses_auto_edit_mode() {
    // Arrange, Act
    let permission_mode = PermissionMode::default();

    // Assert
    assert_eq!(permission_mode, PermissionMode::AutoEdit);
}

#[test]
fn test_label_and_display_label_return_persisted_and_user_facing_text() {
    // Arrange
    let permission_modes = PermissionMode::ALL;

    // Act
    let labels = permission_modes.map(PermissionMode::label);
    let display_labels = permission_modes.map(PermissionMode::display_label);
    let descriptions = permission_modes.map(PermissionMode::description);
    let read_only = permission_modes.map(PermissionMode::is_read_only);

    // Assert
    assert_eq!(
        labels,
        ["auto_edit", "auto_edit_address_comments", "read_only"]
    );
    assert_eq!(
        display_labels,
        [
            "Auto Edit",
            "Auto Edit + Auto Address Comments",
            "Read Only"
        ]
    );
    assert_eq!(
        descriptions,
        [
            "Allow the agent to edit files automatically.",
            "Auto Edit, then address focused-review suggestions up to 3 times.",
            "Inspect the repository without changing files.",
        ]
    );
    assert_eq!(read_only, [false, false, true]);
}

#[test]
fn test_display_uses_persisted_label() {
    // Arrange
    let permission_mode = PermissionMode::AutoEdit;

    // Act
    let formatted = permission_mode.to_string();

    // Assert
    assert_eq!(formatted, "auto_edit");
}
