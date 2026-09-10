use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Validates workspace migration numbering using the host filesystem adapter.
///
/// # Errors
/// Returns an error for unreadable directories or duplicate migration prefixes.
pub(crate) fn run() -> Result<(), String> {
    MigrationCheck {
        file_system: &RealFileSystem,
    }
    .run(Path::new("crates"))
}

/// Filesystem operations used by migration discovery and validation.
#[cfg_attr(test, mockall::automock)]
trait FileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn is_dir(&self, path: &Path) -> io::Result<bool>;
}

/// Migration validation policy, independent of the host filesystem.
struct MigrationCheck<'file_system> {
    file_system: &'file_system dyn FileSystem,
}

impl MigrationCheck<'_> {
    fn run(&self, root: &Path) -> Result<(), String> {
        for directory in self.find_migration_dirs(root)? {
            self.check_prefixes(&directory)?;
        }

        Ok(())
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
