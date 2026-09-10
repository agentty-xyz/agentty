use super::Policy;
use crate::tool::Tool;

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
