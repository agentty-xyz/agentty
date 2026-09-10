use std::path::{Path, PathBuf};
use std::{fs, io};

use mockall::Sequence;
use mockall::predicate::eq;
use tempfile::tempdir;

use crate::check_migration::{FileSystem, MigrationCheck, MockFileSystem, RealFileSystem, run};

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
        let check = MigrationCheck {
            file_system: &file_system,
        };

        // Act
        let result = check.check_prefixes(Path::new("migration"));

        // Assert
        assert_eq!(result, Ok(()));
    }
}

#[test]
fn duplicate_prefix_errors_are_sorted_and_include_file_names() {
    // Arrange
    let file_system = prefix_file_system(&["001_z.sql", "001_a.sql", "002_other.sql"]);
    let check = MigrationCheck {
        file_system: &file_system,
    };

    // Act
    let result = check.check_prefixes(Path::new("migration"));

    // Assert
    assert_eq!(
        result,
        Err("Duplicate migration prefix `001` in migration: 001_a.sql, 001_z.sql".to_string())
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
    let check = MigrationCheck {
        file_system: &file_system,
    };

    // Act
    let result = check.run(Path::new("crates"));

    // Assert
    assert_eq!(result, Ok(()));
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
        let check = MigrationCheck {
            file_system: &file_system,
        };

        // Act
        let result = if is_root {
            check.run(Path::new("crates"))
        } else {
            check.check_prefixes(Path::new("crates"))
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
    let check = MigrationCheck {
        file_system: &file_system,
    };

    // Act
    let result = check.run(Path::new("crates"));

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
    let check = MigrationCheck {
        file_system: &file_system,
    };

    // Act
    let result = check.run(Path::new("crates"));

    // Assert
    assert!(
        result
            .expect_err("duplicate migration")
            .contains("001_first.sql, 001_second.sql")
    );
}

#[test]
fn real_file_system_lists_entries_and_distinguishes_directories() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let file = directory.path().join("001_example.sql");
    fs::write(&file, "").expect("migration file");
    let file_system = RealFileSystem;

    // Act
    let entries = file_system
        .read_dir(directory.path())
        .expect("directory entries");
    let directory_exists = file_system
        .is_dir(directory.path())
        .expect("directory metadata");
    let file_is_directory = file_system.is_dir(&file).expect("file metadata");
    let missing_is_directory = file_system
        .is_dir(&directory.path().join("missing"))
        .expect("missing metadata");
    let missing_entries = file_system.read_dir(&directory.path().join("missing"));
    let file_child_is_directory = file_system
        .is_dir(&file.join("child"))
        .expect("file child metadata");
    let invalid_metadata = file_system.is_dir(&directory.path().join("x".repeat(300)));

    // Assert
    assert_eq!(entries, vec![file]);
    assert!(directory_exists);
    assert!(!file_is_directory);
    assert!(!missing_is_directory);
    assert!(!file_child_is_directory);
    assert!(missing_entries.is_err());
    assert!(invalid_metadata.is_err());
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
