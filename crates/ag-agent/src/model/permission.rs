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
#[path = "permission_test.rs"]
mod tests;
