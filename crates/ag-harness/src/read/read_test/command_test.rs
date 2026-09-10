use std::io;
use std::io::Cursor;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mockall::Sequence;
use serde_json::{Value, json};

use super::support::{
    FailingReader, command_output, inspection_file_system, truncated_command_output,
};
use crate::file_system::LocalFileSystem;
use crate::read::command::{
    LocalRepositoryCommandRunner, MockRepositoryCommandRunner, RepositoryCommandOutput,
    RepositoryCommandRunner,
};
use crate::read::output::InspectionError;
use crate::read::runtime::{MAX_READ_BYTES, ReadTool};
use crate::repository::support::test_git_executable;

#[test]
fn complete_record_retention_keeps_a_delimiter_at_the_capture_boundary() {
    // Arrange
    let output = truncated_command_output(0, b"complete\n");

    // Act
    let output = output.retain_complete_records(b'\n');

    // Assert
    assert_eq!(output.stdout, b"complete\n");
    assert!(output.truncated);
}

#[tokio::test]
async fn propagates_command_truncation_for_list_and_search() {
    // Arrange
    let mut list_runner = MockRepositoryCommandRunner::new();
    list_runner
        .expect_run()
        .times(1)
        .returning(|_, _| Ok(truncated_command_output(0, b"src/lib.rs\0partial-\xc3")));
    let list_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(list_runner));
    let list_arguments = serde_json::from_value(json!({ "action": "list" }))
        .expect("list arguments should be valid");
    let mut search_runner = MockRepositoryCommandRunner::new();
    search_runner.expect_run().times(1).returning(|_, _| {
        Ok(truncated_command_output(
            0,
            b"src/lib.rs:1:hit\npartial-\xc3",
        ))
    });
    let search_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(search_runner));
    let search_arguments = serde_json::from_value(json!({
        "action": "search",
        "query": "hit"
    }))
    .expect("search arguments should be valid");

    // Act
    let (list_result, _) = list_tool
        .execute_inspection(&list_arguments)
        .await
        .expect("truncated list should return its retained paths");
    let (search_result, _) = search_tool
        .execute_inspection(&search_arguments)
        .await
        .expect("truncated search should return its retained matches");
    let list_result: Value =
        serde_json::from_str(&list_result).expect("list result should be JSON");
    let search_result: Value =
        serde_json::from_str(&search_result).expect("search result should be JSON");

    // Assert
    assert_eq!(list_result["result"], json!(["src/lib.rs"]));
    assert_eq!(list_result["truncated"], true);
    assert_eq!(search_result["result"], json!(["src/lib.rs:1:hit"]));
    assert_eq!(search_result["truncated"], true);
}

#[tokio::test]
async fn truncated_repository_command_still_rejects_failed_status() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner.expect_run().times(1).returning(|_, _| {
        Ok(RepositoryCommandOutput {
            code: Some(2),
            stderr: b"invalid revision".to_vec(),
            stdout: vec![b'x'; MAX_READ_BYTES],
            truncated: true,
        })
    });
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments =
        serde_json::from_value(json!({"action": "list"})).expect("list arguments should be valid");

    // Act
    let error = tool
        .execute_inspection(&arguments)
        .await
        .expect_err("rejected Git command should fail");

    // Assert
    assert!(matches!(
        error,
        InspectionError::RepositoryCommandRejected { detail } if detail == "invalid revision"
    ));
}

#[tokio::test]
async fn large_repository_command_rejection_returns_bounded_diagnostic() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    runner
        .expect_run_large()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| {
            Ok(RepositoryCommandOutput {
                code: Some(2),
                stderr: b"invalid object".to_vec(),
                stdout: Vec::new(),
                truncated: false,
            })
        });
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "missing.rs"
    }))
    .expect("show arguments should be valid");

    // Act
    let error = tool
        .execute_inspection(&arguments)
        .await
        .expect_err("rejected Git object read should fail");

    // Assert
    assert!(matches!(
        error,
        InspectionError::RepositoryCommandRejected { detail } if detail == "invalid object"
    ));
}

#[tokio::test]
async fn local_repository_runner_executes_bounded_read_only_git_command() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let arguments = [
        "ls-files".to_string(),
        "--".to_string(),
        "Cargo.toml".to_string(),
    ];

    // Act
    let output = LocalRepositoryCommandRunner::new(test_git_executable())
        .run(root, &arguments)
        .await
        .expect("read-only Git command should run");

    // Assert
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout, b"Cargo.toml\n");
    assert!(!output.truncated);
}

