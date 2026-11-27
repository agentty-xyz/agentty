//! Task worktree Env records: base commits, the per-session limit, and the
//! creation and removal progress that restart recovery reads.

use std::sync::Arc;

use ag_session::{TASK_WORKTREE_LIMIT, TaskWorktreeId};
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::connection::begin_write;
use crate::{DbError, TimestampSource};

/// How far the host got in creating or removing a task worktree.
///
/// A crash can leave a worktree in any state, so each state names what the
/// host does next: recreate a `Creating` worktree from its base commit, keep
/// a `Ready` one with its commits, and finish removing a `Removing` one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum TaskWorktreeState {
    /// The worktree may be missing or partly made.
    Creating,
    /// The worktree exists and may hold commits.
    Ready,
    /// The worktree is being removed and must not be used.
    Removing,
}

/// One recorded task worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskWorktreeRow {
    /// Commit the worktree branch starts from, fixed at reservation.
    pub base_commit: String,
    /// Reservation timestamp in Unix seconds.
    pub created_at: i64,
    /// Owning session and worktree identity.
    pub id: TaskWorktreeId,
    /// Creation or removal progress.
    pub state: TaskWorktreeState,
    /// Last state change in Unix seconds.
    pub updated_at: i64,
}

/// Outcome of reserving a task worktree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskWorktreeReservation {
    /// The record exists in [`TaskWorktreeState::Creating`].
    Reserved,
    /// The session already keeps [`TASK_WORKTREE_LIMIT`] task worktrees.
    LimitReached,
    /// Another worktree of the session derives the same branch and
    /// directory names, so both would share one checkout.
    NameTaken,
}

/// Persistence boundary for task worktree Envs.
///
/// Records only track progress; hosts create and remove the directories and
/// branches. Deleting a session deletes its records, so hosts read them
/// before deleting the session.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait TaskWorktreeRepository: Send + Sync {
    /// Records a new worktree from `base_commit` unless its session already
    /// keeps [`TASK_WORKTREE_LIMIT`] of them. Worktrees being created or
    /// removed count toward the limit because they still use disk space.
    /// Names derive from shortened identifiers, so a different identifier
    /// whose names match a kept worktree is refused.
    ///
    /// # Errors
    /// Fails when the session is missing, the identity already exists, or
    /// the identifier or base commit is blank.
    async fn reserve_task_worktree(
        &self,
        id: &TaskWorktreeId,
        base_commit: &str,
    ) -> Result<TaskWorktreeReservation, DbError>;

    /// Marks a created worktree ready. Returns `false` when the record is
    /// missing or removal has already begun, so the host must not use it.
    async fn mark_task_worktree_ready(&self, id: &TaskWorktreeId) -> Result<bool, DbError>;

    /// Moves a worktree to [`TaskWorktreeState::Removing`] before the host
    /// deletes its directory and branch, also when retrying a removal.
    /// Returns `false` when no record exists.
    async fn begin_task_worktree_removal(&self, id: &TaskWorktreeId) -> Result<bool, DbError>;

    /// Deletes a record after its directory and branch are gone; a missing
    /// record is a no-op.
    async fn delete_task_worktree(&self, id: &TaskWorktreeId) -> Result<(), DbError>;

    /// Lists one session's task worktrees in reservation order.
    async fn list_task_worktrees(&self, session_id: &str) -> Result<Vec<TaskWorktreeRow>, DbError>;
}

/// `SQLite` implementation of [`TaskWorktreeRepository`].
#[derive(Clone)]
pub(crate) struct SqliteTaskWorktreeRepository {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteTaskWorktreeRepository {
    /// Creates a task worktree repository backed by the provided pool and
    /// timestamp source.
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }
}

#[async_trait]
impl TaskWorktreeRepository for SqliteTaskWorktreeRepository {
    async fn reserve_task_worktree(
        &self,
        id: &TaskWorktreeId,
        base_commit: &str,
    ) -> Result<TaskWorktreeReservation, DbError> {
        let session_id = id.session_id.as_str();
        let mut transaction = begin_write(&self.pool, "reserve_task_worktree").await?;
        let kept_ids = sqlx::query_scalar!(
            r"
SELECT id
FROM task_worktree
WHERE session_id = ?
",
            session_id
        )
        .fetch_all(&mut *transaction)
        .await?;
        if kept_ids.len() >= TASK_WORKTREE_LIMIT {
            return Ok(TaskWorktreeReservation::LimitReached);
        }
        // An identical identifier is left to the primary key to reject.
        let directory_name = id.directory_name();
        let name_taken = kept_ids
            .into_iter()
            .filter(|kept_id| *kept_id != id.worktree_id)
            .any(|worktree_id| {
                TaskWorktreeId {
                    session_id: id.session_id.clone(),
                    worktree_id,
                }
                .directory_name()
                    == directory_name
            });
        if name_taken {
            return Ok(TaskWorktreeReservation::NameTaken);
        }

        let created_at = self.timestamp_source.now_timestamp_seconds();
        sqlx::query!(
            r"
INSERT INTO task_worktree (session_id, id, base_commit, state, created_at, updated_at)
VALUES (?, ?, ?, 'creating', ?, ?)
",
            session_id,
            id.worktree_id,
            base_commit,
            created_at,
            created_at
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(TaskWorktreeReservation::Reserved)
    }

    async fn mark_task_worktree_ready(&self, id: &TaskWorktreeId) -> Result<bool, DbError> {
        let session_id = id.session_id.as_str();
        let updated_at = self.timestamp_source.now_timestamp_seconds();
        let result = sqlx::query!(
            r"
UPDATE task_worktree
SET state = 'ready', updated_at = ?
WHERE session_id = ? AND id = ? AND state = 'creating'
",
            updated_at,
            session_id,
            id.worktree_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn begin_task_worktree_removal(&self, id: &TaskWorktreeId) -> Result<bool, DbError> {
        let session_id = id.session_id.as_str();
        let updated_at = self.timestamp_source.now_timestamp_seconds();
        let result = sqlx::query!(
            r"
UPDATE task_worktree
SET state = 'removing', updated_at = ?
WHERE session_id = ? AND id = ?
",
            updated_at,
            session_id,
            id.worktree_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn delete_task_worktree(&self, id: &TaskWorktreeId) -> Result<(), DbError> {
        let session_id = id.session_id.as_str();
        sqlx::query!(
            r"
DELETE FROM task_worktree
WHERE session_id = ? AND id = ?
",
            session_id,
            id.worktree_id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn list_task_worktrees(&self, session_id: &str) -> Result<Vec<TaskWorktreeRow>, DbError> {
        let rows = sqlx::query!(
            r#"
SELECT session_id AS "session_id!: String",
       id AS "id!: String",
       base_commit,
       state AS "state: TaskWorktreeState",
       created_at,
       updated_at
FROM task_worktree
WHERE session_id = ?
ORDER BY created_at, id
"#,
            session_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| TaskWorktreeRow {
                base_commit: row.base_commit,
                created_at: row.created_at,
                id: TaskWorktreeId {
                    session_id: row.session_id.into(),
                    worktree_id: row.id,
                },
                state: row.state,
                updated_at: row.updated_at,
            })
            .collect())
    }
}

#[cfg(test)]
#[path = "task_worktree_test.rs"]
mod tests;
