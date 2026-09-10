use super::support::{prepare_saved_operation, reservation};
use crate::AppRepositories;
use crate::session_preparation::SessionPreparationState;

#[tokio::test]
async fn cancellation_rejects_late_completion_and_submission() {
    // Arrange
    let (repositories, _) = AppRepositories::in_memory_with_pool().await.expect("db");
    let project_id = repositories
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    let sessions = repositories.sessions();
    sessions
        .reserve_session(reservation("session", project_id))
        .await
        .expect("reserve");

    // Act
    let worker_owns_cleanup = sessions
        .cancel_session_preparation("session")
        .await
        .expect("cancel");
    let late_completion = sessions
        .update_session_preparation("session", SessionPreparationState::Ready, None)
        .await
        .expect("late completion");
    let canceled_submission = sessions
        .save_preparation_prompt("session", "late prompt")
        .await
        .expect("canceled submission");

    // Assert
    assert!(worker_owns_cleanup);
    assert!(!late_completion);
    assert!(!canceled_submission);
}

#[tokio::test]
async fn cancellation_owns_cleanup_when_preparation_finished_first() {
    for state in [
        SessionPreparationState::Ready,
        SessionPreparationState::Failed,
    ] {
        // Arrange
        let repositories = AppRepositories::in_memory().await.expect("db");
        prepare_saved_operation(&repositories).await;
        repositories
            .sessions()
            .update_session_preparation("first", state, Some("setup result"))
            .await
            .expect("complete");

        // Act
        let worker_owns_cleanup = repositories
            .sessions()
            .cancel_session_preparation("first")
            .await
            .expect("cancel");
        let preparation = repositories
            .sessions()
            .load_session_preparation("first")
            .await
            .expect("load")
            .expect("row");

        // Assert
        assert!(!worker_owns_cleanup);
        assert_eq!(preparation.state, SessionPreparationState::Canceled);
        assert!(preparation.error.is_none());
        assert!(
            !repositories
                .sessions()
                .cancel_session_preparation("missing")
                .await
                .expect("legacy")
        );
    }
}
