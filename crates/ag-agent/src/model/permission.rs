use std::fmt;
use std::str::FromStr;

/// Supported permission mode values for agent execution workflows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum PermissionMode {
    /// Allows the agent to edit files automatically within its sandbox.
    #[default]
    AutoEdit,
    /// Allows edits and automatically applies actionable focused-review
    /// suggestions after completed turns.
    AutoEditAddressComments,
    /// Restricts the agent to repository inspection without filesystem writes
    /// or mutating command approvals.
    ReadOnly,
}

impl PermissionMode {
    /// Ordered permission-mode options shown by interactive selectors.
    pub const ALL: [PermissionMode; 3] = [
        PermissionMode::AutoEdit,
        PermissionMode::AutoEditAddressComments,
        PermissionMode::ReadOnly,
    ];

    /// Returns explanatory text shown by interactive selectors.
    pub fn description(self) -> &'static str {
        match self {
            Self::AutoEdit => "Allow the agent to edit files automatically.",
            Self::AutoEditAddressComments => {
                "Auto Edit, then address focused-review suggestions up to 3 times."
            }
            Self::ReadOnly => "Inspect the repository without changing files.",
        }
    }

    /// Returns the wire label used for persistence and provider invocation.
    pub fn label(self) -> &'static str {
        match self {
            Self::AutoEdit => "auto_edit",
            Self::AutoEditAddressComments => "auto_edit_address_comments",
            Self::ReadOnly => "read_only",
        }
    }

    /// Returns the user-facing label shown in the UI.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::AutoEdit => "Auto Edit",
            Self::AutoEditAddressComments => "Auto Edit + Auto Address Comments",
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
            "auto_edit_address_comments" => Ok(PermissionMode::AutoEditAddressComments),
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
}
