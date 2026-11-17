//! Check that SQL migration files in each migration directory have unique
//! prefixes and that migrations existing at the branch fork point stay
//! unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Revision whose fork point bounds migration edits by default.
const DEFAULT_BASE: &str = "origin/main";

/// Environment variable that names another base revision, such as `HEAD^1`
/// in a shallow CI checkout of a pull-request merge commit.
const BASE_ENVIRONMENT: &str = "AGENTTY_MIGRATION_BASE";

/// Root-anchored pathspec selecting every crate migration.
const MIGRATION_PATHSPEC: &str = ":(top,glob)crates/*/migrations/*.sql";

/// Validates workspace migration numbering and history using the host
/// filesystem and Git adapters.
///
/// # Errors
/// Returns an error for unreadable directories, duplicate migration prefixes,
/// an unresolvable base revision, or an edited existing migration.
pub(crate) fn run() -> Result<(), String> {
    let base = std::env::var(BASE_ENVIRONMENT).unwrap_or_else(|_| DEFAULT_BASE.to_string());

    MigrationCheck {
        file_system: &RealFileSystem,
        git: &RealGit {
            git: OsString::from("git"),
        },
    }
    .run(Path::new("crates"), &base)
}

/// Filesystem operations used by migration discovery and validation.
#[cfg_attr(test, mockall::automock)]
trait FileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn is_dir(&self, path: &Path) -> io::Result<bool>;
}

/// Read-only Git queries used to find edited migrations.
#[cfg_attr(test, mockall::automock)]
trait Git {
    /// Runs one Git command and returns its standard output.
    fn output(&self, arguments: &[String]) -> Result<String, String>;
}

/// Migration validation policy, independent of the host filesystem and Git.
struct MigrationCheck<'host> {
    file_system: &'host dyn FileSystem,
    git: &'host dyn Git,
}

impl MigrationCheck<'_> {
    fn run(&self, root: &Path, base: &str) -> Result<(), String> {
        for directory in self.find_migration_dirs(root)? {
            self.check_prefixes(&directory)?;
        }

        self.check_history(base)
    }

    /// Rejects migrations that exist at the fork point from `base` and are
    /// modified, deleted, or renamed in the index or working tree.
    fn check_history(&self, base: &str) -> Result<(), String> {
        let fork = self
            .git_output(&["merge-base", "HEAD", base])
            .map_err(|error| {
                format!(
                    "Cannot find the fork point from `{base}` to check migration edits: \
                     {error}\nFetch it with `git fetch origin main`, or set {BASE_ENVIRONMENT} to \
                     the base revision."
                )
            })?;
        let fork = fork.trim();
        let mut edited = BTreeSet::new();
        for staged in [false, true] {
            let mut arguments = vec![
                "diff",
                "--no-ext-diff",
                "--no-renames",
                "--name-status",
                "-z",
            ];
            if staged {
                arguments.push("--cached");
            }
            arguments.extend([fork, "--", MIGRATION_PATHSPEC]);
            edited.extend(Self::edited_paths(&self.git_output(&arguments)?));
        }

        if edited.is_empty() {
            return Ok(());
        }

        Err(format!(
            "Existing migrations must not change, because databases that already applied them \
             reject a changed checksum. Restore them and add a new numbered migration instead:\n{}",
            edited.into_iter().collect::<Vec<_>>().join("\n")
        ))
    }

    fn git_output(&self, arguments: &[&str]) -> Result<String, String> {
        let arguments = arguments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        self.git.output(&arguments)
    }

    /// Returns each non-added path in NUL-separated `--name-status` output,
    /// labeled with how it changed.
    fn edited_paths(name_status: &str) -> Vec<String> {
        let fields = name_status
            .split('\0')
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();

        fields
            .chunks(2)
            .filter_map(|entry| match entry {
                [status, path] if !status.starts_with('A') => {
                    let change = match status.chars().next() {
                        Some('D') => "deleted or renamed",
                        Some('M') => "modified",
                        _ => "changed",
                    };

                    Some(format!("  {path} ({change})"))
                }
                _ => None,
            })
            .collect()
    }

    fn find_migration_dirs(&self, root: &Path) -> Result<Vec<PathBuf>, String> {
        let entries = self.read_dir(root)?;
        let mut directories = Vec::new();
        for entry in entries {
            let migrations_path = entry.join("migrations");
            if self.file_system.is_dir(&migrations_path).map_err(|error| {
                format!("Failed to inspect {}: {error}", migrations_path.display())
            })? {
                directories.push(migrations_path);
            }
        }
        directories.sort();

        Ok(directories)
    }

    fn read_dir(&self, directory: &Path) -> Result<Vec<PathBuf>, String> {
        self.file_system
            .read_dir(directory)
            .map_err(|error| format!("Failed to read {}: {error}", directory.display()))
    }

    fn check_prefixes(&self, directory: &Path) -> Result<(), String> {
        let mut prefix_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in self.read_dir(directory)? {
            if path.extension().is_some_and(|extension| extension == "sql") {
                let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                let prefix = file_name.split('_').next().unwrap_or_default();
                prefix_map
                    .entry(prefix.to_string())
                    .or_default()
                    .push(file_name.into_owned());
            }
        }

        for (prefix, files) in &mut prefix_map {
            files.sort();
            if files.len() > 1 {
                return Err(format!(
                    "Duplicate migration prefix `{prefix}` in {}: {}",
                    directory.display(),
                    files.join(", ")
                ));
            }
        }

        Ok(())
    }
}

/// Host Git adapter; migration decisions remain in `MigrationCheck`.
struct RealGit {
    git: OsString,
}

impl Git for RealGit {
    fn output(&self, arguments: &[String]) -> Result<String, String> {
        let output = Command::new(&self.git)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(arguments)
            .output()
            .map_err(|error| format!("Failed to run git: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                arguments.first().map_or("", String::as_str),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        String::from_utf8(output.stdout).map_err(|error| format!("Invalid UTF-8 from git: {error}"))
    }
}

/// Host adapter; workflow decisions remain in `MigrationCheck`.
struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "check_migration_test.rs"]
mod tests;
