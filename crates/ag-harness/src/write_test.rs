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
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n\n\n",
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
        "--- a/file\n+++ b/file\n@@ -2,0 +2,1 @@\n+new\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n-extra\n+new\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n",
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
fn applies_hunks_with_miscounted_or_missing_header_ranges() {
    // Arrange
    let current = b"first\nsecond\nthird\n";
    let patches = [
        "--- a/file\n+++ b/file\n@@ -2,7 +2,9 @@\n second\n-third\n+changed\n",
        "--- a/file\n+++ b/file\n@@ -40 +40 @@\n second\n-third\n+changed\n",
        "--- a/file\n+++ b/file\n@@\n second\n-third\n+changed\n",
        "--- a/file\n+++ b/file\n@@ -x +1 @@ fn main\n second\n-third\n+changed\n",
        "--- a/file\n+++ b/file\n@@ -0 +1 @@\n second\n-third\n+changed\n",
    ];

    // Act
    let outputs = patches.map(|patch| {
        apply_unified_diff("file", Some(current), patch).expect("miscounted hunk should apply")
    });

    // Assert
    assert!(
        outputs
            .iter()
            .all(|output| output == b"first\nsecond\nchanged\n")
    );
}

#[test]
fn locates_hunks_by_whitespace_tolerant_context_nearest_the_hint() {
    // Arrange
    let current = b"fn a() {\n    value\n}\nfn b() {\n    value\n}\n";
    let trailing =
        "--- a/file\n+++ b/file\n@@ -4,3 +4,3 @@\n fn b() {   \n-    value\n+    other\n";
    let indented = "--- a/file\n+++ b/file\n@@ -5 +5 @@\n-value\n+    other\n";
    let blank_context = "--- a/file\n+++ b/file\n@@ -1,3 +1,4 @@\n one\n\n-two\n+three\n\n";

    // Act
    let trailing_output =
        apply_unified_diff("file", Some(current), trailing).expect("trailing spaces tolerated");
    let indented_output =
        apply_unified_diff("file", Some(current), indented).expect("indentation tolerated");
    let blank_output = apply_unified_diff("file", Some(b"one\n\ntwo\n"), blank_context)
        .expect("bare blank context line should apply");

    // Assert
    assert_eq!(
        trailing_output,
        b"fn a() {\n    value\n}\nfn b() {\n    other\n}\n"
    );
    assert_eq!(
        indented_output,
        b"fn a() {\n    value\n}\nfn b() {\n    other\n}\n"
    );
    assert_eq!(blank_output, b"one\n\nthree\n");
}

#[test]
fn rejects_insertion_hunks_without_a_zero_count_anchor() {
    // Arrange
    let patches = [
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n+inserted\n",
        "--- a/file\n+++ b/file\n@@ -1,1 +1,2 @@\n+inserted\n",
        "--- a/file\n+++ b/file\n@@ -1,x +1,2 @@\n+inserted\n",
        "--- a/file\n+++ b/file\n@@\n+inserted\n",
    ];

    // Act
    let errors = patches.map(|patch| {
        apply_unified_diff("file", Some(b"first\nsecond\n"), patch)
            .expect_err("unanchored insertion hunk should be rejected")
    });

    // Assert
    assert!(
        errors
            .iter()
            .all(|error| error.to_string().contains("`-N,0` header"))
    );
}

#[test]
fn applies_unanchored_insertion_hunk_to_an_empty_target() {
    // Arrange
    let create = "--- /dev/null\n+++ b/file\n@@\n+created\n";
    let fill = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n+filled\n";

    // Act
    let created = apply_unified_diff("file", None, create).expect("bare create should apply");
    let filled = apply_unified_diff("file", Some(b""), fill).expect("empty fill should apply");

    // Assert
    assert_eq!(created, b"created\n");
    assert_eq!(filled, b"filled\n");
}

