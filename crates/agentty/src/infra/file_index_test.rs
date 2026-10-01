use std::path::Path;
use std::process::Command;
use std::{fs, io};

use tempfile::TempDir;

use super::{MAX_DEPTH, list_files, list_files_for_explorer, sort_and_limit_entries};
use crate::domain::file_entry::FileEntry;

const TEST_MAX_ENTRIES: usize = 500;

/// Git boundary used by file-index tests that need repository
/// initialization.
#[cfg_attr(test, mockall::automock)]
trait GitFileIndexClient: Send + Sync {
    /// Initializes a git repository in `repo_root`.
    ///
    /// # Errors
    /// Returns an error when `git init` cannot be executed or fails.
    fn init_repository(&self, repo_root: &Path) -> io::Result<()>;
}

/// [`GitFileIndexClient`] implementation backed by real git subprocesses.
struct RealGitFileIndexClient;

impl RealGitFileIndexClient {
    /// Builds an `io::Error` for failed `git init` command execution.
    fn git_init_failed_error(output: &std::process::Output) -> io::Error {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return io::Error::other("`git init -q` failed");
        }

        io::Error::other(format!("`git init -q` failed: {stderr}"))
    }
}

impl GitFileIndexClient for RealGitFileIndexClient {
    fn init_repository(&self, repo_root: &Path) -> io::Result<()> {
        let output = Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo_root)
            .output()?;
        if !output.status.success() {
            return Err(Self::git_init_failed_error(&output));
        }

        Ok(())
    }
}

/// Initializes one temporary git repository for `.gitignore` tests.
fn initialize_test_repository(repo_root: &Path) {
    let git_file_index_client = RealGitFileIndexClient;

    git_file_index_client
        .init_repository(repo_root)
        .expect("test expectation should hold");
}

#[test]
fn test_list_files_empty_directory() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    assert_eq!(entries, [] as [crate::domain::file_entry::FileEntry; 0]);
}

