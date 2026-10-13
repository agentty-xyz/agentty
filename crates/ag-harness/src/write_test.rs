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
    assert_eq!(updated.content, b"one\nthree\n");
    assert_eq!(updated.adjusted_hunks, Vec::new());
    assert_eq!(created.content, b"hello\nworld\n");
    assert_eq!(empty_file.content, b"");
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
    assert_eq!(output.content, b"\xef\xbb\xbfnew");
    assert_eq!(regular.content, b"one\r\nthree\r\n");
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
    assert_eq!(output.content, b"old\nnew\n");
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
    let error = apply_unified_diff("file", Some(b"first\nsecond\ntarget\n"), patch)
        .expect_err("out-of-order hunks should fail");

    // Assert
    assert!(matches!(error, WriteError::Patch { .. }));
    assert!(
        error
            .to_string()
            .contains("hunk 2 matches the target only at line 1")
    );
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
    assert_eq!(output.content, b"first\ninserted\nsecond\n");
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
    assert_eq!(output.content, expected.as_bytes());
}

fn numbered_lines(count: usize, replacements: &[(usize, &str)]) -> String {
    let mut text = String::new();
    for number in 1..=count {
        match replacements.iter().find(|(line, _)| *line == number) {
            Some((_, replacement)) => writeln!(text, "{replacement}"),
            None => writeln!(text, "line-{number}"),
        }
        .expect("string write should succeed");
    }

    text
}

#[test]
fn recounts_hunk_line_counts_from_the_body() {
    // Arrange
    let patches = [
        "--- a/file\n+++ b/file\n@@ -1,5 +1,9 @@\n one\n-two\n+three\n",
        "--- a/file\n+++ b/file\n@@ -1,2 +1 @@\n one\n+between\n",
        "--- a/file\n+++ b/file\n@@ -1 +1,2 @@\n one\n two\n+three\n",
    ];

    // Act
    let outputs = patches.map(|patch| {
        apply_unified_diff("file", Some(b"one\ntwo\n"), patch)
            .expect("miscounted header should apply after recounting")
    });

    // Assert
    assert_eq!(outputs[0].content, b"one\nthree\n");
    assert_eq!(outputs[1].content, b"one\nbetween\ntwo\n");
    assert_eq!(outputs[2].content, b"one\ntwo\nthree\n");
    assert!(
        outputs
            .iter()
            .all(|output| output.adjusted_hunks.is_empty())
    );
}

#[test]
fn rejects_empty_hunks_and_unanchored_insertions() {
    // Arrange
    let empty_hunk = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n";
    let unanchored = "--- a/file\n+++ b/file\n@@ -1 +2 @@\n+inserted\n";

    // Act
    let empty_error = parse_unified_diff(empty_hunk).expect_err("empty hunk should fail");
    let unanchored_error = apply_unified_diff("file", Some(b"one\ntwo\n"), unanchored)
        .expect_err("insertion without a zero-count anchor should fail");

    // Assert
    assert!(empty_error.to_string().contains("no patch lines"));
    assert!(unanchored_error.to_string().contains("`-N,0`"));
}

#[test]
fn relocates_hunk_with_window_relative_header_to_its_unique_match() {
    // Arrange
    let current = numbered_lines(300, &[(222, "    value = compute()")]);
    let patch = concat!(
        "--- a/file\n+++ b/file\n@@ -19,3 +19,4 @@\n",
        " line-221\n",
        "-    value = compute()\n",
        "+    value = compute()?\n",
        "+    check(value)\n",
        " line-223\n",
    );

    // Act
    let output = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect("unique hunk should relocate");

    // Assert
    let expected = numbered_lines(300, &[(222, "    value = compute()?\n    check(value)")]);
    assert_eq!(output.content, expected.as_bytes());
    assert_eq!(
        serde_json::to_value(&output.adjusted_hunks).expect("adjustments should encode"),
        json!([{ "applied_line": 221, "header_line": 19, "hunk": 1, "match": "exact" }])
    );
}

#[test]
fn prefers_exact_header_match_over_other_identical_blocks() {
    // Arrange
    let current = numbered_lines(100, &[(10, "    return None"), (95, "    return None")]);
    let patch = "--- a/file\n+++ b/file\n@@ -95 +95 @@\n-    return None\n+    return value\n";

    // Act
    let output = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect("header line should select the matching block");

    // Assert
    let expected = numbered_lines(100, &[(10, "    return None"), (95, "    return value")]);
    assert_eq!(output.content, expected.as_bytes());
    assert_eq!(output.adjusted_hunks, Vec::new());
}

