use std::ffi::OsString;
use std::future::pending;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{
    Execution, ExecutionAccess, ExecutionCommand, ExecutionControl, ExecutionError,
    ExecutionPolicy, ExecutionResult, Executor, Grants, Limits, MainExit, Output, OutputStream,
    PreparedExecution, RetainedStream, Termination, ValidationError,
};

#[test]
fn command_preserves_literal_argv_and_workspace_relative_directory() {
    // Arrange
    let arguments = vec![OsString::from(""), OsString::from("$(hostile); *\n--flag")];

    // Act
    let command = ExecutionCommand::new("/runtime/tool".into(), arguments.clone(), "src".into())
        .expect("valid literal command");

    // Assert
    assert_eq!(command.executable(), Path::new("/runtime/tool"));
    assert_eq!(command.arguments(), arguments);
    assert_eq!(command.directory(), Path::new("src"));
}

#[test]
fn command_rejects_ambiguous_paths_and_nul_but_allows_root_directory() {
    // Arrange
    let invalid_executables = ["", "tool", "../tool", "/runtime/../tool", "/tool\0"];
    let invalid_directories = ["", "/workspace", "../sibling", "src/../other", "src\0"];

    // Act / Assert
    for executable in invalid_executables {
        assert_eq!(
            ExecutionCommand::new(executable.into(), vec![], ".".into()).err(),
            Some(ValidationError::InvalidPath)
        );
    }
    for directory in invalid_directories {
        assert_eq!(
            ExecutionCommand::new("/tool".into(), vec![], directory.into()).err(),
            Some(ValidationError::InvalidPath)
        );
    }
    assert_eq!(
        ExecutionCommand::new("/tool".into(), vec!["bad\0argument".into()], ".".into()).err(),
        Some(ValidationError::InvalidArgument)
    );
    assert!(ExecutionCommand::new("/tool".into(), vec![], ".".into()).is_ok());
}