#[test]
fn test_list_files_returns_sorted_entries() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    fs::write(temp_dir.path().join("banana.txt"), "").expect("test expectation should hold");
    fs::write(temp_dir.path().join("apple.txt"), "").expect("test expectation should hold");
    fs::write(temp_dir.path().join("cherry.txt"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(paths, vec!["apple.txt", "banana.txt", "cherry.txt"]);
}

#[test]
fn test_list_files_returns_relative_paths() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    fs::create_dir_all(temp_dir.path().join("src")).expect("test expectation should hold");
    fs::write(temp_dir.path().join("src/main.rs"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    let file_entries: Vec<_> = entries.iter().filter(|entry| !entry.is_dir).collect();
    assert_eq!(file_entries.len(), 1);
    assert_eq!(file_entries[0].path, "src/main.rs");
}

#[test]
fn test_list_files_respects_gitignore() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    initialize_test_repository(temp_dir.path());
    fs::write(temp_dir.path().join(".gitignore"), "ignored.txt\n")
        .expect("test expectation should hold");
    fs::write(temp_dir.path().join("kept.txt"), "").expect("test expectation should hold");
    fs::write(temp_dir.path().join("ignored.txt"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert!(paths.contains(&"kept.txt"));
    assert!(!paths.contains(&"ignored.txt"));
}

#[test]
fn test_list_files_includes_non_ignored_dotfiles() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    initialize_test_repository(temp_dir.path());
    fs::write(temp_dir.path().join(".gitignore"), ".ignored-dotfile\n")
        .expect("test expectation should hold");
    fs::write(temp_dir.path().join(".visible-dotfile"), "").expect("test expectation should hold");
    fs::write(temp_dir.path().join(".ignored-dotfile"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert!(paths.contains(&".visible-dotfile"));
    assert!(!paths.contains(&".ignored-dotfile"));
}

#[test]
fn test_list_files_includes_directories() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    fs::create_dir_all(temp_dir.path().join("subdir")).expect("test expectation should hold");
    fs::write(temp_dir.path().join("file.txt"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, "subdir");
    assert!(entries[0].is_dir);
    assert_eq!(entries[1].path, "file.txt");
    assert!(!entries[1].is_dir);
}

#[test]
fn test_list_files_excludes_root_directory() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    fs::write(temp_dir.path().join("file.txt"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    assert!(!entries.iter().any(|entry| entry.path.is_empty()));
}

#[test]
fn test_list_files_sorts_directories_before_files() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    fs::write(temp_dir.path().join("aaa_file.txt"), "").expect("test expectation should hold");
    fs::create_dir_all(temp_dir.path().join("zzz_dir")).expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert — directory sorts before file despite alphabetical order
    assert_eq!(entries[0].path, "zzz_dir");
    assert!(entries[0].is_dir);
    assert_eq!(entries[1].path, "aaa_file.txt");
    assert!(!entries[1].is_dir);
}

#[test]
fn test_list_files_is_unbounded_within_max_depth() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    for index in 0..TEST_MAX_ENTRIES + 50 {
        fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
            .expect("test expectation should hold");
    }

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    assert_eq!(entries.len(), TEST_MAX_ENTRIES + 50);
}

#[test]
fn test_list_files_keeps_files_when_many_directories_exist() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    for index in 0..TEST_MAX_ENTRIES + 50 {
        fs::create_dir_all(temp_dir.path().join(format!("dir_{index:04}")))
            .expect("test expectation should hold");
    }
    fs::write(temp_dir.path().join("z_last_file.rs"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    assert!(entries.iter().any(|entry| entry.path == "z_last_file.rs"));
    assert!(entries.len() > TEST_MAX_ENTRIES);
}

#[test]
fn test_list_files_for_explorer_can_be_unbounded() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    for index in 0..TEST_MAX_ENTRIES + 50 {
        fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
            .expect("test expectation should hold");
    }

    // Act
    let entries = list_files_for_explorer(temp_dir.path(), None, None);

    // Assert
    assert_eq!(entries.len(), TEST_MAX_ENTRIES + 50);
}

#[test]
fn test_list_files_for_explorer_respects_custom_limits() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    for index in 0..20 {
        fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
            .expect("test expectation should hold");
    }

    // Act
    let entries = list_files_for_explorer(temp_dir.path(), Some(3), Some(7));

    // Assert
    assert_eq!(entries.len(), 7);
}

#[test]
fn test_sort_and_limit_entries_truncates_after_sort() {
    // Arrange
    let mut entries: Vec<FileEntry> = (0..TEST_MAX_ENTRIES + 20)
        .rev()
        .map(|index| FileEntry {
            is_dir: false,
            path: format!("file_{index:04}.txt"),
        })
        .collect();

    // Act
    sort_and_limit_entries(&mut entries, Some(TEST_MAX_ENTRIES));

    // Assert
    assert_eq!(entries.len(), TEST_MAX_ENTRIES);
    assert_eq!(
        entries.first().map(|entry| entry.path.as_str()),
        Some("file_0000.txt")
    );
    assert_eq!(
        entries.last().map(|entry| entry.path.as_str()),
        Some("file_0499.txt")
    );
}

#[test]
fn test_list_files_respects_max_depth() {
    // Arrange
    let temp_dir = TempDir::new().expect("test expectation should hold");
    let mut deep_path = temp_dir.path().to_path_buf();
    for level in 0..MAX_DEPTH + 2 {
        deep_path = deep_path.join(format!("d{level}"));
    }
    fs::create_dir_all(&deep_path).expect("test expectation should hold");
    fs::write(deep_path.join("deep.txt"), "").expect("test expectation should hold");
    fs::write(temp_dir.path().join("shallow.txt"), "").expect("test expectation should hold");

    // Act
    let entries = list_files(temp_dir.path());

    // Assert
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert!(paths.contains(&"shallow.txt"));
    assert!(!paths.iter().any(|path| path.contains("deep.txt")));
}
