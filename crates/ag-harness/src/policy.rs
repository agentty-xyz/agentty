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
#[path = "policy_test.rs"]
mod tests;