#[test]
fn execution_errors_render_content_free_messages() {
    // Arrange
    let cases = [
        (
            ExecutionError::Unsupported,
            "execution unsupported for this policy or platform",
        ),
        (ExecutionError::Setup, "execution setup failed"),
        (ExecutionError::Process, "process execution failed"),
        (ExecutionError::Supervision, "execution supervision failed"),
        (ExecutionError::Cleanup, "execution cleanup failed"),
        (
            ExecutionError::CleanupUnconfirmed,
            "execution cleanup remains unconfirmed",
        ),
    ];

    // Act / Assert
    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn default_policy_is_read_only_without_ambient_grants() {
    // Arrange
    let policy = policy(Grants::default());
    let no_paths: &[PathBuf] = &[];

    // Act / Assert
    assert_eq!(policy.workspace(), Path::new("/workspace"));
    assert_eq!(policy.git_metadata(), [PathBuf::from("/admin/repo")]);
    assert!(policy.environment().is_empty());
    assert_eq!(policy.external_reads(), no_paths);
    assert_eq!(policy.workspace_writes(), no_paths);
    assert!(!policy.exposes_host_information());
    for path in ["/workspace", "/workspace/src", "/workspace/.git/config"] {
        assert_eq!(
            policy.requested_access(Path::new(path)),
            ExecutionAccess::ReadOnly
        );
    }
    for path in [
        "/runtime/tool",
        "/workspace-sibling",
        "/admin/repo/config",
        "/",
        "/workspace/../secret",
    ] {
        assert_eq!(
            policy.requested_access(Path::new(path)),
            ExecutionAccess::Denied
        );
    }
}

#[test]
fn explicit_grants_are_scoped_and_do_not_make_external_reads_writable() {
    // Arrange
    let policy = policy(Grants {
        environment: vec![
            ("TOKEN".into(), "explicit-value".into()),
            ("EMPTY".into(), "".into()),
        ],
        external_reads: vec!["/runtime".into(), "/admin".into()],
        host_information: true,
        workspace_writes: vec!["src".into()],
        ..Grants::default()
    });

    // Act / Assert
    assert_eq!(
        policy.environment().get(&OsString::from("TOKEN")),
        Some(&"explicit-value".into())
    );
    assert_eq!(
        policy.environment().get(&OsString::from("EMPTY")),
        Some(&"".into())
    );
    assert_eq!(
        policy.external_reads(),
        [PathBuf::from("/runtime"), PathBuf::from("/admin")]
    );
    assert_eq!(policy.workspace_writes(), [PathBuf::from("src")]);
    assert!(policy.exposes_host_information());
    for (path, expected) in [
        ("/workspace/src", ExecutionAccess::ReadWrite),
        ("/workspace/src/file", ExecutionAccess::ReadWrite),
        ("/workspace/src-other/file", ExecutionAccess::ReadOnly),
        ("/workspace/other", ExecutionAccess::ReadOnly),
        ("/runtime/tool", ExecutionAccess::ReadOnly),
        ("/runtime-other/tool", ExecutionAccess::Denied),
        (
            "/admin/repo/worktrees/linked/index",
            ExecutionAccess::ReadOnly,
        ),
    ] {
        assert_eq!(policy.requested_access(Path::new(path)), expected);
    }
}

#[test]
fn git_protection_overrides_whole_workspace_write_grants() {
    // Arrange
    let policy = ExecutionPolicy::new(
        "/workspace".into(),
        vec!["/workspace/admin".into(), "/common/git".into()],
        Grants {
            workspace_writes: vec![".".into()],
            external_reads: vec!["/common".into()],
            ..Grants::default()
        },
    )
    .expect("whole workspace grant with protected metadata");

    // Act / Assert
    for path in [
        "/workspace/.git",
        "/workspace/nested/.GIT/config",
        "/workspace/admin/worktrees/id/index",
        "/common/git/config",
        "/common/git/worktrees/id/gitdir",
    ] {
        assert_eq!(
            policy.requested_access(Path::new(path)),
            ExecutionAccess::ReadOnly
        );
    }
    for path in [
        "/workspace",
        "/workspace/src/file",
        "/workspace/.github/config",
    ] {
        assert_eq!(
            policy.requested_access(Path::new(path)),
            ExecutionAccess::ReadWrite
        );
    }
}

#[test]
fn policy_rejects_invalid_roots() {
    // Arrange
    let cases = [
        (
            "relative",
            vec!["/admin"],
            Grants::default(),
            ValidationError::InvalidPath,
        ),
        (
            "/workspace/.git",
            vec!["/admin"],
            Grants::default(),
            ValidationError::InvalidWorkspace,
        ),
        (
            "/workspace",
            vec![],
            Grants::default(),
            ValidationError::MissingGitMetadata,
        ),
        (
            "/workspace",
            vec!["relative"],
            Grants::default(),
            ValidationError::InvalidPath,
        ),
        (
            "/workspace",
            vec!["/workspace"],
            Grants::default(),
            ValidationError::InvalidWorkspace,
        ),
        (
            "/workspace",
            vec!["/"],
            Grants::default(),
            ValidationError::InvalidWorkspace,
        ),
    ];

    // Act / Assert
    for (workspace, metadata, grants, expected) in cases {
        assert_eq!(
            ExecutionPolicy::new(
                workspace.into(),
                metadata.into_iter().map(PathBuf::from).collect(),
                grants
            )
            .err(),
            Some(expected)
        );
    }
}

#[test]
fn policy_rejects_invalid_grants_and_network_enablement() {
    // Arrange
    let cases = [
        (
            "/workspace",
            vec!["/admin"],
            Grants {
                network: true,
                ..Grants::default()
            },
            ValidationError::UnsupportedNetworking,
        ),
        (
            "/workspace",
            vec!["/admin"],
            Grants {
                external_reads: vec!["relative".into()],
                ..Grants::default()
            },
            ValidationError::InvalidPath,
        ),
        (
            "/workspace",
            vec!["/admin"],
            Grants {
                workspace_writes: vec!["../escape".into()],
                ..Grants::default()
            },
            ValidationError::InvalidPath,
        ),
        (
            "/workspace",
            vec!["/admin"],
            Grants {
                workspace_writes: vec!["/external".into()],
                ..Grants::default()
            },
            ValidationError::InvalidPath,
        ),
        (
            "/workspace",
            vec!["/admin"],
            Grants {
                workspace_writes: vec!["nested/.git/config".into()],
                ..Grants::default()
            },
            ValidationError::GitMetadataWrite,
        ),
        (
            "/workspace",
            vec!["/workspace/admin"],
            Grants {
                workspace_writes: vec!["admin/worktrees/id".into()],
                ..Grants::default()
            },
            ValidationError::GitMetadataWrite,
        ),
    ];

    // Act / Assert
    for (workspace, metadata, grants, expected) in cases {
        assert_eq!(
            ExecutionPolicy::new(
                workspace.into(),
                metadata.into_iter().map(PathBuf::from).collect(),
                grants
            )
            .err(),
            Some(expected)
        );
    }
}

#[test]
fn environment_rejects_invalid_names_values_and_duplicates() {
    // Arrange
    let cases = [
        vec![("", "value")],
        vec![("A=B", "value")],
        vec![("A\0", "value")],
        vec![("A", "value\0")],
        vec![("A", "first"), ("A", "second")],
    ];

    // Act / Assert
    for values in cases {
        let grants = Grants {
            environment: values
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
            ..Grants::default()
        };
        assert_eq!(
            ExecutionPolicy::new("/workspace".into(), vec!["/admin".into()], grants).err(),
            Some(ValidationError::InvalidEnvironment)
        );
    }
}

#[test]
fn deadline_is_fixed_from_injected_time_and_rejects_zero_and_overflow() {
    // Arrange
    let now = Instant::now();
    let timeout = Duration::from_secs(5);

    // Act
    let limits = Limits::new(now, timeout, 0).expect("valid deadline");

    // Assert
    assert_eq!(limits.deadline(), now + timeout);
    assert_eq!(limits.capture_bytes(), 0);
    assert_eq!(
        Limits::new(now, Duration::ZERO, 1).err(),
        Some(ValidationError::InvalidDeadline)
    );
    assert_eq!(
        Limits::new(now, Duration::MAX, 1).err(),
        Some(ValidationError::InvalidDeadline)
    );
}

#[test]
fn output_within_the_shared_budget_is_retained_whole_and_binary() {
    // Arrange
    let mut output = Output::new(limits(6));

    // Act
    output.capture(OutputStream::Stderr, &[0, 255]);
    output.capture(OutputStream::Stdout, b"ab");
    output.capture(OutputStream::Stdout, b"cd");

    // Assert
    assert_eq!(output.stderr(), retained(&[0, 255], 0, b""));
    assert_eq!(output.stdout(), retained(b"abcd", 0, b""));
    assert!(!output.truncated());
}

#[test]
fn long_stream_keeps_its_start_and_end_and_counts_omitted_bytes() {
    // Arrange
    let mut output = Output::new(limits(6));

    // Act
    output.capture(OutputStream::Stdout, b"abc");
    output.capture(OutputStream::Stdout, b"defgh");
    output.capture(OutputStream::Stdout, b"ij");

    // Assert
    assert_eq!(output.stdout(), retained(b"abc", 4, b"hij"));
    assert_eq!(output.stderr(), retained(b"", 0, b""));
    assert!(output.truncated());
}

#[test]
fn short_stream_cedes_its_share_and_long_streams_split_the_budget() {
    // Arrange
    let mut ceded = Output::new(limits(6));
    let mut split = Output::new(limits(5));

    // Act
    ceded.capture(OutputStream::Stdout, b"a");
    ceded.capture(OutputStream::Stderr, b"bcdefghij");
    split.capture(OutputStream::Stderr, b"abcdefghij");
    split.capture(OutputStream::Stdout, b"0123456789");

    // Assert
    assert_eq!(ceded.stdout(), retained(b"a", 0, b""));
    assert_eq!(ceded.stderr(), retained(b"bcd", 4, b"ij"));
    assert_eq!(split.stdout(), retained(b"01", 7, b"9"));
    assert_eq!(split.stderr(), retained(b"a", 8, b"j"));
    assert!(ceded.truncated());
    assert!(split.truncated());
}

#[test]
fn zero_budget_omits_every_byte() {
    // Arrange
    let mut zero = Output::new(limits(0));

    // Act
    zero.capture(OutputStream::Stderr, b"");

    // Assert
    assert!(!zero.truncated());

    // Act
    zero.capture(OutputStream::Stdout, b"dropped");

    // Assert
    assert_eq!(zero.stdout(), retained(b"", 7, b""));
    assert_eq!(zero.stderr(), retained(b"", 0, b""));
    assert!(zero.truncated());
}

#[test]
fn result_dimensions_do_not_overwrite_each_other() {
    // Arrange
    let exits = [
        MainExit::Code(0),
        MainExit::Code(7),
        MainExit::Signal(9),
        MainExit::Unavailable,
    ];
    let reasons = [
        Termination::Completed,
        Termination::Deadline,
        Termination::Cancelled,
        Termination::Failed,
    ];

    // Act / Assert
    for main_exit in exits {
        for termination in reasons {
            for cleanup_failure in [None, Some(ExecutionError::Cleanup)] {
                let mut output = Output::new(limits(2));
                output.capture(OutputStream::Stdout, b"out");
                let result = ExecutionResult {
                    cleanup_failure,
                    execution_failure: None,
                    main_exit,
                    output,
                    termination,
                };
                assert_eq!(result.main_exit, main_exit);
                assert_eq!(result.termination, termination);
                assert_eq!(result.cleanup_failure, cleanup_failure);
                assert_eq!(result.output.stdout(), retained(b"o", 1, b"t"));
                assert!(result.output.truncated());
            }
        }
    }
}

#[tokio::test]
async fn retained_control_survives_dropping_a_running_future_and_retries_cleanup() {
    // Arrange
    let state = Arc::new(ControlState::default());
    let executor: &dyn Executor = &FakeExecutor {
        state: Arc::clone(&state),
        rejection: None,
    };
    let PreparedExecution { control, execution } = executor
        .prepare(command(), policy(Grants::default()), limits(2))
        .expect("inert preparation");
    assert_eq!(state.started.load(Ordering::SeqCst), 0);
    let mut running = execution.run();
    let mut context = Context::from_waker(Waker::noop());

    // Act
    assert!(matches!(running.as_mut().poll(&mut context), Poll::Pending));
    drop(running);
    control.cancel();
    control.cancel();
    let first_cleanup = control.cleanup().await;
    let retry = control.cleanup().await;

    // Assert
    assert_eq!(state.started.load(Ordering::SeqCst), 1);
    assert!(state.cancelled.load(Ordering::SeqCst));
    assert_eq!(first_cleanup, Err(ExecutionError::Cleanup));
    assert_eq!(retry, Ok(()));
    assert_eq!(control.cleanup().await, Ok(()));
}

#[tokio::test]
async fn cancellation_before_run_and_dropping_unstarted_execution_retain_control() {
    // Arrange
    let state = Arc::new(ControlState::default());
    let executor = FakeExecutor {
        state: Arc::clone(&state),
        rejection: None,
    };
    let PreparedExecution { control, execution } = executor
        .prepare(command(), policy(Grants::default()), limits(2))
        .expect("prepared");

    // Act
    control.cancel();
    let mut result = execution.run().await;
    result.cleanup_failure = control.cleanup().await.err();
    let unused = executor
        .prepare(command(), policy(Grants::default()), limits(2))
        .expect("prepared");
    drop(unused.execution);

    // Assert
    assert_eq!(result.main_exit, MainExit::Unavailable);
    assert_eq!(result.termination, Termination::Cancelled);
    assert_eq!(result.cleanup_failure, Some(ExecutionError::Cleanup));
    assert_eq!(unused.control.cleanup().await, Ok(()));
    assert_eq!(state.started.load(Ordering::SeqCst), 0);
}

#[test]
fn injection_can_reject_unsupported_or_failed_preparation_without_starting() {
    // Arrange
    let state = Arc::new(ControlState::default());

    // Act / Assert
    for error in [ExecutionError::Unsupported, ExecutionError::Setup] {
        let executor = FakeExecutor {
            state: Arc::clone(&state),
            rejection: Some(error),
        };
        assert_eq!(
            executor
                .prepare(command(), policy(Grants::default()), limits(1))
                .err(),
            Some(error)
        );
        assert_eq!(state.started.load(Ordering::SeqCst), 0);
    }
}

fn policy(grants: Grants) -> ExecutionPolicy {
    ExecutionPolicy::new("/workspace".into(), vec!["/admin/repo".into()], grants)
        .expect("valid policy")
}

fn command() -> ExecutionCommand {
    ExecutionCommand::new("/runtime/tool".into(), vec![], ".".into()).expect("valid command")
}

fn limits(bytes: usize) -> Limits {
    Limits::new(Instant::now(), Duration::from_secs(1), bytes).expect("valid limits")
}

fn retained(head: &[u8], omitted_bytes: u64, tail: &[u8]) -> RetainedStream {
    RetainedStream {
        head: head.to_vec(),
        omitted_bytes,
        tail: tail.to_vec(),
    }
}

#[derive(Default)]
struct ControlState {
    cancelled: AtomicBool,
    cleanup_attempts: AtomicUsize,
    started: AtomicUsize,
}

struct FakeExecutor {
    rejection: Option<ExecutionError>,
    state: Arc<ControlState>,
}

impl Executor for FakeExecutor {
    fn prepare(
        &self,
        command: ExecutionCommand,
        policy: ExecutionPolicy,
        limits: Limits,
    ) -> Result<PreparedExecution, ExecutionError> {
        assert_eq!(command.executable(), Path::new("/runtime/tool"));
        assert_eq!(
            policy.requested_access(command.executable()),
            ExecutionAccess::Denied
        );
        if let Some(error) = self.rejection {
            return Err(error);
        }

        Ok(PreparedExecution {
            control: Box::new(FakeControl(Arc::clone(&self.state))),
            execution: Box::new(FakeExecution {
                limits,
                state: Arc::clone(&self.state),
            }),
        })
    }
}

struct FakeExecution {
    limits: Limits,
    state: Arc<ControlState>,
}

#[async_trait]
impl Execution for FakeExecution {
    async fn run(self: Box<Self>) -> ExecutionResult {
        if !self.state.cancelled.load(Ordering::SeqCst) {
            self.state.started.fetch_add(1, Ordering::SeqCst);
            pending::<()>().await;
        }

        ExecutionResult {
            cleanup_failure: None,
            execution_failure: None,
            main_exit: MainExit::Unavailable,
            output: Output::new(self.limits),
            termination: Termination::Cancelled,
        }
    }
}

struct FakeControl(Arc<ControlState>);

#[async_trait]
impl ExecutionControl for FakeControl {
    fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
    }

    async fn cleanup(&self) -> Result<(), ExecutionError> {
        if self.0.cleanup_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ExecutionError::Cleanup);
        }

        Ok(())
    }
}
