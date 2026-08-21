use crate::tool::Tool;

/// Default-deny permissions for built-in harness tools.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Policy {
    read: bool,
    write: bool,
}

impl Policy {
    pub(crate) fn allow(&mut self, tool: Tool) {
        match tool {
            Tool::Read => self.read = true,
            Tool::Write => self.write = true,
        }
    }

    pub(crate) fn allows(self, tool: Tool) -> bool {
        match tool {
            Tool::Read => self.read,
            Tool::Write => self.write,
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
        let read_denied_by_default = policy.allows(Tool::Read);
        let write_denied_by_default = policy.allows(Tool::Write);
        policy.allow(Tool::Read);
        policy.allow(Tool::Write);

        // Assert
        assert!(!read_denied_by_default);
        assert!(!write_denied_by_default);
        assert!(policy.allows(Tool::Read));
        assert!(policy.allows(Tool::Write));
    }
}
