use crate::policy::ToolPolicy;
use crate::tool::Tool;

#[test]
fn policy_denies_tools_by_default_and_allows_them_explicitly() {
    // Arrange
    let mut policy = ToolPolicy::default();

    // Act
    let read_denied_by_default = policy.allows(Tool::Read);
    let write_denied_by_default = policy.allows(Tool::Write);
    policy = policy.allow(Tool::Read);
    policy = policy.allow(Tool::Write);

    // Assert
    assert!(!read_denied_by_default);
    assert!(!write_denied_by_default);
    assert!(policy.allows(Tool::Read));
    assert!(policy.allows(Tool::Write));
}

#[test]
fn policy_revokes_previously_allowed_tools_without_mutating_the_original() {
    // Arrange
    let allowed = ToolPolicy::default().allow(Tool::Read).allow(Tool::Write);

    // Act
    let denied = allowed.deny(Tool::Read).deny(Tool::Write);

    // Assert
    assert!(allowed.allows(Tool::Read));
    assert!(allowed.allows(Tool::Write));
    assert!(!denied.allows(Tool::Read));
    assert!(!denied.allows(Tool::Write));
}
