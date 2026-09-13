use std::ffi::OsString;
use std::future::pending;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{
    Access, Command, Execution, ExecutionControl, ExecutionError, ExecutionResult, Executor,
    Grants, Limits, MainExit, Output, Policy, PreparedExecution, Stream, Termination,
    ValidationError,
};

#[test]
fn command_preserves_literal_argv_and_workspace_relative_directory() {
    // Arrange
    let arguments = vec![OsString::from(""), OsString::from("$(hostile); *\n--flag")];

    // Act
    let command = Command::new("/runtime/tool".into(), arguments.clone(), "src".into())
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
            Command::new(executable.into(), vec![], ".".into()).err(),
            Some(ValidationError::InvalidPath)
        );
    }
    for directory in invalid_directories {
        assert_eq!(
            Command::new("/tool".into(), vec![], directory.into()).err(),
            Some(ValidationError::InvalidPath)
        );
    }
    assert_eq!(
        Command::new("/tool".into(), vec!["bad\0argument".into()], ".".into()).err(),
        Some(ValidationError::InvalidArgument)
    );
    assert!(Command::new("/tool".into(), vec![], ".".into()).is_ok());
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
            Ok(Access::ReadOnly)
        );
    }
    for path in [
        "/runtime/tool",
        "/workspace-sibling",
        "/admin/repo/config",
        "/",
    ] {
        assert_eq!(policy.requested_access(Path::new(path)), Ok(Access::Denied));
    }
    assert_eq!(
        policy.requested_access(Path::new("/workspace/../secret")),
        Err(ValidationError::InvalidPath)
    );
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
        ("/workspace/src", Access::ReadWrite),
        ("/workspace/src/file", Access::ReadWrite),
        ("/workspace/src-other/file", Access::ReadOnly),
        ("/workspace/other", Access::ReadOnly),
        ("/runtime/tool", Access::ReadOnly),
        ("/runtime-other/tool", Access::Denied),
        ("/admin/repo/worktrees/linked/index", Access::ReadOnly),
    ] {
        assert_eq!(policy.requested_access(Path::new(path)), Ok(expected));
    }
}

#[test]
fn git_protection_overrides_whole_workspace_write_grants() {
    // Arrange
    let policy = Policy::new(
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
            Ok(Access::ReadOnly)
        );
    }
    for path in [
        "/workspace",
        "/workspace/src/file",
        "/workspace/.github/config",
    ] {
        assert_eq!(
            policy.requested_access(Path::new(path)),
            Ok(Access::ReadWrite)
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
            Policy::new(
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
            Policy::new(
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
            Policy::new("/workspace".into(), vec!["/admin".into()], grants).err(),
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
fn capture_budget_is_shared_binary_and_truncation_is_sticky() {
    // Arrange
    let mut output = Output::new(limits(5));

    // Act
    output.capture(Stream::Stderr, &[0, 255]);
    output.capture(Stream::Stdout, b"abc");

    // Assert
    assert_eq!(output.stderr(), &[0, 255]);
    assert_eq!(output.stdout(), b"abc");
    assert!(!output.truncated());

    // Act
    output.capture(Stream::Stderr, b"discarded");
    output.capture(Stream::Stdout, b"");

    // Assert
    assert_eq!(output.stderr(), &[0, 255]);
    assert_eq!(output.stdout(), b"abc");
    assert!(output.truncated());
}

#[test]
fn partial_chunks_and_zero_budget_retain_only_the_shared_prefix() {
    // Arrange
    let mut partial = Output::new(limits(3));
    let mut zero = Output::new(limits(0));

    // Act
    partial.capture(Stream::Stdout, b"a");
    partial.capture(Stream::Stderr, b"bcd");
    zero.capture(Stream::Stderr, b"");

    // Assert
    assert_eq!(partial.stdout(), b"a");
    assert_eq!(partial.stderr(), b"bc");
    assert!(partial.truncated());
    assert!(!zero.truncated());

    // Act
    zero.capture(Stream::Stdout, b"dropped");

    // Assert
    assert_eq!(zero.stdout(), b"");
    assert_eq!(zero.stderr(), b"");
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
                output.capture(Stream::Stdout, b"out");
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
                assert_eq!(result.output.stdout(), b"ou");
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

fn policy(grants: Grants) -> Policy {
    Policy::new("/workspace".into(), vec!["/admin/repo".into()], grants).expect("valid policy")
}

fn command() -> Command {
    Command::new("/runtime/tool".into(), vec![], ".".into()).expect("valid command")
}

fn limits(bytes: usize) -> Limits {
    Limits::new(Instant::now(), Duration::from_secs(1), bytes).expect("valid limits")
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
        command: Command,
        policy: Policy,
        limits: Limits,
    ) -> Result<PreparedExecution, ExecutionError> {
        assert_eq!(command.executable(), Path::new("/runtime/tool"));
        assert_eq!(
            policy.requested_access(command.executable()),
            Ok(Access::Denied)
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
