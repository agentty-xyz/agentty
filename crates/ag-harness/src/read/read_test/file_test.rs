use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use mockall::Sequence;
use serde_json::Value;

use super::support::{
    ContentThenFailReader, FailingReader, arguments, file_system, file_system_reader,
};
use crate::file_system::MockFileSystem;
use crate::read::output::ReadError;
use crate::read::runtime::{MAX_READ_BYTES, MAX_READ_LINES, MAX_SCAN_BYTES, ReadTool};
use crate::tool::MAX_TOOL_RESULT_BYTES;

#[test]
fn public_read_error_contract_remains_exhaustive() {
    // Arrange
    let error = ReadError::OutsideRepository {
        path: "outside.rs".to_string(),
    };

    // Act
    match &error {
        ReadError::RepositoryRoot { .. }
        | ReadError::ResolvePath { .. }
        | ReadError::OutsideRepository { .. }
        | ReadError::Open { .. }
        | ReadError::Read { .. }
        | ReadError::OffsetBeyondEnd { .. }
        | ReadError::LineTooLong { .. }
        | ReadError::InvalidUtf8 { .. }
        | ReadError::ScanLimitExceeded { .. }
        | ReadError::Encode(_) => {}
    }

    // Assert
    assert!(matches!(error, ReadError::OutsideRepository { .. }));
}

