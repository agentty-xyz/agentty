use crate::{BuiltinAgentDefinition, PermissionMode};

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
        let planner = BuiltinAgentDefinition::Planner.permission_mode(session_mode);
        let implementer = BuiltinAgentDefinition::Implementer.permission_mode(session_mode);

        // Assert
        assert_eq!(planner, PermissionMode::ReadOnly);
        assert_eq!(implementer, implementer_mode);
    }
}
