use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::support::{command_output, inspection_file_system, unbounded};
use crate::comparison::support::COMPARISON_OID;
use crate::read::command::MockRepositoryCommandRunner;
use crate::read::output::{InspectionError, ReadError};
use crate::read::runtime::{MAX_READ_BYTES, ReadTool};
use crate::tool::MAX_TOOL_RESULT_BYTES;

#[test]
fn private_inspection_errors_map_to_compatible_read_errors() {
    // Arrange
    let errors = [
        InspectionError::RepositoryCommand {
            source: io::Error::other("command"),
        },
        InspectionError::RepositoryCommandRejected {
            detail: "rejected".to_string(),
        },
        InspectionError::InvalidUtf8,
        InspectionError::Read(ReadError::OutsideRepository {
            path: "original.rs".to_string(),
        }),
        InspectionError::PathMatchesNothing {
            path: "src/store".to_string(),
        },
    ];

    // Act
    let command_is_correctable = errors[0].is_model_correctable();
    let errors = errors
        .into_iter()
        .map(|error| error.into_read_error("inspection".to_string()))
        .collect::<Vec<_>>();

    // Assert
    assert!(!command_is_correctable);
    assert!(matches!(&errors[0], ReadError::Read { path, .. } if path == "inspection"));
    assert!(matches!(&errors[1], ReadError::Open { path, .. } if path == "inspection"));
    assert!(matches!(
        &errors[2],
        ReadError::InvalidUtf8 { line: 1, path } if path == "inspection"
    ));
    assert!(matches!(
        &errors[3],
        ReadError::OutsideRepository { path } if path == "original.rs"
    ));
    assert!(matches!(
        &errors[4],
        ReadError::Open { path, source }
            if path == "inspection" && source.kind() == io::ErrorKind::NotFound
    ));
}

fn expect_listing(
    runner: &mut MockRepositoryCommandRunner,
    path: &'static str,
    stdout: &'static [u8],
) {
    runner
        .expect_run()
        .withf(move |_, arguments| {
            arguments
                == [
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                    "--",
                    path,
                ]
        })
        .times(1)
        .returning(move |_, _| Ok(command_output(0, stdout)));
}

#[tokio::test]
async fn list_and_search_reject_paths_that_match_no_repository_file() {
    // Arrange
    let mut list_runner = MockRepositoryCommandRunner::new();
    expect_listing(&mut list_runner, "src/store", b"");
    let list_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(list_runner));
    let mut search_runner = MockRepositoryCommandRunner::new();
    search_runner
        .expect_run()
        .withf(|_, arguments| arguments.first().is_some_and(|command| command == "grep"))
        .times(1)
        .returning(|_, _| Ok(command_output(1, Vec::new())));
    expect_listing(&mut search_runner, "src/tool", b"");
    let search_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(search_runner));
    let list = serde_json::from_value(json!({"action": "list", "path": "src/store"}))
        .expect("list arguments should be valid");
    let search = serde_json::from_value(json!({
        "action": "search",
        "query": "Store",
        "path": "src/tool"
    }))
    .expect("search arguments should be valid");

    // Act
    let list_error = list_tool
        .execute_inspection(&list, &unbounded)
        .await
        .expect_err("a missing list path should be rejected");
    let search_error = search_tool
        .execute_inspection(&search, &unbounded)
        .await
        .expect_err("a missing search path should be rejected");

    // Assert
    for (error, path) in [(&list_error, "src/store"), (&search_error, "src/tool")] {
        assert!(error.is_model_correctable());
        let result: Value = serde_json::from_str(&error.to_tool_result(path).expect("encode"))
            .expect("rejection should be JSON");
        assert_eq!(result["status"], "rejected");
        assert!(
            result["error"]
                .as_str()
                .expect("error text")
                .contains(&format!("path `{path}` matches no repository file"))
        );
        assert!(
            result["error"]
                .as_str()
                .expect("error text")
                .contains("List its parent directory")
        );
    }
}

#[tokio::test]
async fn lists_bounded_repository_paths_with_one_read_action() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "ls-files",
                        "--cached",
                        "--others",
                        "--exclude-standard",
                        "-z",
                        "--",
                        "crates",
                    ]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(0, b"crates/a.rs\0crates/b.rs\0")));
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "list",
        "path": "crates",
        "limit": 1
    }))
    .expect("list arguments should be valid");

    // Act
    let (result, summary) = tool
        .execute_inspection(&arguments, &unbounded)
        .await
        .expect("list inspection should succeed");
    let result: Value = serde_json::from_str(&result).expect("list result should be JSON");

    // Assert
    assert_eq!(summary, "crates");
    assert_eq!(result["action"], "list");
    assert_eq!(result["result"], json!(["crates/a.rs"]));
    assert_eq!(result["truncated"], true);
}

#[tokio::test]
async fn searches_literal_repository_text_and_accepts_no_matches() {
    // Arrange
    let mut runner = MockRepositoryCommandRunner::new();
    runner
        .expect_run()
        .withf(|root, arguments| {
            root == Path::new("/repo")
                && arguments
                    == [
                        "grep",
                        "--untracked",
                        "-n",
                        "-I",
                        "-F",
                        "-e",
                        "ReadTool",
                        "--",
                        "crates",
                    ]
        })
        .times(1)
        .returning(|_, _| Ok(command_output(1, Vec::new())));
    expect_listing(&mut runner, "crates", b"crates/a.rs\0");
    let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
        .with_command_runner(Arc::new(runner));
    let arguments = serde_json::from_value(json!({
        "action": "search",
        "query": "ReadTool",
        "path": "crates"
    }))
    .expect("search arguments should be valid");

    // Act
    let (result, summary) = tool
        .execute_inspection(&arguments, &unbounded)
        .await
        .expect("empty search should succeed");
    let result: Value = serde_json::from_str(&result).expect("search result should be JSON");

    // Assert
    assert_eq!(summary, "ReadTool");
    assert_eq!(result["result"], json!([]));
    assert_eq!(result["truncated"], false);
}

#[test]
fn bounds_escaping_heavy_inspection_results_after_json_encoding() {
    // Arrange
    let items = (0..1_000).map(|_| "\u{1}".repeat(100)).collect::<Vec<_>>();
    let text = format!("{}\n", "\u{1}".repeat(99)).repeat(MAX_READ_BYTES / 100);

    // Act
    let items = ReadTool::bounded_items_result("list", &items, false, &unbounded);
    let text =
        ReadTool::bounded_text_result("diff", &text, false, Some(COMPARISON_OID), &unbounded)
            .expect("text result should encode");
    let items_value: Value = serde_json::from_str(&items).expect("items should be JSON");
    let text_value: Value = serde_json::from_str(&text).expect("text should be JSON");

    // Assert
    assert!(items.len() <= MAX_TOOL_RESULT_BYTES);
    assert!(text.len() <= MAX_TOOL_RESULT_BYTES);
    assert_eq!(items_value["truncated"], true);
    assert_eq!(text_value["truncated"], true);
    assert!(
        text_value["result"]
            .as_str()
            .is_some_and(|result| !result.is_empty() && result.ends_with('\n'))
    );
    assert_eq!(text_value["comparison_base"], COMPARISON_OID);
}
