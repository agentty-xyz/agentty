//! Changed-file formatting scope unit tests.

use std::path::{Path, PathBuf};

use super::Selection;

fn paths(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

#[test]
fn existing_changed_rust_files_are_formatted_and_other_paths_are_skipped() {
    // Arrange
    let changes = paths(&[
        "crates/core/src/lib.rs",
        "crates/core/src/deleted.rs",
        "docs/guide.md",
        "crates/core/Cargo.toml",
        "crates/core/src/new_test.rs",
    ]);
    // Act
    let selection = Selection::from_changes(&changes, |path| {
        path != Path::new("crates/core/src/deleted.rs")
    });
    // Assert
    assert_eq!(
        selection,
        Selection::Files(paths(&[
            "crates/core/src/lib.rs",
            "crates/core/src/new_test.rs"
        ]))
    );
    assert_eq!(
        selection.command(),
        Some((
            "rustfmt",
            vec![
                "--config".to_string(),
                "skip_children=true".to_string(),
                "crates/core/src/lib.rs".to_string(),
                "crates/core/src/new_test.rs".to_string()
            ]
        ))
    );
}

#[test]
fn changes_without_existing_rust_files_need_no_formatter() {
    // Arrange
    let changes = paths(&["README.md", "crates/core/src/deleted.rs"]);
    // Act
    let selection = Selection::from_changes(&changes, |path| path.extension().is_none());
    // Assert
    assert_eq!(selection, Selection::Nothing);
    assert_eq!(selection.command(), None);
}

#[test]
fn formatter_configuration_or_toolchain_changes_format_the_workspace() {
    // Arrange
    let triggers = [
        "rustfmt.toml",
        ".rustfmt.toml",
        "crates/core/rustfmt.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
    ];
    for trigger in triggers {
        let changes = paths(&["crates/core/src/lib.rs", trigger]);
        // Act
        let selection = Selection::from_changes(&changes, |_| true);
        // Assert
        assert_eq!(selection, Selection::Workspace, "{trigger}");
        assert_eq!(
            selection.command(),
            Some(("cargo", vec!["fmt".to_string(), "--all".to_string()]))
        );
    }
}
