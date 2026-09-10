use std::fmt::Write as _;
use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use serde_json::json;
use tokio::io::{AsyncRead, ReadBuf};

use super::{MAX_FILE_BYTES, WriteError, WriteTool, apply_unified_diff, parse_unified_diff};
use crate::file_system::MockFileSystem;
use crate::tool::WriteArguments;

fn arguments(file_path: &str, unified_diff: &str) -> WriteArguments {
    serde_json::from_value(json!({ "path": file_path, "patch": unified_diff }))
        .expect("write arguments should be valid")
}

fn rooted_file_system() -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .once()
        .returning(|_| Ok(PathBuf::from("/repo")));

    file_system
}

struct FailingReader;

impl AsyncRead for FailingReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("read failed")))
    }
}

#[test]
fn applies_update_and_create_patches() {
    // Arrange
    let update = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n";
    let create = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n";
    let empty_patch = "--- /dev/null\n+++ b/empty.txt\n";

    // Act
    let updated = apply_unified_diff("src/lib.rs", Some(b"one\ntwo\n"), update)
        .expect("update patch should apply");
    let created = apply_unified_diff("new.txt", None, create).expect("create patch should apply");
    let empty_file = apply_unified_diff("empty.txt", None, empty_patch)
        .expect("empty create patch should apply");

    // Assert
    assert_eq!(updated, b"one\nthree\n");
    assert_eq!(created, b"hello\nworld\n");
    assert_eq!(empty_file, b"");
}

#[test]
fn preserves_crlf_bom_and_final_newline_conventions() {
    // Arrange
    let patch = concat!(
        "--- a/file.txt\r\n",
        "+++ b/file.txt\r\n",
        "@@ -1 +1 @@\r\n",
        "-old\r\n",
        "\\ No newline at end of file\r\n",
        "+new\r\n",
        "\\ No newline at end of file\r\n",
    );
    let current = b"\xef\xbb\xbfold";

    // Act
    let output = apply_unified_diff("file.txt", Some(current), patch)
        .expect("patch should preserve text conventions");
    let regular = apply_unified_diff(
        "file.txt",
        Some(b"one\r\ntwo\r\n"),
        "--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n",
    )
    .expect("patch should preserve regular CRLF text");

    // Assert
    assert_eq!(output, b"\xef\xbb\xbfnew");
    assert_eq!(regular, b"one\r\nthree\r\n");
}

#[test]
fn permits_old_side_termination_before_new_additions() {
    // Arrange
    let patch = concat!(
        "--- a/file.txt\n",
        "+++ b/file.txt\n",
        "@@ -1 +1,2 @@\n",
        "-old\n",
        "\\ No newline at end of file\n",
        "+old\n",
        "+new\n",
    );

    // Act
    let output = apply_unified_diff("file.txt", Some(b"old"), patch)
        .expect("new-side additions may follow old-side termination");

    // Assert
    assert_eq!(output, b"old\nnew\n");
}

#[test]
fn rejects_new_side_termination_before_unchanged_output() {
    // Arrange
    let patches = [
        concat!(
            "--- a/file.txt\n",
            "+++ b/file.txt\n",
            "@@ -1 +1,2 @@\n",
            " first\n",
            "+joined\n",
            "\\ No newline at end of file\n",
        ),
        concat!(
            "--- a/file.txt\n",
            "+++ b/file.txt\n",
            "@@ -1,0 +2 @@\n",
            "+joined\n",
            "\\ No newline at end of file\n",
            "@@ -3 +2,0 @@\n",
            "-third\n",
        ),
    ];

    // Act
    let errors = patches.map(|patch| {
        apply_unified_diff("file.txt", Some(b"first\nsecond\nthird\n"), patch)
            .expect_err("unchanged output cannot follow new-side termination")
    });

    // Assert
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, WriteError::Patch { .. }))
    );
}

