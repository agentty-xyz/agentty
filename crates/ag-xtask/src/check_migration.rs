use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::error;

pub(crate) fn run() -> Result<(), String> {
    let migration_dirs = find_migration_dirs(Path::new("crates"));

    for dir in migration_dirs {
        check_prefixes(&dir)?;
    }

    Ok(())
}

fn find_migration_dirs(base: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(err) => {
            error!("Failed to read {}: {}", base.display(), err);

            return Vec::new();
        }
    };

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let migrations_path = entry.path().join("migrations");
        if migrations_path.is_dir() {
            dirs.push(migrations_path);
        }
    }
    dirs.sort();

    dirs
}

fn check_prefixes(dir: &Path) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|err| format!("Failed to read {}: {err}", dir.display()))?;

    let mut prefix_map: HashMap<String, Vec<String>> = HashMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sql") {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if let Some(prefix) = file_name.split('_').next() {
                prefix_map
                    .entry(prefix.to_string())
                    .or_default()
                    .push(file_name);
            }
        }
    }

    for (prefix, files) in &mut prefix_map {
        files.sort();
        if files.len() > 1 {
            return Err(format!(
                "Duplicate migration prefix `{prefix}` in {}: {}",
                dir.display(),
                files.join(", ")
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_check_prefixes_no_duplicates() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        fs::write(dir.path().join("001_create_users.sql"), "").expect("write");
        fs::write(dir.path().join("002_create_orders.sql"), "").expect("write");

        // Act
        let result = check_prefixes(dir.path());

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_prefixes_with_duplicates() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        fs::write(dir.path().join("001_create_users.sql"), "").expect("write");
        fs::write(dir.path().join("001_create_orders.sql"), "").expect("write");

        // Act
        let result = check_prefixes(dir.path());

        // Assert
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Duplicate migration prefix `001`"), "{err}");
        assert!(err.contains("001_create_orders.sql"), "{err}");
        assert!(err.contains("001_create_users.sql"), "{err}");
    }

    #[test]
    fn test_check_prefixes_ignores_non_sql() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        fs::write(dir.path().join("001_create_users.sql"), "").expect("write");
        fs::write(dir.path().join("001_readme.md"), "").expect("write");

        // Act
        let result = check_prefixes(dir.path());

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_prefixes_empty_dir() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");

        // Act
        let result = check_prefixes(dir.path());

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_find_migration_dirs() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let crate_dir = dir.path().join("my-crate");
        fs::create_dir_all(crate_dir.join("migrations")).expect("mkdir");

        // Act
        let dirs = find_migration_dirs(dir.path());

        // Assert
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("migrations"));
    }

    #[test]
    fn test_find_migration_dirs_no_migrations() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        fs::create_dir_all(dir.path().join("my-crate/src")).expect("mkdir");

        // Act
        let dirs = find_migration_dirs(dir.path());

        // Assert
        assert!(dirs.is_empty());
    }
}
