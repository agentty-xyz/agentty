use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::support::{command_output, inspection_file_system};
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
        .execute_inspection(&arguments)
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
        .execute_inspection(&arguments)
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
    let text = "\u{1}".repeat(MAX_READ_BYTES);

    // Act
    let items = ReadTool::bounded_items_result("list", &items, false);
    let text =
        ReadTool::bounded_text_result("diff", &text, false).expect("text result should encode");
    let items_value: Value = serde_json::from_str(&items).expect("items should be JSON");
    let text_value: Value = serde_json::from_str(&text).expect("text should be JSON");

    // Assert
    assert!(items.len() <= MAX_TOOL_RESULT_BYTES);
    assert!(text.len() <= MAX_TOOL_RESULT_BYTES);
    assert_eq!(items_value["truncated"], true);
    assert_eq!(text_value["truncated"], true);
}
