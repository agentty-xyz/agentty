//! Public captured-diff parsing and source-evidence contract.

use ag_git::DiffFile;

#[test]
fn public_diff_snapshot_counts_suppressed_blank_context_on_both_sides() {
    // Arrange
    let snapshot = "diff --git a/source.rs b/source.rs\n--- a/source.rs\n+++ b/source.rs\n@@ \
                    -10,4 +20,4 @@\n context\n\n-old();\n+new();\n\n";

    // Act
    let files = DiffFile::parse(snapshot);

    // Assert
    assert_eq!(files[0].source_ranges("old();", true), [(12, 12)]);
    assert_eq!(files[0].source_ranges("new();", false), [(22, 22)]);
    assert_eq!(
        files[0].source_ranges("context\n\nnew();", false),
        [(20, 22)]
    );
}

#[test]
fn public_diff_snapshot_keeps_deleted_source_evidence() {
    // Arrange
    let snapshot = "diff --git a/config.rs b/config.rs\n--- a/config.rs\n+++ /dev/null\n@@ -8,2 \
                    +0,0 @@\n-check_access();\n-run();\n";

    // Act
    let files = DiffFile::parse(snapshot);
    let deleted_range = files[0].source_ranges("check_access();\nrun();", true);

    // Assert
    assert_eq!(deleted_range, [(8, 9)]);
    assert_eq!(
        files[0].source_ranges("check_access();", false),
        Vec::<(u32, u32)>::new()
    );
}

#[test]
fn public_diff_snapshot_retains_spaced_paths_without_source_hunks() {
    // Arrange
    let snapshot = "diff --git a/check script.sh b/check script.sh\nold mode 100644\nnew mode \
                    100755\ndiff --git a/image asset.png b/image asset.png\nBinary files a/image \
                    asset.png and b/image asset.png differ\n";

    // Act
    let files = DiffFile::parse(snapshot);

    // Assert
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].new_path, "check script.sh");
    assert_eq!(files[1].new_path, "image asset.png");
    assert_eq!(
        files.iter().map(|file| file.text).collect::<String>(),
        snapshot
    );
    assert_eq!(
        files[1].source_ranges("not source", false),
        Vec::<(u32, u32)>::new()
    );
}
