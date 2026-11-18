//! Migration discovery, naming, duplicate-prefix, and edit-history check unit
//! tests.

use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{fs, io};

use mockall::Sequence;
use mockall::predicate::eq;
use tempfile::tempdir;

use crate::check_migration::{Git, MigrationCheck, MockGit, RealGit, run};
use crate::file_system::MockFileSystem;

/// Fork point the history mocks report for `origin/main`.
const FORK: &str = "fork";

/// Returns the `git diff` arguments the history check sends for one side.
fn diff_arguments(is_staged: bool) -> Vec<String> {
    let mut arguments = vec![
        "diff",
        "--no-ext-diff",
        "--no-renames",
        "--name-status",
        "-z",
    ];
    if is_staged {
        arguments.push("--cached");
    }
    arguments.extend([FORK, "--", ":(top,glob)crates/*/migrations/*.sql"]);

    arguments.into_iter().map(ToString::to_string).collect()
}

/// Returns a Git mock whose fork point from `origin/main` is [`FORK`] and
/// whose working-tree and staged diffs return the given name-status output.
fn git_history(worktree: &str, staged: &str) -> MockGit {
    let mut git = MockGit::new();
    let mut sequence = Sequence::new();
    git.expect_output()
        .withf(|arguments| arguments == ["merge-base", "HEAD", "origin/main"])
        .once()
        .in_sequence(&mut sequence)
        .return_once(|_| Ok(format!("{FORK}\n")));
    for (is_staged, output) in [(false, worktree), (true, staged)] {
        let output = output.to_string();
        git.expect_output()
            .withf(move |arguments| arguments == diff_arguments(is_staged).as_slice())
            .once()
            .in_sequence(&mut sequence)
            .return_once(move |_| Ok(output));
    }

    git
}

