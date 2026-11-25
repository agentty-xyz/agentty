//! Built-in tool handler unit tests for their definitions, effects, and
//! calls made outside a turn.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::bash::{
    BashConfig, BashExecutor, BashProcess, CommandCleanupScope, CommandExit, Enforcement,
    ExecutionCommand, ExecutionError, ExecutionPolicy, ProcessEvent, UnsandboxedExecutor,
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
    // Tools that never leave the sandbox keep the default per-call answers.
    assert_eq!(read.call_effects(&json!({})), ToolEffects::ReadOnly);
    assert_eq!(read.unsandboxed_command(&json!({})), None);
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
        exit: CommandExit::default().with_termination(CommandTermination::Failed),
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        truncated: false,
    };

    // Act / Assert
    assert!(refused(&refused_outcome));
    assert!(!refused(&CommandOutcome {
        cleanup_failed: true,
        ..refused_outcome.clone()
    }));
    assert!(!refused(&CommandOutcome {
        exit: CommandExit::default().with_termination(CommandTermination::Cancelled),
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

/// Returns a context for a call the approval handler explicitly allowed to
/// run outside the sandbox.
fn approved_unsandboxed(call_id: &str) -> ToolContext {
    let mut context = ToolContext::new(call_id);
    context.unsandboxed = true;

    context
}

#[tokio::test]
async fn excluded_commands_run_their_exact_words_without_a_shell_once_approved() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    // A file name a shell glob would turn into an option.
    std::fs::write(root.path().join("--upload-pack=touch PWNED"), "").expect("option file");
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
    .with_excluded_command("/bin/echo".into())
    .expect("excluded command");
    let echo = std::fs::canonicalize("/bin/echo")
        .expect("echo")
        .to_string_lossy()
        .into_owned();
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
    let plain = json!({"command": "echo $ fetch"});
    let words = json!({"command": "echo fetch origin"});
    let globbed = json!({"command": "echo fetch *"});

    // Act
    let unapproved = handler
        .call(words.clone(), ToolContext::new("bash_1"))
        .await
        .expect("unapproved command");
    let approved = handler
        .call(words.clone(), approved_unsandboxed("bash_2"))
        .await
        .expect("approved command");
    let globbed_output = handler
        .call(globbed.clone(), approved_unsandboxed("bash_3"))
        .await
        .expect("globbed command");

    // Assert
    assert_eq!(
        handler.unsandboxed_command(&words),
        Some(vec![echo.clone(), "fetch".into(), "origin".into()])
    );
    assert_eq!(
        handler.call_effects(&words),
        ToolEffects::ExecutesUnsandboxed
    );
    // A glob, an expansion, an operator, or arguments that do not decode
    // keep the command sandboxed even when approved.
    for command in [
        &plain,
        &globbed,
        &json!({"command": "echo x; true"}),
        &json!({}),
    ] {
        assert_eq!(handler.call_effects(command), ToolEffects::Executes);
        assert_eq!(handler.unsandboxed_command(command), None);
    }
    assert!(
        handler
            .definition()
            .description()
            .contains("Programs excluded from the sandbox: `echo`."),
        "{:?}",
        handler.definition()
    );
    for output in [unapproved, approved] {
        let result: Value = serde_json::from_str(output.content()).expect("JSON result");
        assert_eq!(result["stdout"], "fetch origin\n");
    }
    // The glob ran through the sandboxed shell, never as approved words.
    let result: Value = serde_json::from_str(globbed_output.content()).expect("JSON result");
    assert_eq!(result["stdout"], "fetch --upload-pack=touch PWNED\n");
    assert!(!root.path().join("PWNED").exists());
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
        .call(json!({"command": "tool"}), approved_unsandboxed("bash_1"))
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

/// Returns a `bash` handler that runs real `/bin/bash` unsandboxed in
/// `root`'s `workspace`, with `root`'s `tmp` as its temporary directory,
/// keeping `capture_bytes` of output.
fn real_bash_handler(root: &std::path::Path, capture_bytes: usize) -> BashHandler {
    let root = root.canonicalize().expect("canonical root");
    let workspace = root.join("workspace");
    let temporary = root.join("tmp");
    for directory in [&workspace, &temporary] {
        std::fs::create_dir_all(directory).expect("test directory");
    }
    let configuration = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        capture_bytes,
    )
    .expect("configuration")
    .with_host_information()
    .with_temporary_directory(temporary)
    .expect("temporary directory grant");

    BashHandler {
        definition: ToolDefinition::bash(Enforcement::None),
        tool: BashTool::new(configuration, workspace, Commands::default(), None)
            .expect("bash tool"),
    }
}

/// One piped command case: the command, its exit code, the stages
/// reported, and its exact standard output and error, or `None` where they
/// vary by Bash version.
type PipeCase = (
    &'static str,
    i32,
    &'static [i32],
    Option<(&'static str, &'static str)>,
);

#[tokio::test]
async fn a_command_ending_in_a_pipeline_reports_each_stage_without_changing_its_result() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let handler = real_bash_handler(root.path(), 4096);
    let cases: [PipeCase; 13] = [
        // The command's own failure, hidden by `tail`, is reported.
        (
            "echo out; echo err >&2; (exit 101) | tail -1",
            0,
            &[101, 0],
            Some(("out\n", "err\n")),
        ),
        // The command's own redirections never carry the status.
        (
            "exec 2>&1; echo err >&2; false | true",
            0,
            &[1, 0],
            Some(("err\n", "")),
        ),
        ("exec 2>log.txt; false | true", 0, &[1, 0], Some(("", ""))),
        ("exec 2>&-; false | true", 0, &[1, 0], Some(("", ""))),
        // The stages trace concurrently, in either order; see
        // `tracing_traces_only_the_command_not_the_probe`.
        ("set -x; false | true", 0, &[1, 0], None),
        ("set -e; true | false", 1, &[0, 1], Some(("", ""))),
        ("set -o pipefail; false | true", 1, &[1, 0], Some(("", ""))),
        ("! true | true", 1, &[0, 0], Some(("", ""))),
        // A signal that kills the final stage.
        ("true | sh -c 'kill -9 $$'", 137, &[0, 137], None),
        // `exit` after a pipeline is not that pipeline's status.
        ("false | true; exit 5", 5, &[], Some(("", ""))),
        // The command replaced or cleared the trap.
        (
            "trap 'echo mine' EXIT; false | true",
            0,
            &[],
            Some(("mine\n", "")),
        ),
        ("trap - EXIT; false | true", 0, &[], Some(("", ""))),
        // A changed `TMPDIR` does not move the status file.
        (
            "TMPDIR=/nonexistent; false | true",
            0,
            &[1, 0],
            Some(("", "")),
        ),
    ];

    for (index, (command, exit_code, pipe_status, streams)) in cases.into_iter().enumerate() {
        // Act
        let output = handler
            .call(
                json!({"command": command}),
                ToolContext::new(format!("bash_{index}")),
            )
            .await
            .expect("command result");

        // Assert
        let (content, activity) = output.into_parts();
        let result: Value = serde_json::from_str(&content).expect("JSON result");
        assert_eq!(result["exit_code"], exit_code, "{command}");
        assert!(!content.contains("agentty_pipe"), "{content}");
        if pipe_status.is_empty() {
            assert!(result.get("pipe_status").is_none(), "{content}");
        } else {
            assert_eq!(result["pipe_status"], json!(pipe_status), "{content}");
        }
        if let Some((stdout, stderr)) = streams {
            assert_eq!(
                (&result["stdout"], &result["stderr"]),
                (&json!(stdout), &json!(stderr)),
                "{command}"
            );
        }
        let Some(ToolActivity::Bash { exit, .. }) = activity else {
            std::panic::resume_unwind(Box::new(format!("unexpected activity for {command}")));
        };
        assert_eq!(
            exit,
            CommandExit::new(Some(exit_code), None).with_pipe_status(pipe_status.to_vec()),
            "{command}"
        );
    }
    let log = std::fs::read(root.path().join("workspace/log.txt")).expect("redirected log");
    assert_eq!(log, b"", "the status never reaches a redirected file");
    let leftovers = std::fs::read_dir(root.path().join("tmp"))
        .expect("temporary directory")
        .count();
    assert_eq!(leftovers, 0, "every status file is deleted once read");
}

#[tokio::test]
async fn tracing_traces_only_the_command_not_the_probe() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let handler = real_bash_handler(root.path(), 4096);

    // Act
    let output = handler
        .call(
            json!({"command": "set -x; false | true"}),
            ToolContext::new("traced"),
        )
        .await
        .expect("command result");

    // Assert: pipeline stages trace concurrently, so only the set of lines
    // is fixed.
    let (content, _) = output.into_parts();
    let result: Value = serde_json::from_str(&content).expect("JSON result");
    let mut traced = result["stderr"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .collect::<Vec<_>>();
    traced.sort_unstable();
    assert_eq!(traced, ["+ false", "+ true"], "{content}");
    assert_eq!(result["stdout"], "");
}

#[tokio::test]
async fn a_background_job_that_redirects_its_output_does_not_hold_the_call_open() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let handler = real_bash_handler(root.path(), 4096);
    let started = std::time::Instant::now();

    // Act
    let output = handler
        .call(
            json!({"command": "sleep 3 >/dev/null 2>&1 & false | true"}),
            ToolContext::new("background"),
        )
        .await
        .expect("command result");

    // Assert
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the call waited {:?} for the background job",
        started.elapsed()
    );
    let (content, _) = output.into_parts();
    let result: Value = serde_json::from_str(&content).expect("JSON result");
    assert_eq!(result["pipe_status"], json!([1, 0]), "{content}");
}

