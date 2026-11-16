//! Built-in tool handler unit tests for their definitions, effects, and
//! calls made outside a turn.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::bash::{BashConfig, Enforcement, UnsandboxedExecutor};
use crate::command_settlement::Commands;
use crate::file_system::LocalFileSystem;

/// Returns a `read` handler over `root` whose Git executable does not
/// exist: file reads never spawn Git.
fn read_handler(root: &std::path::Path) -> ReadHandler {
    ReadHandler {
        definition: ToolDefinition::read_with_comparison_base(None),
        tool: ReadTool::with_git(
            Arc::new(LocalFileSystem),
            root.to_path_buf(),
            "/nonexistent/git".into(),
            None,
        ),
    }
}

fn bash_handler() -> BashHandler {
    let configuration = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("configuration")
    .with_host_information();

    BashHandler {
        definition: ToolDefinition::bash(Enforcement::None),
        tool: BashTool::new(
            configuration,
            "/workspace".into(),
            Commands::default(),
            None,
        )
        .expect("bash tool"),
    }
}

// Binding `bash` captures the current Tokio runtime, so the test needs one.
#[tokio::test]
async fn built_in_handlers_advertise_their_definitions_and_effects() {
    // Arrange
    let root = std::path::Path::new("/workspace");
    let read = read_handler(root);
    let write = WriteHandler {
        tool: WriteTool::new(Arc::new(LocalFileSystem), root.to_path_buf()),
    };
    let bash = bash_handler();

    // Act / Assert
    assert_eq!(
        read.definition(),
        ToolDefinition::read_with_comparison_base(None)
    );
    assert_eq!(read.effects(), ToolEffects::ReadOnly);
    assert_eq!(write.definition(), ToolDefinition::write());
    assert_eq!(write.effects(), ToolEffects::Writes);
    assert_eq!(bash.definition(), ToolDefinition::bash(Enforcement::None));
    assert_eq!(bash.effects(), ToolEffects::Executes);
}

#[tokio::test]
async fn a_read_outside_a_turn_is_bounded_only_by_the_result_limit() {
    // Arrange
    let root = tempfile::tempdir().expect("temporary directory");
    std::fs::write(root.path().join("notes.txt"), "first\nsecond\n").expect("file");
    let handler = read_handler(root.path());

    // Act
    let output = handler
        .call(json!({"path": "notes.txt"}), ToolContext::new("read_1"))
        .await
        .expect("read output");

    // Assert
    let (content, activity) = output.into_parts();
    assert!(content.contains("second"), "{content}");
    assert!(matches!(
        activity,
        Some(ToolActivity::Read { path, truncated: false, .. }) if path == "notes.txt"
    ));
}

#[test]
fn an_unencodable_bash_outcome_fails_the_turn_as_an_execution_error() {
    // Arrange
    let error = serde_json::from_str::<Value>("{").expect_err("invalid JSON");

    // Act
    let error = unencodable_outcome(error);

    // Assert
    assert!(!error.is_rejected());
    assert_eq!(
        error.to_string(),
        TurnError::Bash(BashError::Execution).to_string()
    );
}

#[tokio::test]
async fn arguments_a_built_in_cannot_decode_are_rejected() {
    // Arrange
    let root = tempfile::tempdir().expect("temporary directory");
    let handler = WriteHandler {
        tool: WriteTool::new(Arc::new(LocalFileSystem), root.path().to_path_buf()),
    };

    // Act
    let error = handler
        .call(json!({"path": "notes.txt"}), ToolContext::new("write_1"))
        .await
        .expect_err("missing patch");

    // Assert
    assert!(error.is_rejected());
    assert!(
        error
            .to_string()
            .starts_with("tool call rejected: failed to parse function arguments: "),
        "{error}"
    );
}
