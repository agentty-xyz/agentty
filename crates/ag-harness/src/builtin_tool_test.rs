//! Built-in tool handler unit tests for their definitions, effects, and
//! calls made outside a turn.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::bash::{
    BashConfig, BashExecutor, BashProcess, CommandCleanupScope, Enforcement, ExecutionCommand,
    ExecutionError, ExecutionPolicy, ProcessEvent, UnsandboxedExecutor,
};
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

/// A host executor whose every command fails preparation with `error`.
struct FailingPreparation(ExecutionError);

impl BashExecutor for FailingPreparation {
    fn identity(&self) -> &'static str {
        "failing-preparation"
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::FileSystem
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        Ok(Box::new(FailingProcess(self.0)))
    }
}

struct FailingProcess(ExecutionError);

#[async_trait]
impl BashProcess for FailingProcess {
    async fn prepare(
        &mut self,
        _command: &ExecutionCommand,
        policy: &ExecutionPolicy,
    ) -> Result<(), ExecutionError> {
        assert_eq!(
            policy.environment().get(std::ffi::OsStr::new("TMPDIR")),
            Some(&"/private/tmp/session".into())
        );
        assert!(policy.external_reads().len() > 1, "system reads");

        Err(self.0)
    }

    async fn start(&mut self) -> Result<(), ExecutionError> {
        Err(ExecutionError::Process)
    }

    async fn next_event(&mut self, _buffer: &mut [u8]) -> Result<ProcessEvent, ExecutionError> {
        Err(ExecutionError::Process)
    }

    async fn cleanup(&mut self) -> Result<(), ExecutionError> {
        Ok(())
    }
}

#[tokio::test]
async fn a_command_the_sandbox_refuses_is_rejected_and_other_failures_fail_the_turn() {
    for (error, rejected) in [
        (ExecutionError::Unsupported, true),
        (ExecutionError::Setup, false),
    ] {
        // Arrange
        let configuration = BashConfig::for_executor(
            Arc::new(FailingPreparation(error)),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(5),
            1024,
        )
        .expect("configuration")
        .with_host_information()
        .with_system_reads()
        .with_read("/runtime".into())
        .expect("read grant")
        .with_temporary_directory("/private/tmp/session".into())
        .expect("temporary directory");
        let handler = BashHandler {
            definition: ToolDefinition::bash(Enforcement::FileSystem),
            tool: BashTool::new(
                configuration,
                "/workspace".into(),
                Commands::default(),
                None,
            )
            .expect("bash tool"),
        };

        // Act
        let error = handler
            .call(json!({"command": "true"}), ToolContext::new("bash_1"))
            .await
            .expect_err("preparation fails");

        // Assert
        assert_eq!(error.is_rejected(), rejected, "{error}");
        if rejected {
            assert!(error.to_string().contains(SANDBOX_REFUSED), "{error}");
        }
    }
}

#[test]
fn only_an_unstarted_command_without_cleanup_debt_counts_as_refused() {
    // Arrange
    let refused_outcome = CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        execution_failure: Some(BashError::Unavailable),
        exit_code: None,
        signal: None,
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        termination: CommandTermination::Failed,
        truncated: false,
    };

    // Act / Assert
    assert!(refused(&refused_outcome));
    assert!(!refused(&CommandOutcome {
        cleanup_failed: true,
        ..refused_outcome.clone()
    }));
    assert!(!refused(&CommandOutcome {
        termination: CommandTermination::Cancelled,
        ..refused_outcome.clone()
    }));
    assert!(!refused(&CommandOutcome {
        execution_failure: Some(BashError::Execution),
        ..refused_outcome
    }));
}

/// Executor that claims filesystem isolation but runs commands as the
/// unsandboxed executor does, so a test controls what a "sandboxed"
/// command prints.
struct ClaimedSandbox;

impl BashExecutor for ClaimedSandbox {
    fn identity(&self) -> &'static str {
        "claimed-sandbox"
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::FileSystem
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        UnsandboxedExecutor::without_isolation().bind()
    }
}

#[tokio::test]
async fn a_sandboxed_failure_that_reads_like_a_denial_is_flagged_for_escalation() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let configuration = BashConfig::for_executor(
        Arc::new(ClaimedSandbox),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("configuration")
    .with_host_information()
    .with_full_disk_read()
    .with_excluded_command("printf".into())
    .expect("excluded command");
    let handler = BashHandler {
        definition: ToolDefinition::bash(Enforcement::FileSystem),
        tool: BashTool::new(
            configuration,
            root.path().to_path_buf(),
            Commands::default(),
            None,
        )
        .expect("bash tool"),
    };
    let denial = "echo 'touch: /outside: Operation not permitted' >&2; exit 1";

    // Act
    let blocked = handler
        .call(json!({"command": denial}), ToolContext::new("bash_1"))
        .await
        .expect("blocked command");
    let failed = handler
        .call(json!({"command": "exit 3"}), ToolContext::new("bash_2"))
        .await
        .expect("failed command");
    // Bash's `printf` fails with "printf: Permission denied: invalid number".
    let excluded = handler
        .call(
            json!({"command": "printf '%d' 'Permission denied'"}),
            ToolContext::new("bash_3"),
        )
        .await
        .expect("excluded command");
    let sandboxed = handler
        .call(
            json!({"command": "command printf '%d' 'Permission denied'"}),
            ToolContext::new("bash_4"),
        )
        .await
        .expect("sandboxed command");

    // Assert
    let (content, activity) = blocked.into_parts();
    let result: Value = serde_json::from_str(&content).expect("JSON result");
    assert_eq!(result["sandbox"], SANDBOX_BLOCKED);
    assert_eq!(result["exit_code"], 1);
    assert!(matches!(
        activity,
        Some(ToolActivity::BashSandboxDenied { .. })
    ));
    let (content, activity) = failed.into_parts();
    assert!(!content.contains("\"sandbox\""), "{content}");
    assert!(matches!(activity, Some(ToolActivity::Bash { .. })));
    // An excluded command runs without isolation, so the sandbox cannot
    // have blocked it.
    let (content, activity) = excluded.into_parts();
    assert!(content.contains("invalid number"), "{content}");
    assert!(matches!(activity, Some(ToolActivity::Bash { .. })));
    assert!(matches!(
        sandboxed.into_parts().1,
        Some(ToolActivity::BashSandboxDenied { .. })
    ));
}

#[test]
fn only_a_failed_command_with_denial_output_counts_as_blocked() {
    // Arrange
    let failed = CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        execution_failure: None,
        exit_code: Some(1),
        signal: None,
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        termination: CommandTermination::Completed,
        truncated: false,
    };

    // Act / Assert
    assert!(!blocked(&failed));
    assert!(blocked(&CommandOutcome {
        stderr_tail: "mkdir: /x: Read-only file system".into(),
        ..failed.clone()
    }));
    assert!(blocked(&CommandOutcome {
        stdout: "Sandbox: deny file-write-create".into(),
        ..failed.clone()
    }));
    assert!(!blocked(&CommandOutcome {
        exit_code: Some(0),
        stderr: "Permission denied".into(),
        ..failed.clone()
    }));
    assert!(!blocked(&CommandOutcome {
        exit_code: None,
        signal: Some(9),
        stderr: "Permission denied".into(),
        ..failed
    }));
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