#[tokio::test]
async fn a_status_file_the_command_replaced_with_a_fifo_or_link_is_not_read() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let handler = real_bash_handler(root.path(), 4096);
    let secret = root.path().join("secret");
    std::fs::write(&secret, "0 1 0\n").expect("linked file");
    let fifo = "mkfifo \"$__agentty_pipe_status_file\"; trap - EXIT; false | true".to_string();
    let link = format!(
        "ln -s '{}' \"$__agentty_pipe_status_file\"; trap - EXIT; false | true",
        secret.display()
    );

    for (index, command) in [fifo, link].into_iter().enumerate() {
        let started = std::time::Instant::now();

        // Act
        let output = handler
            .call(
                json!({"command": command}),
                ToolContext::new(format!("replaced_{index}")),
            )
            .await
            .expect("command result");

        // Assert
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{command}: waited {:?}",
            started.elapsed()
        );
        let (content, _) = output.into_parts();
        let result: Value = serde_json::from_str(&content).expect("JSON result");
        assert_eq!(result["exit_code"], 0, "{content}");
        assert!(result.get("pipe_status").is_none(), "{content}");
    }
    let leftovers = std::fs::read_dir(root.path().join("tmp"))
        .expect("temporary directory")
        .count();
    assert_eq!(leftovers, 0, "the replaced paths are removed");
    assert_eq!(std::fs::read(&secret).expect("target kept"), b"0 1 0\n");
}

