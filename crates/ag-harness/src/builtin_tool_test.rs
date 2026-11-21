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

    fn supports_hidden_paths(&self) -> bool {
        true
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        UnsandboxedExecutor::without_isolation().bind()
    }
}

/// Returns a context for a call the approval handler allowed with
/// `effects`.
fn approved(call_id: &str, effects: ToolEffects) -> ToolContext {
    let mut context = ToolContext::new(call_id);
    context.approved = Some(effects);

    context
}

#[tokio::test]
async fn excluded_commands_leave_the_sandbox_only_once_approved() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    // A program the model planted in the workspace must never run instead.
    std::fs::write(root.path().join("printf"), "#!/bin/sh\ntouch planted-ran\n")
        .expect("planted program");
    std::fs::set_permissions(
        root.path().join("printf"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("mode");
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
    .expect("full-disk read")
    .with_excluded_command("/usr/bin/printf".into())
    .expect("excluded command");
    let handler = BashHandler {
        definition: ToolDefinition::bash_for(&configuration),
        tool: BashTool::new(
            configuration,
            root.path().to_path_buf(),
            Commands::default(),
            None,
        )
        .expect("bash tool"),
    };
    // `printf` fails on a non-numeric argument, naming it on stderr.
    let command = json!({"command": "printf '%d' 'Permission denied'"});

    // Act
    let unapproved = handler
        .call(command.clone(), ToolContext::new("bash_1"))
        .await
        .expect("unapproved command");
    let approved_unsandboxed = handler
        .call(
            command.clone(),
            approved("bash_2", ToolEffects::ExecutesUnsandboxed),
        )
        .await
        .expect("approved command");
    let approved_sandboxed = handler
        .call(command.clone(), approved("bash_3", ToolEffects::Executes))
        .await
        .expect("approved as sandboxed");

    // Assert
    assert_eq!(
        handler.call_effects(&command),
        ToolEffects::ExecutesUnsandboxed
    );
    assert_eq!(
        handler.call_effects(&json!({"command": "printf x; true"})),
        ToolEffects::Executes
    );
    assert_eq!(handler.call_effects(&json!({})), ToolEffects::Executes);
    assert!(
        handler
            .definition()
            .description()
            .contains("Programs excluded from the sandbox: `printf`."),
        "{:?}",
        handler.definition()
    );
    // Unapproved, the command runs sandboxed, where its denial-like error
    // is flagged; approved, it runs unsandboxed and never is.
    assert!(matches!(
        unapproved.into_parts().1,
        Some(ToolActivity::BashLikelySandboxDenial { .. })
    ));
    let (content, activity) = approved_unsandboxed.into_parts();
    assert!(content.contains("Permission denied"), "{content}");
    assert!(matches!(activity, Some(ToolActivity::Bash { .. })));
    assert!(matches!(
        approved_sandboxed.into_parts().1,
        Some(ToolActivity::BashLikelySandboxDenial { .. })
    ));
    assert!(!root.path().join("planted-ran").exists());
}

/// Writes an executable script named `name` in `directory` that prints
/// `output`.
fn script(directory: &std::path::Path, name: &str, output: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, format!("#!/bin/sh\necho {output}\n")).expect("script");
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("mode");

    path
}