#[test]
fn rejects_malformed_unified_diffs() {
    // Arrange
    let patches = [
        "",
        "--- a/file\n",
        "bad\n+++ b/file\n@@ -0,0 +1 @@\n+x\n",
        "--- \n+++ b/file\n@@ -0,0 +1 @@\n+x\n",
        "--- a/file\n+++ b/file\nnot-a-hunk\n",
        "--- a/file\n+++ b/file\n@@ -1 1 @@\n x\n",
        "--- a/file\n+++ b/file\n@@ -x +1 @@\n x\n",
        "--- a/file\n+++ b/file\n@@ -1 +x @@\n x\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n?x\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n\\ No newline at end of file\n",
        concat!(
            "--- a/file\n+++ b/file\n@@ -0,0 +1,2 @@\n",
            "+first\n\\ No newline at end of file\n+second\n",
        ),
        concat!(
            "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n",
            " first\n\\ No newline at end of file\n-second\n+second\n",
        ),
        concat!(
            "--- a/file\n+++ /dev/null\n@@ -1 +0,0 @@\n",
            "-old\n\\ No newline at end of file\n",
            "\\ No newline at end of file\n",
        ),
        "--- a/file\n+++ b/file\n@@ -1,2 +1 @@\n x\n",
        "--- a/file\n+++ b/file\n@@ -1 +1,2 @@\n x\n",
        "--- a/file\r+++ b/file\r@@ -1 +1 @@\r x",
    ];

    // Act
    let errors = patches
        .map(|patch| parse_unified_diff(patch).expect_err("malformed patch should be rejected"));

    // Assert
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, WriteError::Patch { .. }))
    );
}

#[test]
fn rejects_delete_rename_and_header_mismatch() {
    // Arrange
    let cases = [
        (
            "file",
            Some(b"x".as_slice()),
            "--- a/file\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n",
        ),
        (
            "other",
            Some(b"x".as_slice()),
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-x\n+y\n",
        ),
        (
            "file",
            Some(b"x".as_slice()),
            "--- /dev/null\n+++ b/file\n@@ -0,0 +1 @@\n+x\n",
        ),
        (
            "file",
            Some(b"x".as_slice()),
            "--- a/old\n+++ b/file\n@@ -1 +1 @@\n-x\n+y\n",
        ),
        ("file", None, "--- a/file\n+++ b/file\n@@ -0,0 +1 @@\n+x\n"),
    ];

    // Act
    let errors = cases.map(|(path, current, patch)| {
        apply_unified_diff(path, current, patch)
            .expect_err("unsupported file operation should be rejected")
    });

    // Assert
    assert!(matches!(errors[0], WriteError::Unsupported { .. }));
    assert!(matches!(errors[1], WriteError::PathMismatch { .. }));
    assert!(
        errors[2..]
            .iter()
            .all(|error| matches!(error, WriteError::Unsupported { .. }))
    );
}

#[test]
fn rejects_binary_and_mixed_line_ending_targets() {
    // Arrange
    let patch = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";
    let targets = [
        b"\xff".as_slice(),
        b"old\r\nnext\n".as_slice(),
        b"old\rnext".as_slice(),
    ];

    // Act
    let errors = targets.map(|target| {
        apply_unified_diff("file", Some(target), patch)
            .expect_err("unsupported target text should fail")
    });

    // Assert
    assert!(matches!(errors[0], WriteError::BinaryTarget { .. }));
    assert!(
        errors[1..]
            .iter()
            .all(|error| matches!(error, WriteError::Unsupported { .. }))
    );
}

#[test]
fn rejects_hunks_that_do_not_match_target() {
    // Arrange
    let patches = [
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n other\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-other\n+new\n",
        "--- a/file\n+++ b/file\n@@ -3,0 +3,1 @@\n+new\n",
        "--- a/file\n+++ b/file\n@@ -0 +1 @@\n-old\n+new\n",
        "--- a/file\n+++ b/file\n@@ -1,0 +0,1 @@\n+new\n",
        "--- a/file\n+++ b/file\n@@ -2,0 +2,1 @@\n+new\n",
    ];

    // Act
    let errors = patches.map(|patch| {
        apply_unified_diff("file", Some(b"old\n"), patch)
            .expect_err("non-matching hunk should be rejected")
    });

    // Assert
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, WriteError::Patch { .. }))
    );
}

