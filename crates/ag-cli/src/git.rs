use std::fs;
use std::path::{Path, PathBuf};

/// Detects git repository information for the given directory.
/// Returns the current branch name if in a git repository, None otherwise.
pub fn detect_git_info(dir: &Path) -> Option<String> {
    let repo_dir = find_git_repo(dir)?;
    get_git_branch(&repo_dir)
}

/// Walks up the directory tree to find a .git directory.
/// Returns the directory containing .git if found, None otherwise.
fn find_git_repo(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Reads .git/HEAD and extracts the current branch name.
/// Returns the branch name for normal HEAD, or "HEAD@{hash}" for detached HEAD.
fn get_git_branch(repo_dir: &Path) -> Option<String> {
    let head_path = repo_dir.join(".git").join("HEAD");
    let content = fs::read_to_string(head_path).ok()?;
    let content = content.trim();

    // Normal branch: "ref: refs/heads/main"
    if let Some(branch_ref) = content.strip_prefix("ref: refs/heads/") {
        return Some(branch_ref.to_string());
    }

    // Detached HEAD: full commit hash
    if content.len() >= 7 && content.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("HEAD@{}", &content[..7]));
    }

    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_find_git_repo_exists() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");

        // Act
        let result = find_git_repo(temp.path());

        // Assert
        assert_eq!(result, Some(temp.path().to_path_buf()));
    }

    #[test]
    fn test_find_git_repo_not_exists() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");

        // Act
        let result = find_git_repo(temp.path());

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_find_git_repo_parent() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let subdir = temp.path().join("subdir");
        fs::create_dir(&subdir).expect("test setup failed");

        // Act
        let result = find_git_repo(&subdir);

        // Assert
        assert_eq!(result, Some(temp.path().to_path_buf()));
    }

    #[test]
    fn test_get_git_branch_normal() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "ref: refs/heads/main\n").expect("test setup failed");

        // Act
        let result = get_git_branch(temp.path());

        // Assert
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn test_get_git_branch_detached() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0\n")
            .expect("test setup failed");

        // Act
        let result = get_git_branch(temp.path());

        // Assert
        assert_eq!(result, Some("HEAD@a1b2c3d".to_string()));
    }

    #[test]
    fn test_get_git_branch_invalid() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "invalid content\n").expect("test setup failed");

        // Act
        let result = get_git_branch(temp.path());

        // Assert
        assert_eq!(result, None);
    }

    #[test]
    fn test_detect_git_info_full_flow() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).expect("test setup failed");
        let head_path = git_dir.join("HEAD");
        fs::write(&head_path, "ref: refs/heads/feature-branch\n").expect("test setup failed");

        // Act
        let result = detect_git_info(temp.path());

        // Assert
        assert_eq!(result, Some("feature-branch".to_string()));
    }

    #[test]
    fn test_detect_git_info_no_repo() {
        // Arrange
        let temp = TempDir::new().expect("test setup failed");

        // Act
        let result = detect_git_info(temp.path());

        // Assert
        assert_eq!(result, None);
    }
}
