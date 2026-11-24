//! `write` tool tests: unified-diff and Codex patch parsing, hunk matching
//! and relocation, line-ending conventions, and rejected patches with the
//! current lines they show.

use std::fmt::Write as _;
use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use serde_json::json;
use tokio::io::{AsyncRead, ReadBuf};

use super::{
    LineMatch, MAX_FILE_BYTES, WriteError, WriteTool, apply_unified_diff, header_target,
    parse_unified_diff,
};
use crate::effect::Effects;
use crate::fault_store_test::{Fault, FaultStore};
use crate::file_system::MockFileSystem;
use crate::reservation;
use crate::store::{NewSession, SessionStore};
use crate::store_fixture::{options, schema};
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
        "--- a/file\n+++ b/file\n@@ -1,x +1 @@\n x\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n?x\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\néx\n",
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
        errors[..2]
            .iter()
            .all(|error| matches!(error, WriteError::ContextMismatch { .. }))
    );
    assert!(
        errors[2..]
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
async fn write_tool_reports_an_unrecorded_intent_without_writing() {
    // Arrange
    let mut file_system = rooted_file_system();
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    let store = Arc::new(FaultStore::new());
    let backend: Arc<dyn SessionStore> = store.clone();
    backend
        .create_session(&NewSession::new("writes", schema()), None, 1024)
        .await
        .expect("session");
    let mut acquired = reservation::acquire(
        Arc::clone(&backend),
        ("writes".into(), 0),
        "run".into(),
        options(),
        None,
        Effects::default(),
    )
    .await
    .expect("acquire");
    store.inject(Fault::WriteIntent);
    let mut tool = WriteTool::new(Arc::new(file_system), PathBuf::from("repo"));
    tool.journal = Some(acquired.guard.write_journal());
    let arguments = arguments(
        "file.txt",
        "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n",
    );

    // Act
    let error = tool
        .execute(&arguments, "write-call")
        .await
        .expect_err("an unrecorded intent stops the write");

    // Assert
    assert!(matches!(error, WriteError::WriteTarget { path, .. } if path == "file.txt"));
    assert_eq!(
        backend.load_writes("writes").await.expect("writes"),
        Vec::new()
    );
    acquired.guard.disarm();
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

#[test]
fn reads_empty_hunk_lines_as_blank_context() {
    // Arrange
    let patch = "--- a/file\n+++ b/file\n@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n";

    // Act
    let patched =
        apply_unified_diff("file", Some(b"a\n\nb\n"), patch).expect("blank context should apply");

    // Assert
    assert_eq!(patched.content, b"a\n\nc\n");
}

#[test]
fn accepts_diffs_in_a_codex_envelope_or_without_file_headers() {
    // Arrange
    let diff = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";
    let cases = [
        (
            Some(b"old\n".as_slice()),
            format!("{diff}*** End Patch\n\n"),
        ),
        (
            Some(b"old\n".as_slice()),
            format!("*** Begin Patch\n{diff}*** End Patch"),
        ),
        (Some(b"old\n".as_slice()), format!("{diff}\n\n")),
        (
            Some(b"old\n".as_slice()),
            "@@ -1 +1 @@\n-old\n+new\n".to_string(),
        ),
        (None, "@@ -0,0 +1 @@\n+new\n".to_string()),
    ];

    // Act
    let results = cases.map(|(current, patch)| {
        apply_unified_diff("file", current, &patch).expect("patch should apply")
    });

    // Assert
    for patched in results {
        assert_eq!(patched.content, b"new\n");
    }
}

#[test]
fn keeps_end_patch_text_that_does_not_fill_a_line() {
    // Arrange
    let patch = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new*** End Patch\n";

    // Act
    let patched = apply_unified_diff("file", Some(b"old\n"), patch).expect("patch should apply");

    // Assert
    assert_eq!(patched.content, b"new*** End Patch\n");
}

#[test]
fn names_the_target_from_patch_headers() {
    // Arrange
    let patches = [
        "--- a/src/lib.rs\n+++ b/src/lib.rs\t2026-01-01\n@@ -1 +1 @@\n-a\n+b\n",
        "*** Begin Patch\n*** Update File: src/lib.rs \n@@\n-a\n+b\n*** End Patch\n",
        "*** Begin Patch\n*** Add File: src/lib.rs\n+a\n*** End Patch\n",
        "--- a/src/lib.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n",
        "@@ -1 +1 @@\n-a\n++++ b/src/lib.rs\n",
        "--- a/src/lib.rs\n+++ \n",
    ];

    // Act
    let targets = patches.map(header_target);

    // Assert
    assert_eq!(
        targets,
        [
            Some("src/lib.rs"),
            Some("src/lib.rs"),
            Some("src/lib.rs"),
            None,
            None,
            None
        ]
    );
}

#[test]
fn shows_current_lines_where_an_unmatched_hunk_most_nearly_matches() {
    // Arrange
    let current = numbered_lines(10, &[]);
    let near = "--- a/file\n+++ b/file\n@@ -2,3 +2,3 @@\n line-5\n line-6\n-line-7 typo\n+new\n";

    // Act
    let error = apply_unified_diff("file", Some(current.as_bytes()), near)
        .expect_err("near miss should be rejected");

    // Assert
    let result = serde_json::from_str::<serde_json::Value>(
        &error.to_tool_result("file").expect("result should encode"),
    )
    .expect("result should be JSON");
    assert!(matches!(error, WriteError::ContextMismatch { .. }));
    assert_eq!(
        result["current"],
        json!({ "content": &current["line-1\n".len()..], "end_line": 10, "start_line": 2 })
    );
    let error_text = result["error"].as_str().expect("error text");
    assert!(error_text.contains("the lines where the hunk most nearly matches are attached"));
    assert!(error_text.contains("`current` holds lines 2-10 of the file as it is now"));
    assert_eq!(result["status"], "rejected");
}

/// Returns the `current` lines a rejected write's tool result shows, as
/// `(start_line, end_line, content)`.
fn shown_lines(error: &WriteError) -> (u64, u64, String) {
    let result = serde_json::from_str::<serde_json::Value>(
        &error.to_tool_result("file").expect("result should encode"),
    )
    .expect("result should be JSON");
    let current = &result["current"];

    (
        current["start_line"].as_u64().expect("start line"),
        current["end_line"].as_u64().expect("end line"),
        current["content"].as_str().expect("content").to_string(),
    )
}

#[test]
fn shows_current_lines_around_the_header_without_a_near_match() {
    // Arrange
    let current = numbered_lines(10, &[]);
    let cases = [
        ("@@ -5 +5 @@\n-zzz\n+y\n", 2, 8),
        ("@@ -50 +50 @@\n-zzz\n+y\n", 7, 10),
        ("@@ -1,2 +1,2 @@\n line-3\n-x\n+y\n", 1, 7),
    ];

    // Act
    let errors = cases.map(|(hunk, _, _)| {
        apply_unified_diff(
            "file",
            Some(current.as_bytes()),
            &format!("--- a/file\n+++ b/file\n{hunk}"),
        )
        .expect_err("unmatched hunk should be rejected")
    });

    // Assert
    for (error, (_, start, end)) in errors.iter().zip(cases) {
        let (start_line, end_line, _) = shown_lines(error);
        assert_eq!((start_line, end_line), (start, end), "{error}");
    }
    assert!(errors[0].to_string().contains("around the hunk's position"));
}

#[test]
fn bounds_the_current_lines_by_count_and_bytes() {
    // Arrange
    let many_lines = numbered_lines(60, &[]);
    let long_lines = format!("{}\n", "x".repeat(999)).repeat(10);
    let many_removed = format!(
        "--- a/file\n+++ b/file\n@@ -1,50 +1 @@\n{}+y\n",
        "-zzz\n".repeat(50)
    );
    let one_removed = "--- a/file\n+++ b/file\n@@ -5 +5 @@\n-zzz\n+y\n";

    // Act
    let by_count = apply_unified_diff("file", Some(many_lines.as_bytes()), &many_removed)
        .expect_err("unmatched hunk should be rejected");
    let by_bytes = apply_unified_diff("file", Some(long_lines.as_bytes()), one_removed)
        .expect_err("unmatched hunk should be rejected");

    // Assert
    let (start_line, end_line, content) = shown_lines(&by_count);
    assert_eq!((start_line, end_line), (1, 40));
    assert_eq!(content, numbered_lines(40, &[]));
    let (start_line, end_line, content) = shown_lines(&by_bytes);
    assert_eq!((start_line, end_line), (2, 5));
    assert_eq!(content.lines().count(), 4);
}

#[test]
fn shows_lines_around_the_header_once_the_nearest_match_exceeds_the_budget() {
    // Arrange
    let current = "x\n".repeat(100_000);
    let patch = format!(
        "--- a/file\n+++ b/file\n@@ -1,100 +1 @@\n{}+z\n",
        "-y\n".repeat(100)
    );

    // Act
    let error = apply_unified_diff("file", Some(current.as_bytes()), &patch)
        .expect_err("unmatched hunk should be rejected");

    // Assert
    assert!(
        error.to_string().contains("around the hunk's position"),
        "{error}"
    );
    assert_eq!(shown_lines(&error).0, 1);
}

#[test]
fn prefers_the_nearest_of_equally_close_matches() {
    // Arrange
    let current = "a\nb\nx\nx\nx\nx\nx\nx\nx\nx\na\nb\n";
    let patch = "--- a/file\n+++ b/file\n@@ -10,2 +10,2 @@\n a\n-c\n+d\n";

    // Act
    let error = apply_unified_diff("file", Some(current.as_bytes()), patch)
        .expect_err("unmatched hunk should be rejected");

    // Assert
    assert!(
        matches!(error, WriteError::ContextMismatch { start_line: 8, .. }),
        "{error:?}"
    );
}

#[test]
fn omits_current_lines_that_cannot_be_shown() {
    // Arrange
    let long_line = format!("{}\n", "x".repeat(MAX_FILE_BYTES / 256));
    let patch = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-y\n+z\n";

    // Act
    let errors = [b"".as_slice(), long_line.as_bytes()].map(|current| {
        apply_unified_diff("file", Some(current), patch).expect_err("hunk should be rejected")
    });

    // Assert
    for error in errors {
        assert!(matches!(error, WriteError::Patch { .. }), "{error:?}");
        assert!(error.to_string().contains("re-read the file"));
    }
}

const CODEX_PATH: &str = "src/lib.rs";

/// Wraps `operations` in a Codex patch envelope.
fn codex_envelope(operations: &str) -> String {
    format!("*** Begin Patch\n{operations}*** End Patch\n")
}

/// Wraps Codex update `hunks` for [`CODEX_PATH`] in a patch envelope.
fn codex_update(hunks: &str) -> String {
    codex_envelope(&format!("*** Update File: {CODEX_PATH}\n{hunks}"))
}

/// Applies `patch` to `current`, the text of [`CODEX_PATH`] when it exists,
/// and returns the resulting text.
fn apply_codex(current: Option<&str>, patch: &str) -> Result<String, WriteError> {
    apply_unified_diff(CODEX_PATH, current.map(str::as_bytes), patch)
        .map(|patched| String::from_utf8(patched.content).expect("result should be UTF-8"))
}

#[test]
fn applies_codex_hunks_in_order_after_their_anchors() {
    // Arrange
    let current = "fn new() {\n    a: 1,\n}\nfn reset() {\n    a: 1,\n}\n";
    let anchored = codex_update("@@ fn reset() {\n-    a: 1,\n+    a: 2,\n");
    let sequential = codex_update(
        "@@ fn new() {\n     a: 1,\n+    b: 1,\n@@ fn reset() {\n     a: 1,\n+    b: 2,\n",
    );
    let context_anchor = codex_update("@@\n fn reset() {\n@@\n     a: 1,\n+    b: 2,\n");
    let repeated = codex_update("@@\n     a: 1,\n+    b: 1,\n");

    // Act
    let anchored = apply_codex(Some(current), &anchored).expect("anchored hunk should apply");
    let sequential =
        apply_codex(Some(current), &sequential).expect("sequential hunks should apply");
    let context_anchor =
        apply_codex(Some(current), &context_anchor).expect("context-only hunk should anchor");
    let repeated = apply_codex(Some(current), &repeated)
        .expect_err("a repeated hunk without an `@@` line must not guess");

    // Assert
    assert!(
        matches!(repeated, WriteError::Patch { .. })
            && repeated
                .to_string()
                .contains("matches more than one place after the previous hunk, at lines 2 and 5"),
        "{repeated}"
    );
    assert_eq!(
        anchored,
        "fn new() {\n    a: 1,\n}\nfn reset() {\n    a: 2,\n}\n"
    );
    assert_eq!(
        sequential,
        "fn new() {\n    a: 1,\n    b: 1,\n}\nfn reset() {\n    a: 1,\n    b: 2,\n}\n"
    );
    assert_eq!(
        context_anchor,
        "fn new() {\n    a: 1,\n}\nfn reset() {\n    a: 1,\n    b: 2,\n}\n"
    );
}

#[test]
fn accepts_unmarked_codex_hunks_range_headers_moves_in_place_and_no_end_line() {
    // Arrange
    let patches = [
        codex_update(" a\n-b\n+c\n"),
        codex_update("@@ -1,2 +1,2 @@\n a\n-b\n+c\n"),
        codex_envelope(&format!(
            "*** Update File: {CODEX_PATH}\n*** Move to: {CODEX_PATH}\n@@\n a\n-b\n+c\n"
        )),
        format!("*** Begin Patch\n*** Update File: {CODEX_PATH}\n@@\n a\n-b\n+c\n"),
        codex_update(" a\n\n-b\n+c\n"),
    ];

    // Act
    let results = patches.each_ref().map(|patch| {
        let current = if patch.contains("\n\n-b") {
            "a\n\nb\n"
        } else {
            "a\nb\n"
        };

        apply_codex(Some(current), patch).expect("patch should apply")
    });

    // Assert
    assert_eq!(results[..4], ["a\nc\n", "a\nc\n", "a\nc\n", "a\nc\n"]);
    assert_eq!(results[4], "a\n\nc\n");
}

#[test]
fn matches_codex_hunks_ignoring_trailing_whitespace_or_at_the_end_of_file() {
    // Arrange
    let tolerant = codex_update("@@\n a\n-b\n+c\n");
    let end_of_file = codex_update("@@\n-x\n+z\n*** End of File\n");

    // Act
    let tolerant = apply_unified_diff(CODEX_PATH, Some(b"a  \nb\n"), &tolerant)
        .expect("whitespace-tolerant hunk should apply");
    let end_of_file =
        apply_codex(Some("x\ny\nx\n"), &end_of_file).expect("end-of-file hunk should apply");

    // Assert
    assert_eq!(tolerant.content, b"a  \nc\n");
    assert_eq!(
        tolerant.adjusted_hunks[0].line_match,
        LineMatch::IgnoringTrailingWhitespace
    );
    assert_eq!(end_of_file, "x\ny\nz\n");
}

#[test]
fn appends_codex_hunks_of_only_added_lines() {
    // Arrange
    let append = codex_update("@@\n+c\n");
    let change_then_append = codex_update("@@\n-a\n+A\n@@\n+d\n");

    // Act
    let terminated = apply_codex(Some("a\nb\n"), &append).expect("append should apply");
    let unterminated = apply_codex(Some("a\nb"), &append).expect("append should apply");
    let after_change = apply_codex(Some("a\nb\nc\n"), &change_then_append)
        .expect("append after a change should apply");

    // Assert
    assert_eq!(terminated, "a\nb\nc\n");
    assert_eq!(unterminated, "a\nb\nc\n");
    assert_eq!(after_change, "A\nb\nc\nd\n");
}

#[test]
fn codex_hunks_keep_or_terminate_an_unterminated_final_line() {
    // Arrange
    let cases = [
        ("@@\n b\n+c\n", "a\nb\nc\n"),
        ("@@\n-a\n+A\n b\n", "A\nb"),
        ("@@\n a\n-b\n", "a\n"),
        ("@@\n-b\n+B\n", "a\nB\n"),
    ];

    // Act
    let results = cases.map(|(hunks, _)| apply_codex(Some("a\nb"), &codex_update(hunks)));

    // Assert
    for (result, (_, expected)) in results.into_iter().zip(cases) {
        assert_eq!(result.expect("hunk should apply"), expected);
    }
}

#[test]
fn creates_files_from_codex_add_operations() {
    // Arrange
    let add = codex_envelope(&format!("*** Add File: {CODEX_PATH}\n+one\n+two\n"));
    let empty = codex_envelope(&format!("*** Add File: {CODEX_PATH}\n"));

    // Act
    let created = apply_codex(None, &add).expect("add should create the file");
    let empty = apply_codex(None, &empty).expect("empty add should create the file");
    let existing = apply_codex(Some("x\n"), &add).expect_err("add must not replace a file");

    // Assert
    assert_eq!(created, "one\ntwo\n");
    assert_eq!(empty, "");
    assert!(matches!(existing, WriteError::Unsupported { .. }));
}

#[test]
fn rejects_unsupported_codex_operations_with_a_correction() {
    // Arrange
    let cases = [
        (
            Some("x\n"),
            codex_envelope(&format!("*** Delete File: {CODEX_PATH}\n")),
        ),
        (
            Some("x\n"),
            codex_envelope(&format!(
                "*** Update File: {CODEX_PATH}\n*** Move to: src/other.rs\n@@\n-x\n+y\n"
            )),
        ),
        (
            Some("x\n"),
            codex_envelope(&format!(
                "*** Update File: {CODEX_PATH}\n@@\n-x\n+y\n*** Update File: \
                 src/other.rs\n@@\n-x\n+y\n"
            )),
        ),
        (None, codex_update("@@\n-x\n+y\n")),
    ];

    // Act
    let errors = cases.map(|(current, patch)| {
        apply_codex(current, &patch).expect_err("operation should be rejected")
    });

    // Assert
    for error in &errors {
        assert!(matches!(error, WriteError::Unsupported { .. }), "{error}");
    }
    assert!(errors[2].to_string().contains("one `write` call per file"));
    assert!(errors[3].to_string().contains("`*** Add File:`"));
}

#[test]
fn rejects_malformed_codex_patches() {
    // Arrange
    let cases = [
        (
            codex_envelope(&format!("*** Add File: {CODEX_PATH}\n one\n")),
            "must start with `+`",
        ),
        (codex_update("@@\n?x\n"), "must start with space"),
        (
            codex_update("@@ a\n@@\n x\n"),
            "hunk 1 contains no patch lines",
        ),
        (
            codex_update("*** End of File\n@@\n x\n"),
            "hunk 1 contains no patch lines",
        ),
        (
            codex_update("@@ a\n+y\n"),
            "only added lines after its `@@` line",
        ),
        (
            codex_update("@@\n-b\n+B\n@@\n+c\n"),
            "merge them into one hunk",
        ),
    ];

    // Act
    let errors = cases.each_ref().map(|(patch, _)| {
        apply_codex(Some("a\nb"), patch).expect_err("malformed patch should be rejected")
    });

    // Assert
    for (error, (_, expected)) in errors.iter().zip(&cases) {
        assert!(matches!(error, WriteError::Patch { .. }), "{error}");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn applies_crlf_codex_patches_and_rejects_lone_carriage_returns() {
    // Arrange
    let crlf = codex_update(" a\n-b\n+c\n").replace('\n', "\r\n");
    let lone_return = codex_update(" a\n-b\n+x\ry\n");

    // Act
    let applied = apply_codex(Some("a\r\nb\r\n"), &crlf).expect("CRLF patch should apply");
    let rejected =
        apply_codex(Some("a\nb\n"), &lone_return).expect_err("a lone carriage return is rejected");

    // Assert
    assert_eq!(applied, "a\r\nc\r\n");
    assert!(
        rejected
            .to_string()
            .contains("unsupported carriage returns")
    );
}

#[test]
fn rejects_codex_hunks_too_repetitive_to_locate() {
    // Arrange
    let current = "x\n".repeat(100_000);
    let patch = codex_update(&format!("@@\n{}-y\n", " x\n".repeat(100)));

    // Act
    let error = apply_codex(Some(&current), &patch).expect_err("search should exhaust its budget");

    // Assert
    assert!(
        error.to_string().contains("too repetitive to locate"),
        "{error}"
    );
}

#[test]
fn rejects_a_codex_patch_for_another_path() {
    // Arrange
    let patch = codex_envelope("*** Update File: src/other.rs\n@@\n-a\n+b\n");

    // Act
    let error = apply_codex(Some("a\n"), &patch).expect_err("other path should be rejected");

    // Assert
    assert!(matches!(error, WriteError::PathMismatch { .. }));
}

#[test]
fn shows_current_lines_for_codex_hunks_and_anchors_that_match_nowhere() {
    // Arrange
    let current = "a\nb\nc\nd\n";
    let patches = [
        codex_update("@@ missing\n a\n"),
        codex_update("@@\n b\n-x\n+y\n"),
        codex_update("@@\n-a\n+z\n*** End of File\n"),
        codex_update("@@\n a\n b\n c\n d\n-e\n"),
    ];

    // Act
    let results = patches.each_ref().map(|patch| {
        let error = apply_codex(Some(current), patch).expect_err("hunk should match nowhere");
        let result = error
            .to_tool_result(CODEX_PATH)
            .expect("result should encode");

        serde_json::from_str::<serde_json::Value>(&result).expect("result should be JSON")
    });

    // Assert
    let errors = results
        .each_ref()
        .map(|result| result["error"].as_str().expect("error text"));
    assert!(errors[0].contains("hunk 1's `@@ missing` line does not occur"));
    assert!(errors[1].contains("where the hunk most nearly matches"));
    assert!(errors[2].contains("at the end of the file"));
    assert!(errors[3].contains("hunk 1 context and removed lines do not occur"));
    assert_eq!(
        results[0]["current"],
        json!({ "content": current, "end_line": 4, "start_line": 1 })
    );
    assert_eq!(results[1]["current"]["start_line"], 1);
}