#[test]
fn rejects_out_of_order_hunks_before_offset_adjustment() {
    // Arrange
    let patch = concat!(
        "--- a/file\n",
        "+++ b/file\n",
        "@@ -3 +3,2 @@\n",
        " target\n",
        "+extra\n",
        "@@ -1 +1 @@\n",
        "-first\n",
        "+changed\n",
    );

    // Act
    let error = apply_unified_diff("file", Some(b"first\nfirst\ntarget\n"), patch)
        .expect_err("out-of-order hunks should fail");

    // Assert
    assert!(matches!(error, WriteError::Patch { .. }));
    assert!(error.to_string().contains("ascending"));
}

#[test]
fn applies_zero_count_hunk_at_unified_diff_boundary() {
    // Arrange
    let patch = "--- a/file\n+++ b/file\n@@ -1,0 +2,1 @@\n+inserted\n";

    // Act
    let output = apply_unified_diff("file", Some(b"first\nsecond\n"), patch)
        .expect("standard insertion hunk should apply");

    // Assert
    assert_eq!(output, b"first\ninserted\nsecond\n");
}

#[test]
fn rejects_inconsistent_modified_hunk_boundary() {
    // Arrange
    let patch = "--- a/file\n+++ b/file\n@@ -2,0 +2,1 @@\n+inserted\n";

    // Act
    let error = apply_unified_diff("file", Some(b"first\nsecond\n"), patch)
        .expect_err("inconsistent modified range should fail");

    // Assert
    assert!(matches!(error, WriteError::Patch { .. }));
    assert!(error.to_string().contains("modified range"));
}

#[test]
fn applies_many_insertions_in_linear_order() {
    // Arrange
    let mut current = String::new();
    for index in 0..4_096 {
        writeln!(current, "line-{index}").expect("string write should succeed");
    }
    let mut patch = String::from("--- a/file\n+++ b/file\n");
    let mut inserted = String::new();
    for index in 0..2_000 {
        let new_start = 2_049 + index;
        write!(patch, "@@ -2048,0 +{new_start} @@\n+insert-{index}\n")
            .expect("patch write should succeed");
        writeln!(inserted, "insert-{index}").expect("expected write should succeed");
    }
    let mut expected = String::new();
    for index in 0..4_096 {
        if index == 2_048 {
            expected.push_str(&inserted);
        }
        writeln!(expected, "line-{index}").expect("expected write should succeed");
    }

    // Act
    let output = apply_unified_diff("file", Some(current.as_bytes()), &patch)
        .expect("insertion hunks should apply");

    // Assert
    assert_eq!(output, expected.as_bytes());
}

#[tokio::test]
async fn write_tool_applies_patch_through_file_system_boundary() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system
        .expect_replace_beneath()
        .times(1)
        .withf(|root, path, expected, content| {
            root == Path::new("/repo")
                && path == Path::new("file.txt")
                && expected.as_deref() == Some(b"old\n".as_slice())
                && content == b"new\n"
        })
        .returning(|_, _, _, _| Ok(()));
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n",
    );

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("write should succeed");

    // Assert
    assert_eq!(output.path(), "file.txt");
    assert_eq!(output.bytes_written(), 4);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &output.to_tool_result().expect("result should encode")
        )
        .expect("result should be JSON"),
        json!({ "bytes_written": 4, "path": "file.txt", "status": "applied" })
    );
}

#[tokio::test]
async fn write_tool_creates_missing_file() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Err(io::Error::new(io::ErrorKind::NotFound, "missing")));
    file_system
        .expect_replace_beneath()
        .times(1)
        .withf(|_, _, expected, content| expected.is_none() && content == b"new\n")
        .returning(|_, _, _, _| Ok(()));
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1 @@\n+new\n",
    );

    // Act
    let output = tool
        .execute(&arguments)
        .await
        .expect("missing file should be created");

    // Assert
    assert_eq!(output.bytes_written(), 4);
}

