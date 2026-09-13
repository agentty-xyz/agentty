use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::support::{arguments, command_output, inspection_file_system, truncated_command_output};
use crate::comparison::support::{COMPARISON_OID, ComparisonRepository};
use crate::file_system::LocalFileSystem;
use crate::read::command::MockRepositoryCommandRunner;
use crate::read::output::{InspectionError, ReadError};
use crate::read::runtime::{MAX_SCAN_BYTES, ReadTool};
use crate::repository::support::test_git_executable;
use crate::{ComparisonBase, Repository};

#[tokio::test]
async fn shows_selected_lines_from_base_revision() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    runner
        .expect_run_large()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments == ["cat-file", "blob", &format!("{COMPARISON_OID}:src/lib.rs")]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, "one\ntwo\nthree\n")));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "base",
        "path": "src/lib.rs",
        "offset": 2,
        "limit": 1
    }))
    .expect("show arguments should be valid");

    // Act
    let (result, summary) = tool
        .execute_inspection(&arguments)
        .await
        .expect("show inspection should succeed");
    let result: Value = serde_json::from_str(&result).expect("show result should be JSON");

    // Assert
    assert_eq!(summary, format!("{COMPARISON_OID}:src/lib.rs"));
    assert_eq!(result["content"], "two");
    assert_eq!(result["comparison_base"], COMPARISON_OID);
    assert_eq!(result["start_line"], 2);
    assert_eq!(result["end_line"], 2);
    assert_eq!(result["next_offset"], 3);
    assert_eq!(result["truncated"], true);
}

#[tokio::test]
async fn shows_head_revision_and_rejects_an_offset_beyond_end() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    runner
        .expect_run_large()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["cat-file", "blob", "HEAD:src/lib.rs"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, "one\n")));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "src/lib.rs",
        "offset": 2
    }))
    .expect("show arguments should be valid");

    // Act
    let error = tool
        .execute_inspection(&arguments)
        .await
        .expect_err("offset beyond the revision file should fail");

    // Assert
    assert!(matches!(
        error,
        InspectionError::Read(ReadError::OffsetBeyondEnd { offset: 2, path })
            if path == "src/lib.rs"
    ));
}

#[tokio::test]
async fn shows_revision_file_beyond_normal_command_capture_limit() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    runner
        .expect_run_large()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["cat-file", "blob", "HEAD:large.txt"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, "123456789\n".repeat(6_000))));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "large.txt",
        "offset": 5500,
        "limit": 1
    }))
    .expect("show arguments should be valid");

    // Act
    let (result, _) = tool
        .execute_inspection(&arguments)
        .await
        .expect("a later revision-file page should be readable");
    let result: Value = serde_json::from_str(&result).expect("show result should be JSON");

    // Assert
    assert_eq!(result["content"], "123456789");
    assert_eq!(result["start_line"], 5500);
    assert_eq!(result["end_line"], 5500);
    assert_eq!(result["next_offset"], 5501);
}

#[tokio::test]
async fn reports_scan_limit_when_revision_page_exceeds_large_capture() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, Vec::new())));
    runner.expect_run_large().times(1).returning(|_, _| {
        let mut source = "x\n".repeat(MAX_SCAN_BYTES / 2 + 1).into_bytes();
        source.truncate(MAX_SCAN_BYTES);

        Ok(truncated_command_output(0, source))
    });
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "very-large.txt",
        "offset": u64::try_from(MAX_SCAN_BYTES / 2).unwrap_or(u64::MAX) + 2,
        "limit": 1
    }))
    .expect("large show arguments should be valid");

    // Act
    let error = tool
        .execute_inspection(&arguments)
        .await
        .expect_err("paging beyond a truncated capture should fail safely");

    // Assert
    assert!(matches!(
        error,
        InspectionError::Read(ReadError::ScanLimitExceeded { limit, path })
            if limit == MAX_SCAN_BYTES && path == "very-large.txt"
    ));
}

#[tokio::test]
async fn reports_scan_limit_when_revision_capture_ends_during_page() {
    // Arrange
    let arguments = arguments(json!({
        "path": "large.txt",
        "limit": 2
    }));

    // Act
    let error = ReadTool::read(
        Box::new(Cursor::new(b"one\n")),
        &arguments,
        "large.txt".to_string(),
        true,
    )
    .await
    .expect_err("truncated revision capture should not look complete");

    // Assert
    assert!(matches!(
        error,
        ReadError::ScanLimitExceeded { limit, path }
            if limit == MAX_SCAN_BYTES && path == "large.txt"
    ));
}