#[test]
fn rejects_hunks_whose_context_matches_ambiguously() {
    // Arrange
    let current = b"one\nend\ntwo\nmid\nthree\nend\n";
    let patches = [
        "--- a/file\n+++ b/file\n@@\n-end\n+finish\n",
        "--- a/file\n+++ b/file\n@@ -4 +4 @@\n-end\n+finish\n",
    ];

    // Act
    let errors = patches.map(|patch| {
        apply_unified_diff("file", Some(current), patch)
            .expect_err("ambiguous hunk should be rejected")
    });
    let hinted = apply_unified_diff(
        "file",
        Some(current),
        "--- a/file\n+++ b/file\n@@ -6 +6 @@\n-end\n+finish\n",
    )
    .expect("hint nearest one match should apply");

    // Assert
    // Without a hint both matches tie; line 4 sits two lines from each.
    assert!(
        errors
            .iter()
            .all(|error| error.to_string().contains("more than one position"))
    );
    assert_eq!(hinted, b"one\nend\ntwo\nmid\nthree\nfinish\n");
}

#[test]
fn prefers_exact_context_unless_a_looser_match_sits_nearer_the_hint() {
    // Arrange
    let current = b"foo()  \nbar\nfoo()\nbar\n";
    let unhinted = "--- a/file\n+++ b/file\n@@\n foo()\n-bar\n+baz\n";
    let hinted_exact = "--- a/file\n+++ b/file\n@@ -3,2 +3,2 @@\n foo()\n-bar\n+baz\n";
    let hinted_loose = "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n foo()\n-bar\n+baz\n";

    // Act
    let unhinted_output =
        apply_unified_diff("file", Some(current), unhinted).expect("exact match should win");
    let hinted_output = apply_unified_diff("file", Some(current), hinted_exact)
        .expect("exact match at the hint should apply");
    let conflict = apply_unified_diff("file", Some(current), hinted_loose)
        .expect_err("a looser match nearer the hint should be rejected");

    // Assert
    assert_eq!(unhinted_output, b"foo()  \nbar\nfoo()\nbaz\n");
    assert_eq!(hinted_output, b"foo()  \nbar\nfoo()\nbaz\n");
    assert!(conflict.to_string().contains("ignoring whitespace"));
}

#[test]
fn rejects_hunks_too_repetitive_to_locate() {
    // Arrange
    let current = b"x\n".repeat(MAX_FILE_BYTES / 2);
    let patch = format!(
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n{}-y\n+z\n",
        " x\n".repeat(16)
    );

    // Act
    let error = apply_unified_diff("file", Some(&current), &patch)
        .expect_err("repetitive hunk should exhaust the comparison budget");

    // Assert
    assert!(error.is_model_correctable());
    assert!(error.to_string().contains("comparison limit"));
}

#[test]
fn rejects_out_of_order_hunks() {
    // Arrange
    let context_patch = concat!(
        "--- a/file\n",
        "+++ b/file\n",
        "@@ -3 +3,2 @@\n",
        " target\n",
        "+extra\n",
        "@@ -1 +1 @@\n",
        "-first\n",
        "+changed\n",
    );
    let insertion_patch = concat!(
        "--- a/file\n",
        "+++ b/file\n",
        "@@ -3 +3 @@\n",
        "-target\n",
        "+changed\n",
        "@@ -1,0 +2 @@\n",
        "+inserted\n",
    );

    // Act
    let context_error = apply_unified_diff("file", Some(b"first\nfirst\ntarget\n"), context_patch)
        .expect_err("out-of-order context hunk should fail");
    let insertion_error =
        apply_unified_diff("file", Some(b"first\nfirst\ntarget\n"), insertion_patch)
            .expect_err("out-of-order insertion hunk should fail");

    // Assert
    assert!(context_error.to_string().contains("file order"));
    assert!(insertion_error.to_string().contains("ordered"));
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
        .execute(&arguments, "write-call")
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
        .execute(&arguments, "write-call")
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
        .execute(&arguments, "write-call")
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
        .execute(&arguments, "write-call")
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
        .execute(&arguments, "write-call")
        .await
        .expect_err("missing root should fail");
    let read_error = read_tool
        .execute(&arguments, "write-call")
        .await
        .expect_err("read boundary failure should fail");
    let content_error = content_tool
        .execute(&arguments, "write-call")
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
        .execute(&arguments, "write-call")
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
        .execute(&arguments, "write-call")
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
