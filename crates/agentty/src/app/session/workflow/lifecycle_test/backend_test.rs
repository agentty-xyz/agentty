use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_contracts::AgentError;
use ag_forge::MockReviewRequestClient;
use ag_git::{GitError, MockGitClient};

use super::support::{
    database_with_session, load_persisted_session_row, session_manager_with_one_session,
    test_services_with_fs_client, test_session,
};
use crate::app::service::MockSessionRunFactory;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::Status;
use crate::infra::clock::RealClock;
use crate::infra::fs::MockFsClient;

#[tokio::test]
async fn unavailable_backend_preserves_draft_and_cleans_only_new_worktrees() {
    for (existing, cleanup_fails) in [(true, false), (false, false), (false, true)] {
        // Arrange
        let mut session = test_session("staged request", Status::Draft, None, "");
        session.is_draft = true;
        session.agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let database = database_with_session(&session).await;
        let mut manager = session_manager_with_one_session(session);
        let (fs, git) = draft_workspace_boundaries(existing, cleanup_fails);
        let mut services = test_services_with_fs_client(
            &database,
            Arc::new(RealClock),
            Arc::new(fs),
            Arc::new(git),
            Arc::new(MockReviewRequestClient::new()),
        );
        let mut session_run_factory = MockSessionRunFactory::new();
        session_run_factory
            .expect_setup_backend()
            .once()
            .returning(|kind, _| {
                assert_eq!(kind, AgentKind::Antigravity);

                Err(AgentError::Backend(
                    "Antigravity CLI is unavailable".to_string(),
                ))
            });
        services.set_session_run_factory(Arc::new(session_run_factory));

        // Act
        let result = manager
            .ensure_session_worktree_ready(&services, "session-id")
            .await;

        // Assert: setup fails before a turn is admitted; existing resources
        // survive, and the expectations require cleanup for new resources.
        let error = result.expect_err("unavailable provider must reject setup");
        assert!(
            error
                .to_string()
                .contains("Failed to setup session backend")
        );
        assert_eq!(
            error.to_string().contains("Cleanup also failed"),
            cleanup_fails,
            "{error}"
        );
        assert!(
            manager
                .session_or_err("session-id")
                .expect("draft")
                .is_draft
        );
        let row = load_persisted_session_row(&database).await;
        assert!(row.is_draft);
        assert_eq!(row.prompt, "staged request");
    }
}

fn draft_workspace_boundaries(
    existing: bool,
    cleanup_fails: bool,
) -> (MockFsClient, MockGitClient) {
    let mut fs = MockFsClient::new();
    let mut git = MockGitClient::new();
    fs.expect_is_dir()
        .times(if existing { 3 } else { 1 })
        .return_const(existing);
    if existing {
        fs.expect_canonicalize().times(2).returning(|path| {
            Box::pin(async move {
                Ok(if path == Path::new("/tmp/project") {
                    PathBuf::from("/tmp/project")
                } else {
                    PathBuf::from("/tmp/session")
                })
            })
        });
        git.expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/session-".to_string()) }));
        git.expect_main_checkout_working_tree()
            .once()
            .returning(|_| Box::pin(async { Ok(Some(PathBuf::from("/tmp/project"))) }));
    } else {
        git.expect_find_git_repo_root()
            .once()
            .returning(|_| Box::pin(async { Some(PathBuf::from("/tmp/project")) }));
        git.expect_create_worktree()
            .once()
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        fs.expect_create_dir_all()
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
        git.expect_remove_worktree().once().returning(move |_| {
            Box::pin(async move {
                if cleanup_fails {
                    Err(GitError::OutputParse("worktree removal failed".to_string()))
                } else {
                    Ok(())
                }
            })
        });
        git.expect_delete_branch()
            .once()
            .returning(|_, _| Box::pin(async { Ok(()) }));
        fs.expect_remove_dir_all()
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
    }

    (fs, git)
}
