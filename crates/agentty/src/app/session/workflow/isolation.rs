//! Session worktree isolation guards shared by lifecycle and worker flows.

use std::path::{Path, PathBuf};

use super::session_branch;
use crate::app::session::SessionError;
use crate::infra::fs::FsClient;
use crate::infra::git::{GitClient, GitError};

/// Validated git metadata for one session worktree.
#[derive(Debug)]
pub(super) struct SessionWorktreeValidation {
    /// Canonical main repository checkout that must remain unchanged.
    pub(super) main_repo_root: PathBuf,
}

/// Verifies that `folder` is an isolated linked worktree for `session_id`.
///
/// The guard checks the folder exists, the checked-out branch matches
/// `wt/<session-id-prefix>`, and git resolves a distinct main repository
/// checkout. This prevents stale or replaced session directories from being
/// reused as provider working directories.
///
/// # Errors
/// Returns [`SessionError::Workflow`] when the folder is missing, branch
/// metadata does not match the session, or the folder resolves to the main
/// checkout instead of a linked worktree.
pub(super) async fn validate_session_worktree(
    fs_client: &dyn FsClient,
    git_client: &dyn GitClient,
    folder: &Path,
    session_id: &str,
) -> Result<SessionWorktreeValidation, SessionError> {
    if !fs_client.is_dir(folder.to_path_buf()) {
        return Err(isolation_error(&format!(
            "session worktree folder is missing: {}",
            folder.display()
        )));
    }

    let expected_branch = session_branch(session_id);
    let detected_branch = git_client
        .detect_git_info(folder.to_path_buf())
        .await
        .ok_or_else(|| {
            isolation_error(&format!(
                "failed to detect branch for session worktree `{}`",
                folder.display()
            ))
        })?;
    if detected_branch != expected_branch {
        return Err(isolation_error(&format!(
            "session worktree `{}` is on branch `{detected_branch}` instead of `{expected_branch}`",
            folder.display()
        )));
    }

    let main_repo_root = git_client
        .main_repo_root(folder.to_path_buf())
        .await
        .map_err(|error| main_repo_root_error(&error))?;
    ensure_main_repo_root_exists(fs_client, &main_repo_root)?;
    let session_folder = canonicalize_for_isolation(fs_client, folder).await?;
    let main_repo_root = canonicalize_for_isolation(fs_client, &main_repo_root).await?;
    if session_folder == main_repo_root {
        return Err(isolation_error(&format!(
            "session worktree `{}` resolves to the main repository checkout",
            folder.display()
        )));
    }

    Ok(SessionWorktreeValidation { main_repo_root })
}

/// Verifies git resolved an existing main checkout before canonicalization.
fn ensure_main_repo_root_exists(
    fs_client: &dyn FsClient,
    main_repo_root: &Path,
) -> Result<(), SessionError> {
    if fs_client.is_dir(main_repo_root.to_path_buf()) {
        return Ok(());
    }

    Err(isolation_error(&format!(
        "main repository checkout is missing: {}",
        main_repo_root.display()
    )))
}

/// Resolves a path for isolation comparisons through the filesystem boundary.
async fn canonicalize_for_isolation(
    fs_client: &dyn FsClient,
    path: &Path,
) -> Result<PathBuf, SessionError> {
    fs_client
        .canonicalize(path.to_path_buf())
        .await
        .map_err(|error| {
            isolation_error(&format!(
                "failed to canonicalize isolation path `{}`: {error}",
                path.display()
            ))
        })
}

/// Converts main-repository resolution failures into workflow errors.
fn main_repo_root_error(error: &GitError) -> SessionError {
    isolation_error(&format!(
        "failed to resolve main repository checkout: {error}"
    ))
}

/// Formats one session-isolation workflow error.
fn isolation_error(message: &str) -> SessionError {
    SessionError::Workflow(format!("Session isolation violation: {message}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::infra::{fs, git};

    /// Builds an `FsClient` mock that treats `session_folder` and `repo_root`
    /// as existing canonical paths.
    fn fs_client_for_validation(session_folder: PathBuf, repo_root: PathBuf) -> fs::MockFsClient {
        let mut fs_client = fs::MockFsClient::new();
        fs_client
            .expect_is_dir()
            .times(2)
            .returning(|path| path.ends_with("session") || path.ends_with("project"));
        fs_client
            .expect_canonicalize()
            .times(2)
            .returning(move |path| {
                let session_folder = session_folder.clone();
                let repo_root = repo_root.clone();

                Box::pin(async move {
                    if path.ends_with("session") {
                        Ok(session_folder)
                    } else {
                        Ok(repo_root)
                    }
                })
            });

        fs_client
    }

    #[tokio::test]
    async fn validate_session_worktree_returns_main_repo_for_linked_worktree() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session");
        let repo_root = PathBuf::from("/tmp/project");
        let fs_client = fs_client_for_validation(session_folder.clone(), repo_root.clone());
        let mut git_client = git::MockGitClient::new();
        git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
        git_client
            .expect_main_repo_root()
            .once()
            .returning(|_| Box::pin(async { Ok(PathBuf::from("/tmp/project")) }));

        // Act
        let validation =
            validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
                .await
                .expect("linked worktree should validate");

        // Assert
        assert_eq!(validation.main_repo_root, repo_root);
    }

    #[tokio::test]
    async fn validate_session_worktree_rejects_branch_mismatch() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session");
        let mut fs_client = fs::MockFsClient::new();
        fs_client.expect_is_dir().once().return_const(true);
        let mut git_client = git::MockGitClient::new();
        git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("main".to_string()) }));
        git_client.expect_main_repo_root().times(0);

        // Act
        let error =
            validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
                .await
                .expect_err("branch mismatch should fail");

        // Assert
        assert!(error.to_string().contains("instead of `wt/session`"));
    }

    #[tokio::test]
    async fn validate_session_worktree_reports_missing_main_repo_checkout() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session");
        let mut fs_client = fs::MockFsClient::new();
        fs_client
            .expect_is_dir()
            .times(2)
            .returning(|path| path.ends_with("session"));
        fs_client.expect_canonicalize().times(0);
        let mut git_client = git::MockGitClient::new();
        git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
        git_client
            .expect_main_repo_root()
            .once()
            .returning(|_| Box::pin(async { Ok(PathBuf::from("/tmp/project")) }));

        // Act
        let error =
            validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
                .await
                .expect_err("missing main checkout should fail");

        // Assert
        assert!(
            error
                .to_string()
                .contains("main repository checkout is missing: /tmp/project")
        );
    }
}
