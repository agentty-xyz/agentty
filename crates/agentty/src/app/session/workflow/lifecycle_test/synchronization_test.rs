use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::support::{
    create_passthrough_mock_fs_client, database_with_session, database_with_session_and_pool,
    session_manager_with_one_session, test_services_with_event_receiver,
    test_services_with_fs_client, test_session,
};
use crate::app::session::SessionError;
use crate::domain::session::Status;
use crate::infra::clock::RealClock;

#[tokio::test]
async fn merged_session_rejects_chat_submission_entry_points() {
    // Arrange
    let session = test_session("Prompt", Status::Merged, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let (services, mut event_rx) = test_services_with_event_receiver(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let queue_result = session_manager.enqueue_message(&services, "session-id", "queued reply");
    let reply_enqueued = session_manager
        .reply(&services, "session-id", "reply")
        .await;
    let review_reply_enqueued = session_manager
        .reply_to_review_comments(
            &services,
            "session-id",
            "review reply",
            vec!["thread-42".to_string()],
        )
        .await;

    // Assert
    assert!(matches!(
        queue_result,
        Err(SessionError::Workflow(message))
            if message == "Merged sessions cannot queue chat messages"
    ));
    assert!(!reply_enqueued);
    assert!(!review_reply_enqueued);
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn synchronous_fork_checkout_failure_removes_reservation_and_replay() {
    // Arrange
    let source = test_session("source", Status::Review, Some("Source"), "history");
    let source_id = source.id.clone();
    let (database, pool) = database_with_session_and_pool(&source).await;
    let mut manager = session_manager_with_one_session(source);
    let original_replay = manager.workflow_state.pending_history_replay.clone();
    let mut git = git::MockGitClient::new();
    git.expect_find_git_repo_root()
        .times(4)
        .returning(|path| Box::pin(async move { Some(path) }));
    git.expect_ref_hash()
        .times(2)
        .returning(|_, _| Box::pin(async { Ok("source-commit".to_string()) }));
    git.expect_create_worktree()
        .times(2)
        .returning(|_, _, _, _| {
            Box::pin(async {
                Err(git::GitError::CommandFailed {
                    command: "git worktree add".to_string(),
                    stderr: "checkout rejected".to_string(),
                })
            })
        });
    git.expect_remove_worktree().never();
    git.expect_delete_branch().never();
    let services = test_services_with_fs_client(
        &database,
        Arc::new(RealClock),
        Arc::new(create_passthrough_mock_fs_client()),
        Arc::new(git),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    for _attempt in 0..2 {
        // Act
        let result = manager.fork_session(&services, &source_id).await;
        let preparation_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_preparation")
            .fetch_one(&pool)
            .await
            .expect("preparation count");

        // Assert
        assert!(
            result
                .expect_err("checkout failure")
                .to_string()
                .contains("checkout rejected")
        );
        assert_eq!(
            manager.workflow_state.pending_history_replay,
            original_replay
        );
        assert_eq!(preparation_count, 0);
        let sessions = database.sessions().load_sessions().await.expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, source_id.as_str());
    }
}
