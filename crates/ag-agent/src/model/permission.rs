use std::fmt;
use std::str::FromStr;

/// Supported permission mode values for agent execution workflows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum PermissionMode {
    /// Allows the agent to edit files automatically within its sandbox.
    #[default]
    AutoEdit,
    /// Restricts the agent to repository inspection without filesystem writes
    /// or mutating command approvals.
    ReadOnly,
}

impl PermissionMode {
    /// Ordered permission-mode options shown by interactive selectors.
    pub const ALL: [PermissionMode; 2] = [PermissionMode::AutoEdit, PermissionMode::ReadOnly];

    /// Returns the wire label used for persistence and provider invocation.
    pub fn label(self) -> &'static str {
        match self {
            Self::AutoEdit => "auto_edit",
            Self::ReadOnly => "read_only",
        }
    }

    /// Returns the user-facing label shown in the UI.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::AutoEdit => "Auto Edit",
            Self::ReadOnly => "Read Only",
        }
    }

    /// Returns whether the provider must deny repository mutations.
    pub fn is_read_only(self) -> bool {
        self == Self::ReadOnly
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for PermissionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto_edit" => Ok(PermissionMode::AutoEdit),
            "read_only" => Ok(PermissionMode::ReadOnly),
            _ => Err(format!("Unknown permission mode: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let permission_modes = [PermissionMode::AutoEdit, PermissionMode::ReadOnly];

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
}
