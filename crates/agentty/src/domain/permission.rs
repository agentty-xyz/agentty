use std::fmt;
use std::str::FromStr;

/// Supported permission mode values for agent execution workflows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum PermissionMode {
    #[default]
    AutoEdit,
}

impl PermissionMode {
    /// Returns the wire label used for persistence and display.
    pub fn label(self) -> &'static str {
        "auto_edit"
    }

    /// Returns the user-facing label shown in the UI.
    pub fn display_label(self) -> &'static str {
        "Auto Edit"
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
            _ => Err(format!("Unknown permission mode: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_accepts_auto_edit() {
        // Arrange
        let permission_mode = "auto_edit";

        // Act
        let parsed_permission_mode = PermissionMode::from_str(permission_mode);

        // Assert
        assert_eq!(parsed_permission_mode, Ok(PermissionMode::AutoEdit));
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
        let permission_mode = PermissionMode::AutoEdit;

        // Act
        let label = permission_mode.label();
        let display_label = permission_mode.display_label();

        // Assert
        assert_eq!(label, "auto_edit");
        assert_eq!(display_label, "Auto Edit");
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
