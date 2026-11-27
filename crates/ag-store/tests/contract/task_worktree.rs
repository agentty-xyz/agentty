//! Task worktree Env contracts through the public, migrated SQLite adapter.

use std::sync::Arc;

use ag_session::{TASK_WORKTREE_LIMIT, TaskWorktreeId};
use ag_store::{Database, DbError, TaskWorktreeReservation, TaskWorktreeState};

#[tokio::test]
async fn a_session_reuses_its_worktree_slot_only_after_removal_finishes() -> Result<(), DbError> {
    // Arrange
    let database = Database::in_memory_with_timestamp_source(Arc::new(|| 123)).await?;
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-5.6-sol", "main", "Review", project)
        .await?;
    let worktrees = database.task_worktrees();
    let ids = (0..=TASK_WORKTREE_LIMIT)
        .map(|index| TaskWorktreeId {
            session_id: "session".into(),
            worktree_id: format!("writer-{index}"),
        })
        .collect::<Vec<_>>();
    for id in &ids[..TASK_WORKTREE_LIMIT] {
        worktrees.reserve_task_worktree(id, "base-commit").await?;
        worktrees.mark_task_worktree_ready(id).await?;
    }
    let extra = &ids[TASK_WORKTREE_LIMIT];

    // Act
    let at_limit = worktrees
        .reserve_task_worktree(extra, "base-commit")
        .await?;
    worktrees.begin_task_worktree_removal(&ids[0]).await?;
    let while_removing = worktrees
        .reserve_task_worktree(extra, "base-commit")
        .await?;
    worktrees.delete_task_worktree(&ids[0]).await?;
    let after_removal = worktrees
        .reserve_task_worktree(extra, "next-commit")
        .await?;
    let listed = worktrees.list_task_worktrees("session").await?;

    // Assert
    assert_eq!(at_limit, TaskWorktreeReservation::LimitReached);
    assert_eq!(while_removing, TaskWorktreeReservation::LimitReached);
    assert_eq!(after_removal, TaskWorktreeReservation::Reserved);
    assert_eq!(
        listed
            .iter()
            .map(|row| (
                row.id.worktree_id.as_str(),
                row.base_commit.as_str(),
                row.state
            ))
            .collect::<Vec<_>>(),
        [
            ("writer-1", "base-commit", TaskWorktreeState::Ready),
            ("writer-2", "next-commit", TaskWorktreeState::Creating),
        ]
    );

    Ok(())
}