#[tokio::test]
async fn a_syntax_error_quoting_a_piped_command_does_not_quote_the_probe() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let handler = real_bash_handler(root.path(), 4096);

    // Act
    let syntax = handler
        .call(
            json!({"command": "echo a | | b"}),
            ToolContext::new("syntax"),
        )
        .await
        .expect("syntax error result");

    // Assert
    let (content, _) = syntax.into_parts();
    let result: Value = serde_json::from_str(&content).expect("JSON result");
    let stderr = result["stderr"].as_str().unwrap_or_default();
    assert_eq!(result["exit_code"], 2);
    assert!(stderr.contains("`echo a | | b'"), "{stderr}");
    assert!(!stderr.contains("agentty_pipe"), "{stderr}");
}

#[tokio::test]
async fn output_that_fills_the_capture_budget_is_not_truncated_by_the_probe() {
    // Arrange
    let root = tempfile::tempdir().expect("workspace");
    let handler = real_bash_handler(root.path(), 64);

    // Act
    let output = handler
        .call(
            json!({"command": "printf '%064d' 0 | cat"}),
            ToolContext::new("full"),
        )
        .await
        .expect("command result");

    // Assert
    let (content, _) = output.into_parts();
    let result: Value = serde_json::from_str(&content).expect("JSON result");
    assert_eq!(result["stdout"], "0".repeat(64));
    assert_eq!(result["stderr"], "");
    assert_eq!(result["truncated"], false);
    assert_eq!(result["pipe_status"], json!([0, 0]));
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
            exit: CommandExit::new(exit_code, signal),
            stderr: String::new(),
            stderr_omitted_bytes: 0,
            stderr_tail: String::new(),
            stdout: String::new(),
            stdout_omitted_bytes: 0,
            stdout_tail: String::new(),
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