#[test]
fn prefers_trailing_whitespace_header_match_over_exact_match_elsewhere() {
    // Arrange
    let current = numbered_lines(60, &[(10, "    retry();  "), (50, "    retry();")]);
    let patch = "--- a/file\n+++ b/file\n@@ -10 +10 @@\n-    retry();\n+    retry(3);\n";

    // Act
    let output = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect("whitespace-tolerant header match should apply");

    // Assert
    let expected = numbered_lines(60, &[(10, "    retry(3);"), (50, "    retry();")]);
    assert_eq!(output.content, expected.as_bytes());
    assert_eq!(
        serde_json::to_value(&output.adjusted_hunks).expect("adjustments should encode"),
        json!([{
            "applied_line": 10,
            "header_line": 10,
            "hunk": 1,
            "match": "ignoring_trailing_whitespace"
        }])
    );
}

#[test]
fn rejects_insertions_before_earlier_hunks_or_past_the_end() {
    // Arrange
    let current = b"one\ntwo\nthree\n";
    let backwards = "--- a/file\n+++ b/file\n@@ -3 +3 @@\n-three\n+3\n@@ -1,0 +2 @@\n+inserted\n";
    let past_end = "--- a/file\n+++ b/file\n@@ -4,0 +5 @@\n+inserted\n";

    // Act
    let backwards_error = apply_unified_diff("file", Some(current), backwards)
        .expect_err("insertion before an earlier hunk should fail");
    let past_end_error = apply_unified_diff("file", Some(current), past_end)
        .expect_err("insertion past the end should fail");

    // Assert
    assert!(
        backwards_error
            .to_string()
            .contains("ascending, non-overlapping")
    );
    assert!(past_end_error.to_string().contains("beyond the end"));
}

#[test]
fn rejects_relocation_with_multiple_candidates() {
    // Arrange
    let current = numbered_lines(100, &[(10, "    return None"), (95, "    return None")]);
    let patch = "--- a/file\n+++ b/file\n@@ -48 +48 @@\n-    return None\n+    return value\n";
    let repeated = "x\n".repeat(12);
    let repeated_patch = "--- a/file\n+++ b/file\n@@ -40 +40 @@\n-x\n+y\n";

    // Act
    let error = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect_err("ambiguous hunk should fail");
    let repeated_error = apply_unified_diff("file", Some(repeated.as_bytes()), repeated_patch)
        .expect_err("ambiguous hunk should fail");

    // Assert
    assert!(matches!(error, WriteError::Patch { .. }));
    assert!(error.to_string().contains("hunk 1 does not match"));
    assert!(error.to_string().contains("at lines 10, 95;"));
    assert!(error.to_string().contains("re-read the file"));
    assert!(
        repeated_error
            .to_string()
            .contains("at lines 1, 2, 3, 4, 5, 6, 7, 8, ...;")
    );
}

#[test]
fn rejects_hunk_that_matches_nowhere() {
    // Arrange
    let cases = [
        (
            b"old".as_slice(),
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
        ),
        (
            b"a\n".as_slice(),
            "--- a/file\n+++ b/file\n@@ -7,3 +7,3 @@\n a\n-b\n+c\n d\n",
        ),
    ];

    // Act
    let errors = cases.map(|(current, patch)| {
        apply_unified_diff("file", Some(current), patch).expect_err("unmatched hunk should fail")
    });

    // Assert
    assert!(
        errors[0]
            .to_string()
            .contains("at header line 1 or anywhere else")
    );
    assert!(
        errors[1]
            .to_string()
            .contains("at header line 7 or anywhere else")
    );
}

