use serde::{Deserialize, Serialize};

use crate::tool::Tool;

/// Default-deny permissions for built-in harness tools.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicy {
    read: bool,
    write: bool,
}

impl ToolPolicy {
    /// Returns a policy that explicitly allows `tool`.
    #[must_use]
    pub fn allow(mut self, tool: Tool) -> Self {
        match tool {
            Tool::Read => self.read = true,
            Tool::Write => self.write = true,
        }

        self
    }

    /// Returns a policy that denies `tool`, including a previously allowed
    /// tool.
    #[must_use]
    pub fn deny(mut self, tool: Tool) -> Self {
        match tool {
            Tool::Read => self.read = false,
            Tool::Write => self.write = false,
        }

        self
    }

    /// Returns whether this policy permits executing and advertising `tool`.
    pub fn allows(self, tool: Tool) -> bool {
        match tool {
            Tool::Read => self.read,
            Tool::Write => self.write,
        }
    }
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
