use ag_protocol::ProtocolRequestProfile;

use crate::execution_graph::test_support::reader;
use crate::{
    GraphContext, GraphContractError, GraphExecutionProfile, GraphPostProcessing,
    GraphWorkspaceAccess, PermissionMode,
};

#[test]
fn profiles_reject_incompatible_paths_effects_and_blank_identities() {
    // Arrange
    type Mutation = fn(&mut GraphExecutionProfile);
    let cases: &[(Mutation, GraphContractError)] = &[
        (
            |profile| profile.response_profile = ProtocolRequestProfile::SessionTurn,
            GraphContractError::InvalidProfile(
                "fresh utilities require a utility schema and no session effects",
            ),
        ),
        (
            |profile| profile.post_processing = GraphPostProcessing::SessionWorkflow,
            GraphContractError::InvalidProfile(
                "fresh utilities require a utility schema and no session effects",
            ),
        ),
        (
            |profile| {
                profile.context = GraphContext::TaskConversation {
                    task_id: " ".into(),
                }
            },
            GraphContractError::EmptyField("task_id"),
        ),
        (
            |profile| {
                profile.context = GraphContext::TaskConversation {
                    task_id: "task".into(),
                }
            },
            GraphContractError::InvalidProfile(
                "task conversations require the session-turn schema",
            ),
        ),
        (
            |profile| {
                profile.context = GraphContext::TaskConversation {
                    task_id: "task".into(),
                };
                profile.response_profile = ProtocolRequestProfile::SessionTurn;
                profile.post_processing = GraphPostProcessing::SessionWorkflow;
            },
            GraphContractError::InvalidProfile(
                "shared readers cannot perform host mutation effects",
            ),
        ),
        (
            |profile| {
                profile.context = GraphContext::TaskConversation {
                    task_id: "task".into(),
                };
                profile.response_profile = ProtocolRequestProfile::SessionTurn;
                profile.workspace_access = GraphWorkspaceAccess::BackgroundRead;
            },
            GraphContractError::InvalidProfile(
                "background readers require a fresh utility context",
            ),
        ),
        (
            |profile| profile.shared_budget_id = Some("\t".into()),
            GraphContractError::EmptyField("shared_budget_id"),
        ),
    ];

    for (mutate, expected) in cases {
        let mut profile = reader("node").execution;
        mutate(&mut profile);

        // Act
        let result = profile.validate();

        // Assert
        assert_eq!(result, Err(expected.clone()), "{profile:?}");
    }
}

#[test]
fn profiles_preserve_permission_ceilings_for_every_session_mode() {
    // Arrange
    for access in [
        GraphWorkspaceAccess::SharedRead,
        GraphWorkspaceAccess::BackgroundRead,
        GraphWorkspaceAccess::ExclusiveWrite,
    ] {
        for read_only in [false, true] {
            for session_mode in PermissionMode::ALL {
                let mut node = reader("node");
                node.execution.workspace_access = access;
                node.definition.read_only = read_only;
                let expected = if read_only || access != GraphWorkspaceAccess::ExclusiveWrite {
                    PermissionMode::ReadOnly
                } else {
                    session_mode
                };

                // Act
                let resolved = node
                    .execution
                    .permission_mode(&node.definition, session_mode);

                // Assert
                assert_eq!(resolved, expected);
            }
        }
    }
}

#[test]
fn task_workflow_profiles_allow_ordered_effects_and_optional_shared_budgets() {
    // Arrange
    let mut profile = reader("node").execution;
    profile.context = GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    profile.response_profile = ProtocolRequestProfile::SessionTurn;
    profile.post_processing = GraphPostProcessing::SessionWorkflow;
    profile.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    profile.shared_budget_id = None;

    // Act
    let result = profile.validate();

    // Assert
    assert_eq!(result, Ok(()));
}