#[test]
fn relocates_only_unique_trailing_whitespace_insensitive_matches() {
    // Arrange
    let current = "keep  \ntarget \nend\n";
    let patch = "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n keep\n-target\n+changed\n";
    let ambiguous = "same \nsame\t\n";
    let ambiguous_patch = "--- a/file\n+++ b/file\n@@ -5 +5 @@\n-same\n+other\n";
    let exact_first = "value \nvalue\n";
    let exact_patch = "--- a/file\n+++ b/file\n@@ -5 +5 @@\n-value\n+other\n";

    // Act
    let output = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect("unique whitespace-insensitive match should apply");
    let ambiguous_error = apply_unified_diff("file", Some(ambiguous.as_bytes()), ambiguous_patch)
        .expect_err("ambiguous whitespace-insensitive match should fail");
    let exact = apply_unified_diff("file", Some(exact_first.as_bytes()), exact_patch)
        .expect("exact match should win over whitespace-insensitive matches");

    // Assert
    assert_eq!(output.content, b"keep  \nchanged\nend\n");
    assert_eq!(
        serde_json::to_value(&output.adjusted_hunks).expect("adjustments should encode"),
        json!([{
            "applied_line": 1,
            "header_line": 1,
            "hunk": 1,
            "match": "ignoring_trailing_whitespace"
        }])
    );
    assert!(
        ambiguous_error
            .to_string()
            .contains("at lines 1, 2 after ignoring trailing whitespace")
    );
    assert_eq!(exact.content, b"value \nother\n");
    assert_eq!(exact.adjusted_hunks[0].applied_line, 2);
}

#[test]
fn relocates_later_hunks_against_content_changed_by_earlier_hunks() {
    // Arrange
    let current = numbered_lines(8, &[(2, "return None"), (6, "return None")]);
    let patch = concat!(
        "--- a/file\n+++ b/file\n",
        "@@ -2 +2 @@\n-return None\n+return first\n",
        "@@ -1 +1 @@\n-return None\n+return second\n",
    );
    let ambiguous_patch = concat!(
        "--- a/file\n+++ b/file\n",
        "@@ -1 +1 @@\n line-1\n",
        "@@ -1 +1 @@\n-return None\n+return second\n",
    );

    // Act
    let output = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect("second hunk should relocate after the first");
    let error = apply_unified_diff("file", Some(current.as_bytes()), ambiguous_patch)
        .expect_err("second hunk should stay ambiguous");

    // Assert
    let expected = numbered_lines(8, &[(2, "return first"), (6, "return second")]);
    assert_eq!(output.content, expected.as_bytes());
    assert_eq!(
        serde_json::to_value(&output.adjusted_hunks).expect("adjustments should encode"),
        json!([{ "applied_line": 6, "header_line": 1, "hunk": 2, "match": "exact" }])
    );
    assert!(error.to_string().contains(
        "hunk 2 does not match at its header line and matches the target at lines 2, 6 (counted \
         after this patch's earlier hunks)"
    ));
}

#[test]
fn rejects_hunks_too_repetitive_to_locate() {
    // Arrange
    let current = "x\n".repeat(1024 * 1024);
    let mut patch = String::from("--- a/file\n+++ b/file\n@@ -1,9 +1,9 @@\n");
    for _ in 0..8 {
        patch.push_str(" x\n");
    }
    patch.push_str("-z\n+y\n");

    // Act
    let error = apply_unified_diff("file", Some(current.as_bytes()), &patch)
        .expect_err("repetitive search should stop at the comparison limit");

    // Assert
    assert!(error.to_string().contains("too repetitive"));
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
async fn write_tool_reports_relocated_hunks_in_its_result() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"first\nold\n".to_vec()))));
    file_system
        .expect_replace_beneath()
        .times(1)
        .withf(|_, _, _, content| content == b"first\nnew\n")
        .returning(|_, _, _, _| Ok(()));
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -7,1 +7,1 @@\n-old\n+new\n",
    );

    // Act
    let output = tool
        .execute(&arguments, "write-call")
        .await
        .expect("relocated write should succeed");

    // Assert
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &output.to_tool_result().expect("result should encode")
        )
        .expect("result should be JSON"),
        json!({
            "adjusted_hunks": [
                { "applied_line": 2, "header_line": 7, "hunk": 1, "match": "exact" }
            ],
            "bytes_written": 10,
            "path": "file.txt",
            "status": "applied"
        })
    );
}

#[tokio::test]
async fn write_tool_rejects_whole_patch_when_a_later_hunk_fails() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"one\ntwo\nthree\n".to_vec()))));
    file_system.expect_replace_beneath().times(0);
    let tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    let arguments = arguments(
        "file.txt",
        concat!(
            "--- a/file.txt\n+++ b/file.txt\n",
            "@@ -1 +1 @@\n-one\n+uno\n",
            "@@ -3 +3 @@\n-missing\n+tres\n",
        ),
    );

    // Act
    let error = tool
        .execute(&arguments, "write-call")
        .await
        .expect_err("patch with an unmatched hunk should fail");

    // Assert
    assert!(error.is_model_correctable());
    assert!(error.to_string().contains("hunk 2 context"));
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
