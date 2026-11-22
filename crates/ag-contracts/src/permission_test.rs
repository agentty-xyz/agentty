//! `PermissionMode` parsing, default, and label tests.

use std::str::FromStr;

use crate::permission::PermissionMode;

#[test]
fn test_from_str_accepts_supported_modes() {
    // Arrange
    let permission_modes = ["auto_edit", "read_only"];

    // Act
    let parsed_permission_modes = permission_modes.map(PermissionMode::from_str);

    // Assert
    assert_eq!(
        parsed_permission_modes,
        [Ok(PermissionMode::AutoEdit), Ok(PermissionMode::ReadOnly)]
    );
}

#[test]
fn test_from_str_rejects_removed_permission_modes() {
    // Arrange
    let removed_modes = ["autonomous", "auto_edit_address_comments"];

    // Act
    let parsed_permission_modes = removed_modes.map(PermissionMode::from_str);

    // Assert
    assert_eq!(
        parsed_permission_modes,
        [
            Err("Unknown permission mode: autonomous".to_string()),
            Err("Unknown permission mode: auto_edit_address_comments".to_string()),
        ]
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
    let read_only = permission_modes.map(PermissionMode::is_read_only);

    // Assert
    assert_eq!(labels, ["auto_edit", "read_only"]);
    assert_eq!(display_labels, ["Auto Edit", "Read Only"]);
    assert_eq!(read_only, [false, true]);
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