#[tokio::test]
async fn write_tool_rejects_patch_that_makes_no_change() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system.expect_replace_beneath().times(0);
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n old\n",
    );

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("no-op patch should fail");

    // Assert
    assert!(matches!(error, WriteError::NoChange { .. }));
    assert!(error.is_model_correctable());
}

#[tokio::test]
async fn write_tool_returns_typed_replace_failure() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system
        .expect_replace_beneath()
        .times(1)
        .returning(|_, _, _, _| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n",
    );

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("replace failure should be typed");

    // Assert
    assert!(matches!(error, WriteError::WriteTarget { .. }));
    assert!(!error.is_model_correctable());
}

#[tokio::test]
async fn write_tool_returns_typed_boundary_failures() {
    // Arrange
    let mut root_failure = MockFileSystem::new();
    root_failure
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
    let root_tool = WriteTool::new(Arc::new(root_failure), PathBuf::from("repo"));
    let mut read_failure = rooted_file_system();
    read_failure
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));
    let read_tool = WriteTool::new(Arc::new(read_failure), PathBuf::from("repo"));
    let mut content_failure = rooted_file_system();
    content_failure
        .expect_open_beneath()
        .once()
        .returning(|_, _| Ok(Box::new(FailingReader)));
    let content_tool = WriteTool::new(Arc::new(content_failure), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1 @@\n+new\n",
    );

    // Act
    let root_error = root_tool
        .execute(&arguments)
        .await
        .expect_err("missing root should fail");
    let read_error = read_tool
        .execute(&arguments)
        .await
        .expect_err("read boundary failure should fail");
    let content_error = content_tool
        .execute(&arguments)
        .await
        .expect_err("content read failure should fail");

    // Assert
    assert!(matches!(root_error, WriteError::RepositoryRoot { .. }));
    assert!(matches!(read_error, WriteError::ReadTarget { .. }));
    assert!(matches!(content_error, WriteError::ReadTarget { .. }));
    assert!(!root_error.is_model_correctable());
    assert!(!read_error.is_model_correctable());
}

#[tokio::test]
async fn write_tool_bounds_target_and_returns_correctable_rejection() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(vec![b'x'; MAX_FILE_BYTES + 1]))));
    file_system.expect_replace_beneath().times(0);
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-x\n+y\n",
    );

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("oversized target should fail");
    let result = error
        .to_tool_result("file.txt")
        .expect("rejection should encode");

    // Assert
    assert!(matches!(error, WriteError::TargetTooLarge { .. }));
    assert!(error.is_model_correctable());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result).expect("rejection should be JSON")
            ["status"],
        "rejected"
    );
}

#[tokio::test]
async fn write_tool_bounds_resulting_file() {
    // Arrange
    let current = b"x\n".repeat(MAX_FILE_BYTES / 2);
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .return_once(move |_, _| Ok(Box::new(Cursor::new(current))));
    file_system.expect_replace_beneath().times(0);
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -1048576,0 +1048577,1 @@\n+extra\n",
    );

    // Act
    let error = tool
        .execute(&arguments)
        .await
        .expect_err("oversized result should fail");

    // Assert
    assert!(matches!(error, WriteError::TargetTooLarge { .. }));
}

#[test]
fn classifies_stale_write_as_model_correctable() {
    // Arrange
    let stale = WriteError::WriteTarget {
        path: "file.txt".to_string(),
        source: io::Error::new(io::ErrorKind::InvalidData, "stale"),
    };
    let denied = WriteError::WriteTarget {
        path: "file.txt".to_string(),
        source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
    };
    let existing = WriteError::WriteTarget {
        path: "file.txt".to_string(),
        source: io::Error::new(io::ErrorKind::AlreadyExists, "existing"),
    };
    let unrelated_missing = WriteError::WriteTarget {
        path: "file.txt".to_string(),
        source: io::Error::new(io::ErrorKind::NotFound, "unrelated missing path"),
    };

    // Act and Assert
    assert!(stale.is_model_correctable());
    assert!(existing.is_model_correctable());
    assert!(!denied.is_model_correctable());
    assert!(!unrelated_missing.is_model_correctable());
}
