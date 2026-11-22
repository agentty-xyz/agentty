//! Git inventory and hook catalog unit tests.

use std::collections::BTreeSet;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::repository::{git_files, hook_ids};

#[test]
fn real_git_inventory_is_read_only_and_reports_execution_failures() {
    // Arrange
    let directory = tempdir().expect("Git fixture directory");

    // Act
    let listing = git_files(Path::new("git"), Path::new(env!("CARGO_MANIFEST_DIR")));
    let not_executable = git_files(&directory.path().join("missing-git"), directory.path());

    // Assert
    assert!(
        listing
            .expect("real Git inventory")
            .contains(&PathBuf::from("src/main.rs"))
    );
    assert!(
        not_executable
            .expect_err("missing Git")
            .starts_with("Failed to run git ls-files:")
    );
}

#[cfg(unix)]
#[test]
fn failed_and_non_utf8_git_inventory_cannot_pass_as_empty_results() {
    // Arrange
    let directory = tempdir().expect("Git stub directory");
    let git = directory.path().join("git");
    for (script, message) in [
        (
            "#!/bin/sh\nprintf '\\377\\000'\n",
            "Invalid UTF-8 in git ls-files:",
        ),
        (
            "#!/bin/sh\necho 'inventory failed' >&2\nexit 7\n",
            "git ls-files failed: inventory failed",
        ),
    ] {
        fs::write(&git, script).expect("Git stub");
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("executable stub");

        // Act
        let result = git_files(&git, directory.path());

        // Assert
        assert!(result.expect_err("invalid inventory").starts_with(message));
    }
}

#[test]
fn hook_ids_span_local_and_remote_repositories() {
    // Arrange
    let catalog =
        "repos:\n- repo: local\n  hooks:\n  - id: check\n- repo: https://example.test/hooks\n  \
         hooks:\n  - id: remote-check\n";

    // Act
    let ids = hook_ids(catalog);

    // Assert
    assert_eq!(
        ids,
        Ok(BTreeSet::from([
            "check".to_owned(),
            "remote-check".to_owned()
        ]))
    );
}

#[test]
fn malformed_or_incomplete_catalog_fails() {
    // Arrange
    for content in [
        "[unterminated",
        "{}",
        "null",
        "repos: [{}]",
        "repos: [{hooks: [{}]}]",
    ] {
        // Act
        let result = hook_ids(content);

        // Assert
        assert!(
            result
                .expect_err("invalid catalog")
                .starts_with("Invalid hook catalog:"),
            "{content}"
        );
    }
}
