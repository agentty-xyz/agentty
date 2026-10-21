use crate::{PermissionMode, TaskAgentRole};

#[test]
fn role_identity_round_trips_and_names_each_role() {
    // Arrange
    let cases = [
        (TaskAgentRole::Planner, "Planner"),
        (TaskAgentRole::Implementer, "Implementer"),
    ];

    for (definition, display_name) in cases {
        // Act
        let parsed = TaskAgentRole::from_id(definition.id());

        // Assert
        assert_eq!(parsed, Some(definition));
        assert_eq!(definition.display_name(), display_name);
    }
    assert_eq!(TaskAgentRole::from_id("reviewer"), None);
}

#[test]
fn role_permissions_never_expand_session_access() {
    // Arrange
    let cases = [
        (PermissionMode::AutoEdit, PermissionMode::AutoEdit),
        (
            PermissionMode::AutoEditAddressComments,
            PermissionMode::AutoEditAddressComments,
        ),
        (PermissionMode::ReadOnly, PermissionMode::ReadOnly),
    ];

    for (session_mode, implementer_mode) in cases {
        // Act
        let planner = TaskAgentRole::Planner.permission_mode(session_mode);
        let implementer = TaskAgentRole::Implementer.permission_mode(session_mode);

        // Assert
        assert_eq!(planner, PermissionMode::ReadOnly);
        assert_eq!(implementer, implementer_mode);
    }
}