#[tokio::test]
async fn show_scopes_tree_path_to_configured_subdirectory_root() {
    // Arrange
    let tool = ReadTool::new(
        Arc::new(LocalFileSystem),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    );
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "Cargo.toml",
        "limit": 12
    }))
    .expect("show arguments should be valid");

    // Act
    let (result, summary) = tool
        .execute_inspection(&arguments)
        .await
        .expect("subdirectory-root show should succeed");
    let result: Value = serde_json::from_str(&result).expect("show result should be JSON");

    // Assert
    assert_eq!(summary, "HEAD:Cargo.toml");
    assert!(
        result["content"]
            .as_str()
            .is_some_and(|content| content.contains("name = \"ag-harness\""))
    );
    assert!(
        result["content"]
            .as_str()
            .is_none_or(|content| !content.contains("[workspace]"))
    );
}

#[tokio::test]
async fn rejects_invalid_git_prefix_before_reading_revision_file() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, "../\n")));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "Cargo.toml"
    }))
    .expect("show arguments should be valid");

    // Act
    let error = tool
        .execute_inspection(&arguments)
        .await
        .expect_err("invalid Git prefix should be rejected");

    // Assert
    assert!(matches!(
        error,
        InspectionError::RepositoryCommandRejected { detail }
            if detail == "Git returned an invalid repository prefix"
    ));
}

#[tokio::test]
async fn rejects_truncated_git_prefix_before_reading_revision_file() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
        })
        .times(1)
        .returning(|_, _| Ok(truncated_command_output(0, b"crates/ag-harness/")));
    runner.expect_run_large().times(0);
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "show",
        "side": "head",
        "path": "Cargo.toml"
    }))
    .expect("show arguments should be valid");

    // Act
    let error = tool
        .execute_inspection(&arguments)
        .await
        .expect_err("truncated Git prefix should be rejected");

    // Assert
    assert!(matches!(
        error,
        InspectionError::RepositoryCommandRejected { detail }
            if detail == "Git returned a truncated repository prefix"
    ));
}

#[tokio::test]
async fn comparisons_keep_the_selected_oid_after_branch_movement_and_with_nested_scope() {
    // Arrange
    let fixture = ComparisonRepository::new().await;
    let repository = Repository::new(
        fixture.directory.path().join("scope"),
        test_git_executable(),
    )
    .expect("nested scope");
    let base = ComparisonBase::resolve(&repository, "release")
        .await
        .expect("base");
    let tool = ReadTool::with_git(
        Arc::new(LocalFileSystem),
        repository.root().to_path_buf(),
        test_git_executable(),
        Some(base),
    );
    let show = serde_json::from_value(json!({"action":"show", "side":"base", "path":"name.txt"}))
        .expect("show");
    let diff = serde_json::from_value(json!({"action":"diff"})).expect("diff");

    // Act
    fixture.move_branch().await;
    let replacements = fixture.directory.path().join(".git/refs/replace");
    tokio::fs::create_dir_all(&replacements)
        .await
        .expect("replacement refs");
    tokio::fs::write(replacements.join(&fixture.base), &fixture.next)
        .await
        .expect("replacement commit");
    let (shown, summary) = tool.execute_inspection(&show).await.expect("pinned show");
    let (patch, diff_summary) = tool.execute_inspection(&diff).await.expect("pinned diff");
    let shown: Value = serde_json::from_str(&shown).expect("show JSON");
    let patch: Value = serde_json::from_str(&patch).expect("diff JSON");

    // Assert
    assert_eq!(shown["content"], "base");
    assert_eq!(shown["comparison_base"], fixture.base);
    assert_eq!(patch["comparison_base"], fixture.base);
    assert_eq!(summary, format!("{}:name.txt", fixture.base));
    assert_eq!(diff_summary, fixture.base);
    assert!(patch["result"].as_str().expect("patch").contains("-base"));
    assert!(
        !patch["result"]
            .as_str()
            .expect("patch")
            .contains("outside.txt")
    );
}

#[tokio::test]
async fn missing_comparison_base_only_rejects_comparison_actions() {
    // Arrange
    let fixture = ComparisonRepository::new().await;
    let tool = ReadTool::with_git(
        Arc::new(LocalFileSystem),
        fixture.repository.root().to_path_buf(),
        test_git_executable(),
        None,
    );
    let requests = [
        json!({"action":"file", "path":"scope/name.txt"}),
        json!({"action":"list"}),
        json!({"action":"search", "query":"working"}),
        json!({"action":"show", "side":"head", "path":"scope/name.txt"}),
        json!({"action":"diff"}),
        json!({"action":"show", "side":"base", "path":"scope/name.txt"}),
    ];

    // Act
    let mut results = Vec::new();
    for request in requests {
        results.push(
            tool.execute_inspection(&serde_json::from_value(request).expect("arguments"))
                .await,
        );
    }

    // Assert
    assert!(results[..4].iter().all(Result::is_ok));
    for result in &results[4..] {
        let error = result.as_ref().expect_err("comparison unavailable");
        assert!(error.is_model_correctable());
        assert!(
            error
                .to_string()
                .contains("comparison base is not configured")
        );
    }
}
