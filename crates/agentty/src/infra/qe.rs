//! Quality-enforcement probe boundary used by the QE check engine.
//!
//! `/qe:check` audits the active session worktree against agentty's
//! hardcoded standard for agentic-development readiness. The check engine
//! lives in pure domain code and depends on the [`QeProbe`] trait so checks
//! stay deterministic in tests via `MockQeProbe`.

use std::fs;
use std::path::PathBuf;

/// Read-only filesystem boundary used by `domain::qe` checks.
///
/// Production uses [`RealQeProbe`], while tests inject `MockQeProbe` (only
/// generated under `#[cfg(test)]`) to exercise recommendation logic without
/// touching the real filesystem.
#[cfg_attr(test, mockall::automock)]
pub trait QeProbe: Send + Sync {
    /// Returns whether `path` resolves to an existing regular file.
    fn is_file(&self, path: PathBuf) -> bool;

    /// Reads `path` as UTF-8 text, returning `None` on any I/O or decode
    /// failure. The check engine treats `None` as "content unavailable" and
    /// fails the recommendation that depends on it.
    fn read_to_string(&self, path: PathBuf) -> Option<String>;

    /// Returns the raw target of `path` when it is a symbolic link.
    ///
    /// Returns `None` when `path` is not a symlink, does not exist, or the
    /// target cannot be resolved. The returned path is the literal symlink
    /// target as recorded on disk; callers are responsible for any
    /// equality comparison they need.
    fn read_link(&self, path: PathBuf) -> Option<PathBuf>;
}

/// Production [`QeProbe`] implementation backed by synchronous filesystem
/// calls.
///
/// The QE engine runs each check once per `/qe:check` invocation against
/// cheap stat and small file reads, so a synchronous adapter avoids the
/// overhead of an async future per probe call.
pub struct RealQeProbe;

impl QeProbe for RealQeProbe {
    fn is_file(&self, path: PathBuf) -> bool {
        path.is_file()
    }

    fn read_to_string(&self, path: PathBuf) -> Option<String> {
        fs::read_to_string(path).ok()
    }

    fn read_link(&self, path: PathBuf) -> Option<PathBuf> {
        fs::read_link(path).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs as unix_fs;

    use tempfile::tempdir;

    use super::*;

    /// Verifies `RealQeProbe::is_file()` distinguishes regular files from
    /// directories and missing entries.
    #[test]
    fn test_real_qe_probe_is_file_returns_true_only_for_regular_files() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let file_path = temp_dir.path().join("note.md");
        fs::write(&file_path, b"hello").expect("write file");
        let probe = RealQeProbe;

        // Act
        let file_result = probe.is_file(file_path);
        let dir_result = probe.is_file(temp_dir.path().to_path_buf());
        let missing_result = probe.is_file(temp_dir.path().join("missing.md"));

        // Assert
        assert!(file_result);
        assert!(!dir_result);
        assert!(!missing_result);
    }

    /// Verifies `RealQeProbe::read_to_string()` reads file contents and
    /// surfaces missing files as `None`.
    #[test]
    fn test_real_qe_probe_read_to_string_returns_contents_or_none() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let file_path = temp_dir.path().join("readme.md");
        fs::write(&file_path, b"hello world").expect("write file");
        let probe = RealQeProbe;

        // Act
        let present_contents = probe.read_to_string(file_path);
        let missing_contents = probe.read_to_string(temp_dir.path().join("missing.md"));

        // Assert
        assert_eq!(present_contents.as_deref(), Some("hello world"));
        assert!(missing_contents.is_none());
    }

    /// Verifies `RealQeProbe::read_link()` returns the literal symlink
    /// target for symlinks and `None` for regular files or missing paths.
    #[test]
    fn test_real_qe_probe_read_link_returns_target_for_symlinks() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let target_path = temp_dir.path().join("AGENTS.md");
        fs::write(&target_path, b"# Agents").expect("write target");
        let symlink_path = temp_dir.path().join("CLAUDE.md");
        unix_fs::symlink("AGENTS.md", &symlink_path).expect("create symlink");
        let probe = RealQeProbe;

        // Act
        let symlink_target = probe.read_link(symlink_path);
        let regular_target = probe.read_link(target_path);
        let missing_target = probe.read_link(temp_dir.path().join("missing.md"));

        // Assert
        assert_eq!(symlink_target, Some(PathBuf::from("AGENTS.md")));
        assert!(regular_target.is_none());
        assert!(missing_target.is_none());
    }
}
