//! Agentty data-directory path resolution.

use std::env;
use std::path::PathBuf;

/// Returns the resolved `agentty` home directory.
///
/// The `AGENTTY_ROOT` environment variable takes precedence when set to a
/// non-empty path. Otherwise the resolver falls back to `~/.agentty`, then to
/// a relative `.agentty` directory when no home directory is available.
pub fn agentty_home() -> PathBuf {
    let agentty_root = env::var_os("AGENTTY_ROOT").map(PathBuf::from);
    let home_dir = env::home_dir();

    resolve_agentty_home(agentty_root, home_dir)
}

/// Resolves the Agentty home directory from optional root and home paths.
///
/// When `agentty_root` is present and non-empty, it takes precedence. When no
/// override is available, the resolver falls back to `home_dir/.agentty`, then
/// finally to a relative `.agentty` directory.
fn resolve_agentty_home(agentty_root: Option<PathBuf>, home_dir: Option<PathBuf>) -> PathBuf {
    agentty_root
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home_dir.map(|path| path.join(".agentty")))
        .unwrap_or_else(|| PathBuf::from(".agentty"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_agentty_home_returns_env_override_when_set() {
        // Arrange
        let agentty_root = Some(PathBuf::from("/tmp/custom-agentty"));
        let home_dir = Some(PathBuf::from("/home/test-user"));

        // Act
        let home = resolve_agentty_home(agentty_root, home_dir);

        // Assert
        assert_eq!(home, PathBuf::from("/tmp/custom-agentty"));
    }

    #[test]
    fn resolve_agentty_home_falls_back_to_home_directory_when_override_is_empty() {
        // Arrange
        let agentty_root = Some(PathBuf::new());
        let home_dir = Some(PathBuf::from("/home/test-user"));

        // Act
        let home = resolve_agentty_home(agentty_root, home_dir);

        // Assert
        assert_eq!(home, PathBuf::from("/home/test-user/.agentty"));
    }

    #[test]
    fn resolve_agentty_home_falls_back_to_relative_directory_without_home_dir() {
        // Arrange
        let agentty_root = None;
        let home_dir = None;

        // Act
        let home = resolve_agentty_home(agentty_root, home_dir);

        // Assert
        assert_eq!(home, PathBuf::from(".agentty"));
    }
}
