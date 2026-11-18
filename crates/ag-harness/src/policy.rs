//! Deprecated default-deny permissions for built-in harness tools.
//!
//! A turn offers the tools its [`crate::ToolSet`] holds; a policy given to a
//! deprecated setter only drops the built-ins it denies from that set.
#![expect(
    deprecated,
    reason = "the deprecated policy maps the deprecated `Tool`"
)]

use serde::{Deserialize, Serialize};

use crate::tool::Tool;
use crate::tool_handler::BuiltinPermissions;

/// Default-deny permissions for built-in harness tools.
///
/// A policy given to a deprecated setter drops the built-ins it denies from
/// the turn's [`crate::ToolSet`]; select the built-ins in the set instead.
#[deprecated(
    since = "0.20.0",
    note = "select built-in tools with `ToolSet`; a turn offers the tools its set holds"
)]
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ToolPolicy {
    permissions: BuiltinPermissions,
}

impl ToolPolicy {
    /// Returns a policy that explicitly allows `tool`.
    #[must_use]
    pub fn allow(self, tool: Tool) -> Self {
        Self {
            permissions: self.permissions.allow(tool.into()),
        }
    }

    /// Returns a policy that denies `tool`, including a previously allowed
    /// tool.
    #[must_use]
    pub fn deny(self, tool: Tool) -> Self {
        Self {
            permissions: self.permissions.deny(tool.into()),
        }
    }

    /// Returns whether this policy permits executing and advertising `tool`.
    pub fn allows(self, tool: Tool) -> bool {
        self.permissions.allows(tool.into())
    }
}

impl From<ToolPolicy> for BuiltinPermissions {
    fn from(policy: ToolPolicy) -> Self {
        policy.permissions
    }
}

impl From<BuiltinPermissions> for ToolPolicy {
    fn from(permissions: BuiltinPermissions) -> Self {
        Self { permissions }
    }
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
