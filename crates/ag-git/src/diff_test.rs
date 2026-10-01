use crate::diff::{DiffFile, decode_path, hunk_starts};

#[test]
fn captures_file_boundaries_renames_spaces_and_deletions() {
    // Arrange
    let input = "diff --git a/old name.rs b/new name.rs\n--- a/old name.rs\n+++ b/new name.rs\n@@ \
                 -4,2 +9,2 @@\n old\n-gone\n+new\ndiff --git a/deleted b/deleted\n--- \
                 a/deleted\n+++ /dev/null\n@@ -1 +0,0 @@\n-removed\n";

    // Act
    let files = DiffFile::parse(input);

    // Assert
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].old_path, "old name.rs");
    assert_eq!(files[0].new_path, "new name.rs");
    assert_eq!(files[1].new_path, "deleted");
    assert_eq!(
        files.iter().map(|file| file.text).collect::<String>(),
        input
    );
    assert!(DiffFile::parse("non-diff evidence").is_empty());
}

#[test]
fn handles_quoted_and_metadata_only_paths_without_guessing() {
    // Arrange
    let headers = [
        (
            "diff --git a/old b/new\nrename from old\nrename to new\n",
            "old",
            "new",
        ),
        (
            "diff --git a/old name b/new name\nrename from old name\nrename to new name\n",
            "old name",
            "new name",
        ),
        (
            "diff --git a/old name b/new name\ncopy from old name\ncopy to \"new name\"\n",
            "old name",
            "new name",
        ),
        (
            "diff --git a/new b/new\n--- /dev/null\n+++ b/new\n",
            "new",
            "new",
        ),
        (
            "diff --git a/x b/x\n--- a/x\tdate\n+++ b/x\tdate\n",
            "x",
            "x",
        ),
        (
            "diff --git \"a/\\303\\251.rs\" \"b/\\303\\251.rs\"\n--- \"a/\\303\\251.rs\"\n+++ \
             \"b/\\303\\251.rs\"\n",
            "é.rs",
            "é.rs",
        ),
        ("diff --git broken\n", "", ""),
        ("diff --git a/x b/x unexpected\n", "", ""),
        ("diff --git x b/x\n", "", ""),
        ("diff --git a/x x\n", "", ""),
    ];

    // Act / Assert
    for (input, old, new) in headers {
        let files = DiffFile::parse(input);
        assert_eq!(files[0].old_path, old, "{input}");
        assert_eq!(files[0].new_path, new, "{input}");
    }
}

#[test]
fn parses_unquoted_space_paths_in_mode_only_and_binary_changes() {
    // Arrange
    let inputs = [
        (
            "scripts/check permissions.sh",
            "old mode 100644\nnew mode 100755\n",
        ),
        (
            "assets/new image.png",
            "Binary files /dev/null and b/assets/new image.png differ\n",
        ),
        (
            "assets/old image.png",
            "Binary files a/assets/old image.png and /dev/null differ\n",
        ),
        (
            "assets/a b/image.png",
            "GIT binary patch\nliteral 0\nHcmV?d00001\n",
        ),
        ("é space.rs", "old mode 100644\nnew mode 100755\n"),
    ];

    // Act / Assert
    for (path, metadata) in inputs {
        let input = format!("diff --git a/{path} b/{path}\n{metadata}");
        let files = DiffFile::parse(&input);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].old_path, path);
        assert_eq!(files[0].new_path, path);
        assert_eq!(files[0].text, input);
    }
}

#[test]
fn anchors_changed_source_on_both_sides_without_crossing_hunk_gaps() {
    // Arrange
    let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,3 +20,3 @@\n unchanged\n- old\n+ \
                new\n tail\n\\ No newline at end of file\n@@ -30 +40 @@\n-old\n+new\n";
    let files = DiffFile::parse(diff);

    // Act / Assert
    assert_eq!(
        files[0].source_ranges("unchanged\nnew\ntail", false),
        [(20, 22)]
    );
    assert_eq!(files[0].source_ranges("old", true), [(11, 11), (30, 30)]);
    assert_eq!(files[0].source_ranges("new", false), [(21, 21), (40, 40)]);
    for snippet in ["", " \n ", "tail\nnew", "unchanged", "absent"] {
        assert_eq!(
            files[0].source_ranges(snippet, false),
            Vec::<(u32, u32)>::new()
        );
    }
    let malformed = DiffFile::parse("diff --git a/x b/x\n@@ malformed\n+new\n");
    assert_eq!(
        malformed[0].source_ranges("new", false),
        Vec::<(u32, u32)>::new()
    );
    let overflow = DiffFile::parse("diff --git a/x b/x\n@@ -0 +4294967295 @@\n+x\n+y\n");
    assert_eq!(
        overflow[0].source_ranges("x", false),
        [(u32::MAX, u32::MAX)]
    );
}

#[test]
fn rejects_invalid_hunk_ranges_and_git_escapes() {
    // Arrange
    let invalid_headers = [
        "",
        "@@ -1 +bad @@",
        "@@ -bad +1 @@",
        "@@ -1 @@",
        "@@ -1 +1",
        "@@@ -1 +1 @@@",
    ];
    let invalid_paths = [
        "",
        "\"unterminated",
        "\"trailing\\",
        "\"\\z\"",
        "\"\\777\"",
        "\"\\377\"",
    ];

    // Act / Assert
    assert_eq!(hunk_starts("@@ -1,0 +2,3 @@ function"), Some((1, 2)));
    for header in invalid_headers {
        assert_eq!(hunk_starts(header), None);
    }
    for path in invalid_paths {
        assert_eq!(decode_path(path), None, "{path}");
    }
    assert_eq!(
        decode_path("a/file rest"),
        Some(("a/file".to_string(), " rest"))
    );
    assert_eq!(
        decode_path("\"\\a\\b\\t\\n\\v\\f\\r\\\\\\\"\\1\\12\\141\""),
        Some(("\x07\x08\t\n\x0b\x0c\r\\\"\x01\na".to_string(), ""))
    );
}