#[tokio::test]
async fn excluded_commands_run_the_configured_executable_never_a_planted_one() {
    // Arrange
    let host = tempfile::tempdir().expect("host programs");
    let workspace = tempfile::tempdir().expect("workspace");
    let cache = tempfile::tempdir().expect("writable root");
    let tool = script(host.path(), "tool", "configured");
    // Without a granted `PATH`, Bash's default one ends in the working
    // directory, where a sandboxed command could plant its own `tool`.
    script(workspace.path(), "tool", "planted");
    let configuration = BashConfig::for_executor(
        Arc::new(ClaimedSandbox),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("configuration")
    .with_host_information();
    let handler = BashHandler {
        definition: ToolDefinition::bash(Enforcement::FileSystem),
        tool: BashTool::new(
            configuration
                .clone()
                .with_excluded_command(tool)
                .expect("excluded command"),
            workspace.path().to_path_buf(),
            Commands::default(),
            None,
        )
        .expect("bash tool"),
    };
    let planted_in_root = configuration
        .clone()
        .with_writable_root(cache.path().to_path_buf())
        .expect("writable root")
        .with_excluded_command(script(cache.path(), "shim", "shim"))
        .expect("excluded command");
    let planted_in_workspace = configuration
        .with_excluded_command(script(workspace.path(), "local", "local"))
        .expect("excluded command");

    // Act
    let output = handler
        .call(
            json!({"command": "tool"}),
            approved("bash_1", ToolEffects::ExecutesUnsandboxed),
        )
        .await
        .expect("excluded command");

    // Assert
    let result: Value = serde_json::from_str(output.content()).expect("JSON result");
    assert_eq!(result["stdout"], "configured\n");
    for configuration in [planted_in_root, planted_in_workspace] {
        assert!(matches!(
            BashTool::new(
                configuration,
                workspace.path().to_path_buf(),
                Commands::default(),
                None
            ),
            Err(BashError::InvalidPolicy)
        ));
    }
}

#[tokio::test]
async fn a_sandboxed_failure_that_reads_like_a_denial_is_flagged_as_advisory() {
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
    .with_host_information();
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
    let flagged = handler
        .call(json!({"command": denial}), ToolContext::new("bash_1"))
        .await
        .expect("flagged command");
    let failed = handler
        .call(json!({"command": "exit 3"}), ToolContext::new("bash_2"))
        .await
        .expect("failed command");

    // Assert
    let (content, activity) = flagged.into_parts();
    let result: Value = serde_json::from_str(&content).expect("JSON result");
    assert_eq!(result["sandbox"], LIKELY_SANDBOX_DENIAL);
    assert_eq!(result["exit_code"], 1);
    assert!(matches!(
        activity,
        Some(ToolActivity::BashLikelySandboxDenial { .. })
    ));
    let (content, activity) = failed.into_parts();
    assert!(!content.contains("\"sandbox\""), "{content}");
    assert!(matches!(activity, Some(ToolActivity::Bash { .. })));
}

/// Which stream of a command outcome a detector case writes.
#[derive(Clone, Copy, Debug)]
enum Stream {
    Stderr,
    StderrTail,
    Stdout,
    StdoutTail,
}

/// One denial detector case: the stream written, its text, the exit code,
/// the terminating signal, and whether the detector flags it.
type DetectorCase = (Stream, &'static str, Option<i32>, Option<i32>, bool);

/// Whether this platform's sandbox kills processes with seccomp.
const LINUX: bool = cfg!(target_os = "linux");

/// Every signal the denial detector reads, with its known false positives
/// and false negatives.
const DETECTOR_CASES: [DetectorCase; 18] = [
    // What the sandboxes cause: Seatbelt and seccomp `EPERM`, Landlock
    // `EACCES`, and read-only mounts' `EROFS`, in any case.
    (
        Stream::Stderr,
        "touch: /x: Operation not permitted",
        Some(1),
        None,
        true,
    ),
    (
        Stream::Stderr,
        "cannot create /x: Permission denied",
        Some(1),
        None,
        true,
    ),
    (
        Stream::Stderr,
        "mkdir: /x: Read-only file system",
        Some(1),
        None,
        true,
    ),
    (
        Stream::Stderr,
        "OPERATION NOT PERMITTED",
        Some(2),
        None,
        true,
    ),
    // A marker past truncation, in the retained end of stderr.
    (
        Stream::StderrTail,
        "Read-only file system",
        Some(1),
        None,
        true,
    ),
    // A seccomp kill, which Linux reports as `SIGSYS` (128 + 31).
    (Stream::Stderr, "", Some(159), None, LINUX),
    (Stream::Stderr, "", None, Some(31), LINUX),
    // Accepted false positives: the same errors from other causes.
    (
        Stream::Stderr,
        "git@github.com: Permission denied (publickey).",
        Some(128),
        None,
        true,
    ),
    (
        Stream::Stderr,
        "cat: secret: Permission denied",
        Some(1),
        None,
        true,
    ),
    // Not evidence: words that never appear in a real denial, stdout,
    // which a command fully controls, success, and other signals.
    (
        Stream::Stderr,
        "test sandbox::lifecycle ... FAILED",
        Some(101),
        None,
        false,
    ),
    (Stream::Stderr, "landlock or seccomp", Some(1), None, false),
    (
        Stream::Stdout,
        "Operation not permitted",
        Some(1),
        None,
        false,
    ),
    (
        Stream::StdoutTail,
        "Permission denied",
        Some(1),
        None,
        false,
    ),
    (
        Stream::Stderr,
        "Operation not permitted",
        Some(0),
        None,
        false,
    ),
    (
        Stream::Stderr,
        "Operation not permitted",
        None,
        Some(9),
        false,
    ),
    // Known false negatives: a blocked network connection reads as a
    // resolution or connection failure, and a plain failure.
    (
        Stream::Stderr,
        "curl: (6) Could not resolve host: example.com",
        Some(6),
        None,
        false,
    ),
    (
        Stream::Stderr,
        "curl: (7) Couldn't connect to server",
        Some(7),
        None,
        false,
    ),
    (Stream::Stderr, "", Some(1), None, false),
];

#[test]
fn the_denial_detector_matches_operating_system_denials_on_stderr() {
    // Arrange / Act / Assert
    for (stream, text, exit_code, signal, expected) in DETECTOR_CASES {
        let mut outcome = CommandOutcome {
            cleanup_failed: false,
            cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
            execution_failure: None,
            exit_code,
            signal,
            stderr: String::new(),
            stderr_omitted_bytes: 0,
            stderr_tail: String::new(),
            stdout: String::new(),
            stdout_omitted_bytes: 0,
            stdout_tail: String::new(),
            termination: CommandTermination::Completed,
            truncated: false,
        };
        let field = match stream {
            Stream::Stderr => &mut outcome.stderr,
            Stream::StderrTail => &mut outcome.stderr_tail,
            Stream::Stdout => &mut outcome.stdout,
            Stream::StdoutTail => &mut outcome.stdout_tail,
        };
        *field = text.to_string();
        assert_eq!(
            likely_denied(&outcome),
            expected,
            "{stream:?} {text:?} {exit_code:?} {signal:?}"
        );
    }
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
