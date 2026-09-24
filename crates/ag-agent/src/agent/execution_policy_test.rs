use std::num::NonZeroUsize;
use std::process::Command;

use ag_contracts::{ExecutionPolicy, McpPolicy, PermissionMode, ToolPolicy};
use ag_session::AgentKind;

use crate::agent::execution_policy::{apply_claude_tools, validate};

#[test]
fn unsupported_explicit_controls_fail_instead_of_becoming_hints() {
    // Arrange
    let policies = [
        ExecutionPolicy {
            max_concurrent_subagents: NonZeroUsize::new(4),
            ..ExecutionPolicy::default()
        },
        ExecutionPolicy {
            tools: ToolPolicy::AllowOnly(vec![]),
            ..ExecutionPolicy::default()
        },
        ExecutionPolicy {
            mcp: McpPolicy::Disabled,
            ..ExecutionPolicy::default()
        },
    ];
    for kind in AgentKind::ALL {
        // Act / Assert
        assert!(validate(*kind, &ExecutionPolicy::default()).is_ok());
        for (index, policy) in policies.iter().enumerate() {
            let result = validate(*kind, policy);
            let supported = *kind == AgentKind::Claude || (*kind == AgentKind::Codex && index == 0);
            assert_eq!(result.is_ok(), supported, "{kind}: {policy:?}");
            if let Err(error) = result {
                assert!(error.to_string().contains("does not support the requested"));
            }
        }
    }
}

#[test]
fn claude_tool_policy_controls_availability_and_preserves_read_only_permissions() {
    // Arrange
    let cases = [
        (ToolPolicy::Inherit, PermissionMode::AutoEdit, vec![]),
        (
            ToolPolicy::AutoApprove(vec!["Bash".into()]),
            PermissionMode::AutoEdit,
            vec!["--allowedTools", "Bash"],
        ),
        (
            ToolPolicy::AllowOnly(vec![]),
            PermissionMode::AutoEdit,
            vec!["--tools", "", "--allowedTools", ""],
        ),
        (
            ToolPolicy::AllowOnly(vec!["Read".into(), "Bash".into()]),
            PermissionMode::AutoEdit,
            vec!["--tools", "Read,Bash", "--allowedTools", "Read,Bash"],
        ),
        (
            ToolPolicy::AllowOnly(vec!["Read".into(), "Bash".into()]),
            PermissionMode::ReadOnly,
            vec![
                "--tools",
                "Read",
                "--allowedTools",
                "Read",
                "--permission-mode",
                "plan",
            ],
        ),
        (
            ToolPolicy::AutoApprove(vec!["Write".into()]),
            PermissionMode::ReadOnly,
            vec![
                "--tools",
                "Read,Glob,Grep,WebSearch,WebFetch",
                "--allowedTools",
                "Read,Glob,Grep,WebSearch,WebFetch",
                "--permission-mode",
                "plan",
            ],
        ),
    ];
    for (policy, permissions, expected) in cases {
        let mut command = Command::new("claude");
        // Act
        apply_claude_tools(&mut command, &policy, permissions);
        // Assert
        assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
    }
}