#[tokio::test]
async fn reads_requested_lines_and_reports_continuation() {
    // Arrange
    let tool = ReadTool::new(file_system("one\r\ntwo\nthree\nfour\n"), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "offset": 2,
        "limit": 2
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("bounded read should succeed");

    // Assert
    assert_eq!(output.content(), "two\nthree");
    assert_eq!(output.path(), "input.txt");
    assert_eq!(output.start_line(), 2);
    assert_eq!(output.end_line(), Some(3));
    assert_eq!(output.next_offset(), Some(4));
    assert!(output.truncated());
    assert_eq!(
        output.to_tool_result().expect("output should serialize"),
        r#"{"content":"two\nthree","end_line":3,"next_offset":4,"path":"input.txt","start_line":2,"truncated":true}"#
    );
}

#[tokio::test]
async fn bounds_serialized_read_result_with_escaping_heavy_content() {
    // Arrange
    let content = format!("{}\n", "\u{1}".repeat(100)).repeat(480);
    let tool = ReadTool::new(file_system(content), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("raw read should succeed");
    let result = output
        .to_tool_result()
        .expect("encoded read should be bounded");
    let result_value: Value =
        serde_json::from_str(&result).expect("bounded read result should be JSON");

    // Assert
    assert!(result.len() <= MAX_TOOL_RESULT_BYTES);
    assert_eq!(result_value["truncated"], true);
    assert!(
        result_value["next_offset"]
            .as_u64()
            .is_some_and(|offset| offset > 1)
    );
}

#[tokio::test]
async fn rejects_one_escaping_heavy_line_that_cannot_fit_encoded_result() {
    // Arrange
    let tool = ReadTool::new(file_system("\u{1}".repeat(20_000)), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("raw line should fit the read limit");
    let error = output
        .to_tool_result()
        .expect_err("encoded line should be rejected without aborting the turn");

    // Assert
    assert!(matches!(
        error,
        ReadError::LineTooLong { line: 1, path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn reads_empty_file_without_truncation() {
    // Arrange
    let tool = ReadTool::new(file_system(Vec::new()), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("empty file should be readable");

    // Assert
    assert_eq!(output.content(), "");
    assert_eq!(output.end_line(), None);
    assert_eq!(output.next_offset(), None);
    assert!(!output.truncated());
}

#[tokio::test]
async fn preserves_leading_and_consecutive_blank_lines() {
    // Arrange
    let tool = ReadTool::new(file_system("\n\nvalue\n\n"), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": 4
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("blank lines should be preserved");

    // Assert
    assert_eq!(output.content(), "\n\nvalue\n");
    assert_eq!(output.start_line(), 1);
    assert_eq!(output.end_line(), Some(4));
    assert_eq!(output.next_offset(), None);
}

#[tokio::test]
async fn reads_to_exact_end_without_truncation() {
    // Arrange
    let tool = ReadTool::new(file_system("one\ntwo"), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": 2
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("complete bounded read should succeed");

    // Assert
    assert_eq!(output.content(), "one\ntwo");
    assert_eq!(output.end_line(), Some(2));
    assert_eq!(output.next_offset(), None);
    assert!(!output.truncated());
}

#[tokio::test]
async fn caps_requested_line_count() {
    // Arrange
    let line_count =
        usize::try_from(MAX_READ_LINES + 1).expect("read line limit should fit the platform");
    let content = "line\n".repeat(line_count);
    let tool = ReadTool::new(file_system(content), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": u64::MAX
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("line-bounded read should succeed");

    // Assert
    assert_eq!(output.end_line(), Some(MAX_READ_LINES));
    assert_eq!(output.next_offset(), Some(MAX_READ_LINES + 1));
    assert!(output.truncated());
}

#[tokio::test]
async fn bounds_output_by_bytes() {
    // Arrange
    let first_line = "a".repeat(MAX_READ_BYTES - 1);
    let content = format!("{first_line}\nsecond\n");
    let tool = ReadTool::new(file_system(content), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("byte-bounded read should succeed");

    // Assert
    assert_eq!(output.content(), first_line);
    assert_eq!(output.next_offset(), Some(2));
    assert!(output.truncated());
}

#[tokio::test]
async fn accepts_exact_byte_limit_before_lf() {
    // Arrange
    let expected = "x".repeat(MAX_READ_BYTES);
    let tool = ReadTool::new(file_system(format!("{expected}\n")), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": 1
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("line at the normalized byte limit should succeed");

    // Assert
    assert_eq!(output.content(), expected);
    assert_eq!(output.end_line(), Some(1));
    assert!(!output.truncated());
}

#[tokio::test]
async fn accepts_exact_byte_limit_before_crlf() {
    // Arrange
    let expected = "x".repeat(MAX_READ_BYTES);
    let tool = ReadTool::new(file_system(format!("{expected}\r\n")), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": 1
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("CRLF line at the normalized byte limit should succeed");

    // Assert
    assert_eq!(output.content(), expected);
    assert_eq!(output.end_line(), Some(1));
    assert!(!output.truncated());
}

#[tokio::test]
async fn does_not_validate_unrequested_oversized_line() {
    // Arrange
    let content = format!("one\n{}", "x".repeat(MAX_READ_BYTES + 1));
    let tool = ReadTool::new(file_system(content), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": 1
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("unrequested line should only be probed for presence");

    // Assert
    assert_eq!(output.content(), "one");
    assert_eq!(output.end_line(), Some(1));
    assert_eq!(output.next_offset(), Some(2));
    assert!(output.truncated());
}

#[tokio::test]
async fn skips_unrequested_oversized_prefix_line() {
    // Arrange
    let content = format!("{}\nvalue\n", "x".repeat(MAX_READ_BYTES + 1));
    let tool = ReadTool::new(file_system(content), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "offset": 2,
        "limit": 1
    }));

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("unrequested prefix line should be discarded");

    // Assert
    assert_eq!(output.content(), "value");
    assert_eq!(output.start_line(), 2);
    assert_eq!(output.end_line(), Some(2));
    assert_eq!(output.next_offset(), None);
}

#[tokio::test]
async fn rejects_reads_that_exceed_scan_budget() {
    // Arrange
    let tool = ReadTool::new(file_system(vec![b'x'; MAX_SCAN_BYTES + 1]), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "offset": 2
    }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("prefix scan beyond the byte budget should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::ScanLimitExceeded { limit, path }
            if limit == MAX_SCAN_BYTES && path == "input.txt"
    ));
}

#[tokio::test]
async fn reports_continuation_probe_failure() {
    // Arrange
    let reader = ContentThenFailReader {
        content: Some(b"one\n".to_vec()),
    };
    let tool = ReadTool::new(file_system_reader(Box::new(reader)), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "limit": 1
    }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("failed continuation probe should fail the read");

    // Assert
    assert!(matches!(
        error,
        ReadError::Read { path, source }
            if path == "input.txt" && source.kind() == io::ErrorKind::Other
    ));
}

#[tokio::test]
async fn reports_failure_while_skipping_prefix() {
    // Arrange
    let tool = ReadTool::new(file_system_reader(Box::new(FailingReader)), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "offset": 2
    }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("failed prefix discard should fail the read");

    // Assert
    assert!(matches!(
        error,
        ReadError::Read { path, source }
            if path == "input.txt" && source.kind() == io::ErrorKind::Other
    ));
}

#[tokio::test]
async fn rejects_offset_beyond_end() {
    // Arrange
    let tool = ReadTool::new(file_system("one\n"), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "offset": 3
    }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("out-of-range offset should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::OffsetBeyondEnd { offset: 3, path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn rejects_offset_after_unterminated_final_line() {
    // Arrange
    let tool = ReadTool::new(file_system("one"), "repo".into());
    let arguments = arguments(serde_json::json!({
        "path": "input.txt",
        "offset": 2
    }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("offset after an unterminated final line should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::OffsetBeyondEnd { offset: 2, path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn rejects_oversized_line_without_unbounded_read() {
    // Arrange
    let tool = ReadTool::new(file_system(vec![b'x'; MAX_READ_BYTES + 1]), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("oversized line should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::LineTooLong { line: 1, path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn rejects_invalid_utf8() {
    // Arrange
    let tool = ReadTool::new(file_system(vec![0xff, b'\n']), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("invalid UTF-8 should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::InvalidUtf8 { line: 1, path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn rejects_path_that_resolves_outside_repository() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/outside/input.txt")));
    file_system.expect_open_beneath().times(0);
    let tool = ReadTool::new(Arc::new(file_system), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("escaping canonical path should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::OutsideRepository { path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn rejects_path_that_resolves_to_repository_root() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(2)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system.expect_open_beneath().times(0);
    let tool = ReadTool::new(Arc::new(file_system), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("repository directory should not be readable as a file");

    // Assert
    assert!(matches!(
        error,
        ReadError::OutsideRepository { path } if path == "input.txt"
    ));
}

#[tokio::test]
async fn reports_path_resolution_failure() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing file")));
    file_system.expect_open_beneath().times(0);
    let tool = ReadTool::new(Arc::new(file_system), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("missing file should fail path resolution");

    // Assert
    assert!(matches!(
        error,
        ReadError::ResolvePath { path, source }
            if path == "input.txt" && source.kind() == io::ErrorKind::NotFound
    ));
}

#[tokio::test]
async fn reports_file_open_failure() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo/input.txt")));
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "permission denied",
            ))
        });
    let tool = ReadTool::new(Arc::new(file_system), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("unopenable file should fail");

    // Assert
    assert!(matches!(
        error,
        ReadError::Open { path, source }
            if path == "input.txt" && source.kind() == io::ErrorKind::PermissionDenied
    ));
}

#[tokio::test]
async fn reports_file_read_failure() {
    // Arrange
    let tool = ReadTool::new(file_system_reader(Box::new(FailingReader)), "repo".into());
    let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("broken stream should fail the read");

    // Assert
    assert!(matches!(
        error,
        ReadError::Read { path, source }
            if path == "input.txt" && source.kind() == io::ErrorKind::Other
    ));
}
