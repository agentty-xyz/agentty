//! Session worktree isolation guards shared by lifecycle and worker flows.

use std::path::{Path, PathBuf};

use ag_git::{GitClient, GitError};

use super::session_branch;
use crate::app::session::SessionError;
use crate::infra::fs::FsClient;

/// Validated git metadata for one session worktree.
#[derive(Debug)]
pub(super) struct SessionWorktreeValidation {
    /// Canonical main repository checkout that must remain unchanged, or
    /// `None` when the shared repository is bare and therefore has no main
    /// working checkout to guard against.
    pub(super) main_checkout: Option<PathBuf>,
}

/// Verifies that `folder` is an isolated linked worktree for `session_id`.
///
/// The guard checks the folder exists and the checked-out branch matches
/// `wt/<session-id-prefix>`. When git resolves a main working checkout it must
/// be distinct from `folder`, preventing stale or replaced session directories
/// from being reused as provider working directories. When the shared
/// repository is bare there is no main working checkout, so any valid linked
/// worktree passes with `main_checkout` set to `None`.
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

    let Some(main_repo_root) = git_client
        .main_checkout_working_tree(folder.to_path_buf())
        .await
        .map_err(|error| main_checkout_error(&error))?
    else {
        return Ok(SessionWorktreeValidation {
            main_checkout: None,
        });
    };
    ensure_main_repo_root_exists(fs_client, &main_repo_root)?;
    let session_folder = canonicalize_for_isolation(fs_client, folder).await?;
    let main_repo_root = canonicalize_for_isolation(fs_client, &main_repo_root).await?;
    if session_folder == main_repo_root {
        return Err(isolation_error(&format!(
            "session worktree `{}` resolves to the main repository checkout",
            folder.display()
        )));
    }

    Ok(SessionWorktreeValidation {
        main_checkout: Some(main_repo_root),
    })
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

/// Converts main-checkout resolution failures into workflow errors.
fn main_checkout_error(error: &GitError) -> SessionError {
    isolation_error(&format!(
        "failed to resolve main repository checkout: {error}"
    ))
}

/// Formats one session-isolation workflow error.
fn isolation_error(message: &str) -> SessionError {
    SessionError::Workflow(format!("Session isolation violation: {message}"))
}

#[cfg(test)]
#[path = "isolation_test.rs"]
mod tests;
