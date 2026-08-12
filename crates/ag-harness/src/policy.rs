use crate::tool::Tool;

/// Default-deny permissions for built-in harness tools.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Policy {
    read: bool,
}

impl Policy {
    pub(crate) fn allow(&mut self, tool: Tool) {
        match tool {
            Tool::Read => self.read = true,
        }
    }

    pub(crate) fn allows(self, tool: Tool) -> bool {
        match tool {
            Tool::Read => self.read,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_denies_tools_by_default_and_allows_them_explicitly() {
        // Arrange
        let mut policy = Policy::default();

        // Act
        let denied_by_default = policy.allows(Tool::Read);
        policy.allow(Tool::Read);
        let explicitly_allowed = policy.allows(Tool::Read);

        // Assert
        assert!(!denied_by_default);
        assert!(explicitly_allowed);
    }
}