fn directory_entries(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

fn prefix_file_system(names: &[&str]) -> MockFileSystem {
    let entries = directory_entries(names);
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_read_dir()
        .with(eq(Path::new("migration")))
        .once()
        .return_once(move |_| Ok(entries));

    file_system
}

#[test]
fn unique_prefixes_ignore_non_sql_files_and_accept_empty_directories() {
    // Arrange
    for names in [
        vec![],
        vec!["002_second.sql", "001_first.sql", "001_note.md"],
    ] {
        let file_system = prefix_file_system(&names);
        let git = MockGit::new();
        let check = MigrationCheck {
            file_system: &file_system,
            git: &git,
        };

        // Act
        let result = check.check_names(Path::new("migration"));

        // Assert
        assert_eq!(result, Ok(()));
    }
}

#[test]
fn duplicate_prefix_errors_are_sorted_and_include_file_names() {
    // Arrange
    let file_system = prefix_file_system(&["001_z.sql", "001_a.sql", "002_other.sql"]);
    let git = MockGit::new();
    let check = MigrationCheck {
        file_system: &file_system,
        git: &git,
    };

    // Act
    let result = check.check_names(Path::new("migration"));

    // Assert
    assert_eq!(
        result,
        Err("Duplicate migration prefix `001` in migration: 001_a.sql, 001_z.sql".to_string())
    );
}

#[test]
fn misnamed_migrations_are_reported_before_duplicate_prefixes() {
    // Arrange
    let file_system = prefix_file_system(&[
        "001_First.sql",
        "001_.sql",
        "abc_name.sql",
        "01_short.sql",
        "002.sql",
        "003_valid_name_2.sql",
    ]);
    let git = MockGit::new();
    let check = MigrationCheck {
        file_system: &file_system,
        git: &git,
    };

    // Act
    let result = check.check_names(Path::new("migration"));

    // Assert
    assert_eq!(
        result,
        Err(
            "Rename migrations in migration to `NNN_description.sql`, with a three-digit number \
             and a lowercase `snake_case` description: 001_.sql, 001_First.sql, 002.sql, \
             01_short.sql, abc_name.sql"
                .to_string()
        )
    );
}

#[test]
fn discovery_sorts_migration_directories_and_skips_missing_ones() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates")))
        .once()
        .in_sequence(&mut sequence)
        .return_once(|_| Ok(directory_entries(&["crates/z", "crates/empty", "crates/a"])));
    for (name, is_directory) in [("z", true), ("empty", false), ("a", true)] {
        file_system
            .expect_is_dir()
            .with(eq(PathBuf::from(format!("crates/{name}/migrations"))))
            .once()
            .in_sequence(&mut sequence)
            .return_once(move |_| Ok(is_directory));
    }
    for name in ["a", "z"] {
        file_system
            .expect_read_dir()
            .with(eq(PathBuf::from(format!("crates/{name}/migrations"))))
            .once()
            .in_sequence(&mut sequence)
            .return_once(|_| Ok(Vec::new()));
    }
    let git = git_history("A\0crates/a/migrations/003_new.sql\0", "");
    let check = MigrationCheck {
        file_system: &file_system,
        git: &git,
    };

    // Act
    let result = check.run(Path::new("crates"), "origin/main");

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn edited_existing_migrations_are_reported_once_with_their_change() {
    // Arrange
    let git = git_history(
        "M\0crates/a/migrations/001_first.sql\0A\0crates/a/migrations/003_new.sql\0",
        "D\0crates/a/migrations/002_second.sql\0T\0crates/a/migrations/004_link.sql\0M\0crates/a/\
         migrations/001_first.sql\0",
    );
    let file_system = MockFileSystem::new();
    let check = MigrationCheck {
        file_system: &file_system,
        git: &git,
    };

    // Act
    let result = check.check_history("origin/main");

    // Assert
    assert_eq!(
        result,
        Err(
            "Existing migrations must not change, because databases that already applied them \
             reject a changed checksum. Restore them and add a new numbered migration instead:\n  \
             crates/a/migrations/001_first.sql (modified)\n  crates/a/migrations/002_second.sql \
             (deleted or renamed)\n  crates/a/migrations/004_link.sql (changed)"
                .to_string()
        )
    );
}

#[test]
fn history_check_reports_a_missing_base_and_diff_failures() {
    // Arrange
    let mut missing_base = MockGit::new();
    missing_base
        .expect_output()
        .once()
        .return_once(|_| Err("git merge-base failed: Not a valid object name".to_string()));
    let mut failing_diff = MockGit::new();
    let mut sequence = Sequence::new();
    failing_diff
        .expect_output()
        .once()
        .in_sequence(&mut sequence)
        .return_once(|_| Ok(format!("{FORK}\n")));
    failing_diff
        .expect_output()
        .once()
        .in_sequence(&mut sequence)
        .return_once(|_| Err("git diff failed: bad revision".to_string()));
    let file_system = MockFileSystem::new();

    // Act
    let missing_result = MigrationCheck {
        file_system: &file_system,
        git: &missing_base,
    }
    .check_history("origin/main");
    let diff_result = MigrationCheck {
        file_system: &file_system,
        git: &failing_diff,
    }
    .check_history("origin/main");

    // Assert
    assert_eq!(
        missing_result,
        Err(
            "Cannot find the fork point from `origin/main` to check migration edits: git \
             merge-base failed: Not a valid object name\nFetch it with `git fetch origin main`, \
             or set AGENTTY_MIGRATION_BASE to the base revision."
                .to_string()
        )
    );
    assert_eq!(
        diff_result,
        Err("git diff failed: bad revision".to_string())
    );
}

#[test]
fn name_status_parsing_ignores_additions_and_an_incomplete_entry() {
    // Act
    let paths = MigrationCheck::edited_paths("A\0added.sql\0M\0edited.sql\0D");

    // Assert
    assert_eq!(paths, vec!["  edited.sql (modified)".to_string()]);
}

#[test]
fn discovery_and_validation_report_read_failures() {
    // Arrange
    for is_root in [true, false] {
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_read_dir()
            .once()
            .return_once(|_| Err(io::Error::other("read failed")));
        let git = MockGit::new();
        let check = MigrationCheck {
            file_system: &file_system,
            git: &git,
        };

        // Act
        let result = if is_root {
            check.run(Path::new("crates"), "origin/main")
        } else {
            check.check_names(Path::new("crates"))
        };

        // Assert
        assert_eq!(
            result,
            Err("Failed to read crates: read failed".to_string())
        );
    }
}

#[test]
fn discovery_reports_metadata_failures() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_read_dir()
        .once()
        .return_once(|_| Ok(directory_entries(&["crates/a"])));
    file_system
        .expect_is_dir()
        .once()
        .return_once(|_| Err(io::Error::other("metadata failed")));
    let git = MockGit::new();
    let check = MigrationCheck {
        file_system: &file_system,
        git: &git,
    };

    // Act
    let result = check.run(Path::new("crates"), "origin/main");

    // Assert
    assert_eq!(
        result,
        Err("Failed to inspect crates/a/migrations: metadata failed".to_string())
    );
}

