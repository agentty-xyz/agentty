use std::path::{Path, PathBuf};
use std::sync::Arc;

use mockall::Sequence;
use serde_json::{Value, json};

use super::support::{
    arguments, command_output, file_system, inspection_file_system, truncated_command_output,
};
use crate::comparison::support::COMPARISON_OID;
use crate::read::command::MockRepositoryCommandRunner;
use crate::read::runtime::{MAX_READ_BYTES, MAX_UNTRACKED_DIFF_FILES, ReadTool};

#[tokio::test]
async fn dispatches_worktree_file_through_read_action() {
    // Arrange
    let tool = ReadTool::new(file_system("first\nsecond\n"), PathBuf::from("repo"));
    let arguments = arguments(json!({
        "path": "input.txt",
        "limit": 1
    }));

    // Act
    let (result, summary) = tool
        .execute_inspection(&arguments)
        .await
        .expect("file action should succeed");
    let result: Value = serde_json::from_str(&result).expect("file result should be JSON");

    // Assert
    assert_eq!(summary, "input.txt");
    assert_eq!(result["content"], "first");
    assert_eq!(result["next_offset"], 2);
}

#[tokio::test]
async fn reads_host_bound_diff_with_path_filter() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--relative",
                        "--unified=20",
                        COMPARISON_OID,
                        "--",
                        "crates/ag-harness",
                    ]
        })
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, "diff --git a/file b/file\n")));
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "ls-files",
                        "--others",
                        "--exclude-standard",
                        "-z",
                        "--",
                        "crates/ag-harness",
                    ]
        })
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "diff",
        "path": "crates/ag-harness"
    }))
    .expect("diff arguments should be valid");

    // Act
    let (result, summary) = tool
        .execute_inspection(&arguments)
        .await
        .expect("diff inspection should succeed");
    let result: Value = serde_json::from_str(&result).expect("diff result should be JSON");

    // Assert
    assert_eq!(summary, COMPARISON_OID);
    assert_eq!(result["result"], "diff --git a/file b/file\n");
    assert_eq!(result["truncated"], false);
    assert_eq!(result["comparison_base"], COMPARISON_OID);
}

#[tokio::test]
async fn includes_untracked_files_in_host_bound_diff() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--relative",
                        "--unified=20",
                        COMPARISON_OID,
                        "--",
                        ".",
                    ]
        })
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, "tracked\n")));
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "ls-files",
                        "--others",
                        "--exclude-standard",
                        "-z",
                        "--",
                        ".",
                    ]
        })
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, b"new.rs\0")));
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "diff",
                        "--no-index",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--unified=20",
                        "--",
                        "/dev/null",
                        "new.rs",
                    ]
        })
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(1, "untracked\n")));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments =
        serde_json::from_value(json!({"action": "diff"})).expect("diff arguments should be valid");

    // Act
    let (result, _) = tool
        .execute_inspection(&arguments)
        .await
        .expect("diff inspection should succeed");
    let result: Value = serde_json::from_str(&result).expect("diff result should be JSON");

    // Assert
    assert_eq!(result["result"], "tracked\nuntracked\n");
    assert_eq!(result["truncated"], false);
}

#[tokio::test]
async fn bounds_large_diffs_and_untracked_path_discovery() {
    // Arrange
    let mut large_diff_runner = MockRepositoryCommandRunner::new();
    large_diff_runner
        .expect_run()
        .times(1)
        .returning(|_, _| Ok(truncated_command_output(0, b"complete\npartial-\xc3")));
    let large_diff_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(large_diff_runner));
    let mut untracked_runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    untracked_runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    let untracked_paths = (0..=MAX_UNTRACKED_DIFF_FILES)
        .flat_map(|index| format!("file-{index}.rs\0").into_bytes())
        .collect::<Vec<_>>();
    untracked_runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .return_once(move |_, _| Ok(command_output(0, untracked_paths)));
    let untracked_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(untracked_runner));
    let arguments =
        serde_json::from_value(json!({"action": "diff"})).expect("diff arguments should be valid");

    // Act
    let (large_result, _) = large_diff_tool
        .execute_inspection(&arguments)
        .await
        .expect("large diff should be bounded");
    let (untracked_result, _) = untracked_tool
        .execute_inspection(&arguments)
        .await
        .expect("large untracked set should be bounded");
    let large_result: Value =
        serde_json::from_str(&large_result).expect("large diff result should be JSON");
    let untracked_result: Value =
        serde_json::from_str(&untracked_result).expect("untracked result should be JSON");

    // Assert
    assert_eq!(large_result["result"], "complete\n");
    assert_eq!(large_result["truncated"], true);
    assert_eq!(untracked_result["result"], "");
    assert_eq!(untracked_result["truncated"], true);
}

#[tokio::test]
async fn stops_untracked_diff_collection_after_a_truncated_patch() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(command_output(0, b"large.rs\0ignored.rs\0")));
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(truncated_command_output(1, b"complete\npartial-\xc3")));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments =
        serde_json::from_value(json!({"action": "diff"})).expect("diff arguments should be valid");

    // Act
    let (result, _) = tool
        .execute_inspection(&arguments)
        .await
        .expect("truncated untracked diff should be bounded");
    let result: Value = serde_json::from_str(&result).expect("diff result should be JSON");

    // Assert
    assert_eq!(result["result"], "complete\n");
    assert_eq!(result["truncated"], true);
}

#[test]
fn bounded_diff_helpers_preserve_utf8_and_separate_patches() {
    // Arrange
    let mut oversized = "x".repeat(MAX_READ_BYTES - 1);
    oversized.push('é');
    let mut joined = "tracked".to_string();
    let mut nearly_full = "x".repeat(MAX_READ_BYTES - 2);
    let addition = "éé";

    // Act
    let (bounded, truncated) =
        ReadTool::bounded_inspection_text(command_output(0, oversized.into_bytes()))
            .expect("UTF-8 diff should remain valid");
    let joined_truncated = ReadTool::append_bounded_diff(&mut joined, "untracked");
    let full_truncated = ReadTool::append_bounded_diff(&mut nearly_full, addition);

    // Assert
    assert_eq!(bounded.len(), MAX_READ_BYTES - 1);
    assert!(truncated);
    assert_eq!(joined, "tracked\nuntracked");
    assert!(!joined_truncated);
    assert_eq!(nearly_full.len(), MAX_READ_BYTES - 1);
    assert!(nearly_full.ends_with('\n'));
    assert!(full_truncated);
}