#[cfg(unix)]
#[test]
fn local_repository_runner_ignores_untrusted_process_configuration() {
    // Arrange
    let test_executable = std::env::current_exe().expect("test executable should be available");
    let fake_directory = tempfile::Builder::new()
        .prefix("ag-harness-fake-git-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("fake Git directory should be created beneath the inspected repository");
    let fake_git = fake_directory
        .path()
        .join(format!("git{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&fake_git, "#!/bin/sh\nexit 97\n")
        .expect("fake Git executable should be created");
    let mut permissions = std::fs::metadata(&fake_git)
        .expect("fake Git metadata should be available")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fake_git, permissions)
        .expect("fake Git executable permissions should be installed");
    let inherited_path = std::env::var_os("PATH").expect("test PATH should be configured");
    let git_executable = test_git_executable();
    let path = std::env::join_paths(
        [PathBuf::from("."), fake_directory.path().to_path_buf()]
            .into_iter()
            .chain(std::env::split_paths(&inherited_path)),
    )
    .expect("test PATH should be valid");

    // Act
    let output = std::process::Command::new(test_executable)
        .args([
            "--ignored",
            "--exact",
            "read::tests::command::local_repository_runner_environment_subprocess",
        ])
        .env("GIT_DIR", "missing-git-dir")
        .env("GIT_WORK_TREE", "/")
        .env("GIT_INDEX_FILE", "missing-index")
        .env("AG_HARNESS_TEST_GIT", git_executable)
        .env("PATH", path)
        .output()
        .expect("isolated Git environment test should run");
    let standard_error = String::from_utf8_lossy(&output.stderr);

    // Assert
    assert!(
        output.status.success(),
        "isolated Git environment test failed: {standard_error}"
    );
}

#[tokio::test]
#[ignore = "run by local_repository_runner_ignores_untrusted_process_configuration"]
async fn local_repository_runner_environment_subprocess() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let arguments = ["rev-parse".to_string(), "--show-prefix".to_string()];
    let git_executable = std::env::var_os("AG_HARNESS_TEST_GIT")
        .map(PathBuf::from)
        .expect("trusted test Git executable should be configured");

    // Act
    let output = LocalRepositoryCommandRunner::new(git_executable)
        .run(root, &arguments)
        .await
        .expect("sanitized Git inspection should run");

    // Assert
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout, b"crates/ag-harness/\n");
}

#[tokio::test]
async fn repository_verification_rejects_root_outside_selected_worktree() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let unrelated_root = root
        .parent()
        .expect("crate directory should have a parent")
        .join("ag-agent");
    let output = command_output(0, format!("{}\n", unrelated_root.display()));

    // Act
    let error = LocalRepositoryCommandRunner::verify_repository_root(root, output)
        .await
        .expect_err("outside worktree root should be rejected");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn repository_verification_rejects_failed_discovery() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = command_output(1, Vec::new());

    // Act
    let error = LocalRepositoryCommandRunner::verify_repository_root(root, output)
        .await
        .expect_err("failed Git discovery should be rejected");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn treats_repository_path_filters_as_literal() {
    // Arrange
    let tool = ReadTool::new(
        Arc::new(LocalFileSystem),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    );
    let arguments = serde_json::from_value(json!({
        "action": "list",
        "path": ":(top)Cargo.toml"
    }))
    .expect("literal list arguments should be valid");

    // Act
    let (result, _) = tool
        .execute_inspection(&arguments)
        .await
        .expect("literal path inspection should succeed");
    let result: Value = serde_json::from_str(&result).expect("list result should be JSON");

    // Assert
    assert_eq!(result["result"], json!([]));
    assert_eq!(result["truncated"], false);
}

#[tokio::test]
async fn local_repository_runner_bounds_large_git_output() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let arguments = [
        "cat-file".to_string(),
        "blob".to_string(),
        "HEAD:Cargo.lock".to_string(),
    ];

    // Act
    let output = LocalRepositoryCommandRunner::new(test_git_executable())
        .run(root, &arguments)
        .await
        .expect("large read-only Git command should be bounded");

    // Assert
    assert_eq!(output.stdout.len(), MAX_READ_BYTES);
    assert!(output.truncated);
}

#[tokio::test]
async fn local_repository_runner_reports_timeout_and_stream_failures() {
    // Arrange
    let stalled = std::future::pending::<io::Result<()>>();

    // Act
    let timeout = LocalRepositoryCommandRunner::with_timeout(Duration::ZERO, stalled).await;
    let stream_error = LocalRepositoryCommandRunner::read_bounded(FailingReader, 1).await;

    // Assert
    assert_eq!(
        timeout.expect_err("stalled command should time out").kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        stream_error
            .expect_err("failing stream should be reported")
            .kind(),
        io::ErrorKind::Other
    );
}

#[tokio::test]
async fn bounded_stream_reader_drains_bytes_after_retention_limit() {
    // Arrange
    let content = b"retained-and-drained".to_vec();
    let mut reader = Cursor::new(content.clone());

    // Act
    let output = LocalRepositoryCommandRunner::read_bounded(&mut reader, 8)
        .await
        .expect("bounded stream should be readable");

    // Assert
    assert_eq!(output.bytes, b"retained");
    assert!(output.truncated);
    assert_eq!(reader.position(), content.len() as u64);
}