#[test]
fn workflow_propagates_duplicate_prefixes() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates")))
        .once()
        .return_once(|_| Ok(directory_entries(&["crates/a"])));
    file_system.expect_is_dir().once().return_once(|_| Ok(true));
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates/a/migrations")))
        .once()
        .return_once(|_| Ok(directory_entries(&["001_first.sql", "001_second.sql"])));
    let git = MockGit::new();
    let check = MigrationCheck {
        file_system: &file_system,
        git: &git,
    };

    // Act
    let result = check.run(Path::new("crates"), "origin/main");

    // Assert
    assert!(
        result
            .expect_err("duplicate migration")
            .contains("001_first.sql, 001_second.sql")
    );
}

#[test]
fn real_git_returns_output_and_reports_failures() {
    // Arrange
    let git = RealGit {
        git: OsString::from("git"),
    };
    let missing = RealGit {
        git: OsString::from("ag-xtask-missing-git"),
    };
    let arguments = |values: &[&str]| values.iter().map(ToString::to_string).collect::<Vec<_>>();

    // Act
    let inside = git.output(&arguments(&["rev-parse", "--is-inside-work-tree"]));
    let unknown = git.output(&arguments(&[
        "rev-parse",
        "--verify",
        "--quiet",
        "no/such/ref",
    ]));
    let unavailable = missing.output(&arguments(&["status"]));

    // Assert
    assert_eq!(inside, Ok("true\n".to_string()));
    assert_eq!(unknown, Err("git rev-parse failed: ".to_string()));
    assert!(
        unavailable
            .expect_err("missing git")
            .starts_with("Failed to run git:")
    );
}

#[cfg(unix)]
#[test]
fn real_git_rejects_failed_runs_without_arguments_and_non_utf8_output() {
    // Arrange
    let directory = tempdir().expect("Git stub directory");
    let stub = directory.path().join("git");
    for (script, expected) in [
        ("#!/bin/sh\nprintf '\\377'\n", "Invalid UTF-8 from git:"),
        (
            "#!/bin/sh\necho 'no repository' >&2\nexit 128\n",
            "git  failed: no repository",
        ),
    ] {
        fs::write(&stub, script).expect("Git stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("executable stub");
        let git = RealGit {
            git: stub.clone().into_os_string(),
        };

        // Act
        let result = git.output(&[]);

        // Assert
        assert!(
            result
                .expect_err("invalid Git output")
                .starts_with(expected)
        );
    }
}

#[test]
fn production_composition_reports_a_missing_workspace_root() {
    // Arrange
    // Cargo runs unit tests in the package directory, outside the workspace
    // root.
    assert!(!Path::new("crates").exists());

    // Act
    let result = run();

    // Assert
    assert!(
        result
            .expect_err("missing workspace root")
            .starts_with("Failed to read crates:")
    );
}
