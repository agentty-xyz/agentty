use std::path::PathBuf;

use ag_git as git;
use ag_session::EnvId;

use super::validate_session_worktree;
use crate::infra::fs;

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
    let fs_client = fs_client_for_validation(PathBuf::from("canonical/session"), repo_root.clone());
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
    git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(PathBuf::from("/tmp/project"))) }));

    // Act
    let validation =
        validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
            .await
            .expect("linked worktree should validate");

    // Assert
    assert_eq!(validation.main_checkout, Some(repo_root));
    assert_eq!(validation.folder, session_folder);
    assert_eq!(validation.branch, "wt/session");
    assert_eq!(validation.id, EnvId::SessionWorktree("session".into()));
}

#[tokio::test]
async fn validate_session_worktree_accepts_bare_shared_repo_without_main_checkout() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session");
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().once().return_const(true);
    fs_client.expect_canonicalize().times(0);
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
    git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));

    // Act
    let validation =
        validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
            .await
            .expect("bare shared repo worktree should validate");

    // Assert
    assert_eq!(validation.main_checkout, None);
    assert_eq!(validation.folder, session_folder);
    assert_eq!(validation.branch, "wt/session");
    assert_eq!(validation.id, EnvId::SessionWorktree("session".into()));
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
    git_client.expect_main_checkout_working_tree().times(0);

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
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(PathBuf::from("/tmp/project"))) }));

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

#[tokio::test]
async fn validate_session_worktree_reports_undetectable_branch() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session");
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().once().return_const(true);
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { None }));
    git_client.expect_main_checkout_working_tree().times(0);

    // Act
    let error =
        validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
            .await
            .expect_err("undetectable branch should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("failed to detect branch for session worktree `/tmp/session`")
    );
}

#[tokio::test]
async fn validate_session_worktree_reports_main_checkout_resolution_failure() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session");
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().once().return_const(true);
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
    git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| {
            Box::pin(async {
                Err(git::GitError::OutputParse(
                    "worktree list failed".to_string(),
                ))
            })
        });

    // Act
    let error =
        validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
            .await
            .expect_err("main checkout resolution failure should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("failed to resolve main repository checkout: worktree list failed")
    );
}

#[tokio::test]
async fn validate_session_worktree_rejects_folder_resolving_to_main_checkout() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session");
    let fs_client = fs_client_for_validation(
        PathBuf::from("/canonical/project"),
        PathBuf::from("/canonical/project"),
    );
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
    git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(PathBuf::from("/tmp/project"))) }));

    // Act
    let error =
        validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
            .await
            .expect_err("folder resolving to the main checkout should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("session worktree `/tmp/session` resolves to the main repository checkout")
    );
}

#[tokio::test]
async fn validate_session_worktree_reports_canonicalization_failure() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session");
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().times(2).return_const(true);
    fs_client.expect_canonicalize().once().returning(|_| {
        Box::pin(async {
            Err(fs::FsError::Io(std::io::Error::other(
                "canonicalize denied",
            )))
        })
    });
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/session".to_string()) }));
    git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(PathBuf::from("/tmp/project"))) }));

    // Act
    let error =
        validate_session_worktree(&fs_client, &git_client, session_folder.as_path(), "session")
            .await
            .expect_err("canonicalization failure should fail");

    // Assert
    assert!(
        error
            .to_string()
            .contains("failed to canonicalize isolation path `/tmp/session`: canonicalize denied")
    );
}
