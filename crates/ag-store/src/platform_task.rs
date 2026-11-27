//! Durable record of the task a platform's Sync holds for one session.
//!
//! A session asks the platform for a task before its first turn starts. The
//! row keeps that question, its answer, and the claim that proves the
//! session holds the task, so a restart neither forgets the claim nor
//! repeats a start the platform may have answered. The claim is a secret:
//! it never appears in `Debug` output.

use std::fmt;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::DbError;
use crate::connection::begin_write;

/// Where a session's platform task stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum PlatformTaskState {
    /// A start request is on its way; the session waits for its answer.
    Checking,
    /// Agentty quit before the start request was answered; the session
    /// waits until the user asks again.
    Interrupted,
    /// Another session holds the work; the session waits for the user's
    /// choice.
    Conflict,
    /// The start was answered: the session holds the task when a claim is
    /// kept, and runs without one otherwise.
    Resolved,
}

/// One session's platform task.
#[derive(Clone, Eq, PartialEq)]
pub struct PlatformTaskRow {
    /// Message text sent as the work's brief.
    pub brief: String,
    /// Proof that the session holds the task, while it does.
    pub claim_id: Option<String>,
    /// Last activity of the session holding the conflicting task, in Unix
    /// seconds.
    pub holder_last_active_at: Option<i64>,
    /// Label of the session holding the conflicting task.
    pub holder_session: Option<String>,
    /// Task numbers the user declared to be other work.
    pub not_duplicates: Vec<u64>,
    /// Serialized first prompt held until the start is answered, when no
    /// other store holds it.
    pub pending_prompt: Option<String>,
    /// Platform origin that answered, the only one the claim goes to.
    pub platform_url: String,
    /// Session the task belongs to.
    pub session_id: String,
    /// Where the task stands.
    pub state: PlatformTaskState,
    /// Task number within its project, once the platform named one.
    pub task_number: Option<u64>,
    /// Project of the task, once the platform named one.
    pub task_project: Option<String>,
    /// Task title, once the platform named one.
    pub task_title: Option<String>,
    /// Task page on the platform, when it is one of the platform's pages.
    pub task_url: Option<String>,
    /// Text sent as the work's title.
    pub title: String,
}

impl fmt::Debug for PlatformTaskRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformTaskRow")
            .field("claim_id", &self.claim_id.as_ref().map(|_| "<redacted>"))
            .field("not_duplicates", &self.not_duplicates)
            .field("platform_url", &self.platform_url)
            .field("session_id", &self.session_id)
            .field("state", &self.state)
            .field("task_number", &self.task_number)
            .field("task_project", &self.task_project)
            .finish_non_exhaustive()
    }
}

/// A start request about to be sent for a session's first message.
#[derive(Clone, Copy, Debug)]
pub struct NewPlatformTask<'a> {
    /// Message text sent as the work's brief.
    pub brief: &'a str,
    /// Platform origin the request goes to.
    pub platform_url: &'a str,
    /// Session asking.
    pub session_id: &'a str,
    /// Text sent as the work's title.
    pub title: &'a str,
}

/// The task a platform names in an answer.
#[derive(Clone, Copy, Debug)]
pub struct PlatformTaskName<'a> {
    /// Task number within its project.
    pub number: u64,
    /// Project of the task.
    pub project: &'a str,
    /// Task title.
    pub title: &'a str,
    /// Task page on the platform, when it is one of the platform's pages.
    pub url: Option<&'a str>,
}

/// A held task's claim, taken from its row so it is sent at most once.
#[derive(Clone, Eq, PartialEq)]
pub struct PlatformTaskClaim {
    /// Proof that the session held the task.
    pub claim_id: String,
    /// Platform origin that issued the claim.
    pub platform_url: String,
    /// Task number within its project.
    pub task_number: u64,
    /// Project of the task.
    pub task_project: String,
}

impl fmt::Debug for PlatformTaskClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformTaskClaim")
            .field("claim_id", &"<redacted>")
            .field("platform_url", &self.platform_url)
            .field("task_number", &self.task_number)
            .field("task_project", &self.task_project)
            .finish()
    }
}

/// A held task whose session reached `Merged`, `Done`, or `Canceled`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettledPlatformTask {
    /// Address of the session's review request, when it has one.
    pub review_request_url: Option<String>,
    /// Session holding the task.
    pub session_id: String,
    /// Persisted session status.
    pub status: String,
    /// Session title, when it has one.
    pub title: Option<String>,
}

/// Persistence boundary for sessions' platform tasks.
#[async_trait]
pub trait PlatformTaskRepository: Send + Sync {
    /// Records that a start request is about to be sent, unless the session
    /// already has a platform task. Returns whether it was recorded.
    async fn begin_platform_task(&self, task: NewPlatformTask<'_>) -> Result<bool, DbError>;

    /// Loads one session's platform task.
    async fn load_platform_task(
        &self,
        session_id: &str,
    ) -> Result<Option<PlatformTaskRow>, DbError>;

    /// Loads the platform tasks of `project_id`'s sessions that still hold
    /// their session back from starting.
    async fn load_waiting_platform_tasks(
        &self,
        project_id: i64,
    ) -> Result<Vec<PlatformTaskRow>, DbError>;

    /// Keeps `prompt` until the start is answered, unless one is kept
    /// already or the start was answered. Returns whether it was kept.
    async fn save_platform_task_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<bool, DbError>;

    /// Removes and returns the prompt kept until the start was answered.
    async fn take_platform_task_prompt(&self, session_id: &str) -> Result<Option<String>, DbError>;

    /// Records that a start request is sent again, to `platform_url`, for a
    /// session waiting on a conflict or an interrupted request, with the
    /// task numbers declared other work. Returns whether the session was
    /// waiting.
    async fn check_platform_task_again(
        &self,
        session_id: &str,
        platform_url: &str,
        not_duplicates: &[u64],
    ) -> Result<bool, DbError>;

    /// Records the task a start answered and the claim to it. Returns
    /// whether the start was still waiting for its answer.
    async fn hold_platform_task(
        &self,
        session_id: &str,
        task: PlatformTaskName<'_>,
        claim_id: &str,
    ) -> Result<bool, DbError>;

    /// Records that another session, labelled `holder_session` when it has
    /// a label, holds the work. Returns whether the start was still waiting
    /// for its answer.
    async fn record_platform_task_conflict(
        &self,
        session_id: &str,
        task: PlatformTaskName<'_>,
        holder_session: Option<&str>,
        holder_last_active_at: i64,
    ) -> Result<bool, DbError>;

    /// Lets a waiting session start without a task. Returns whether it was
    /// waiting.
    async fn resolve_platform_task(&self, session_id: &str) -> Result<bool, DbError>;

    /// Removes and returns the session's claim, so it is used at most once.
    async fn take_platform_task_claim(
        &self,
        session_id: &str,
    ) -> Result<Option<PlatformTaskClaim>, DbError>;

    /// Lists held tasks whose sessions reached `Merged`, `Done`, or
    /// `Canceled`.
    async fn load_settled_platform_tasks(&self) -> Result<Vec<SettledPlatformTask>, DbError>;

    /// Forgets every claim, as a sign-out does.
    async fn forget_platform_task_claims(&self) -> Result<(), DbError>;

    /// Marks start requests a previous run left unanswered as interrupted,
    /// so none is repeated without the user.
    async fn recover_platform_tasks(&self) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`PlatformTaskRepository`].
#[derive(Clone)]
pub(crate) struct SqlitePlatformTaskRepository {
    pool: SqlitePool,
}

impl SqlitePlatformTaskRepository {
    /// Creates a repository backed by `pool`.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// Stored form of [`PlatformTaskRow`].
struct StoredPlatformTask {
    brief: String,
    claim_id: Option<String>,
    holder_last_active_at: Option<i64>,
    holder_session: Option<String>,
    not_duplicates: String,
    pending_prompt: Option<String>,
    platform_url: String,
    session_id: String,
    state: PlatformTaskState,
    task_number: Option<i64>,
    task_project: Option<String>,
    task_title: Option<String>,
    task_url: Option<String>,
    title: String,
}

impl From<StoredPlatformTask> for PlatformTaskRow {
    fn from(stored: StoredPlatformTask) -> Self {
        Self {
            brief: stored.brief,
            claim_id: stored.claim_id,
            holder_last_active_at: stored.holder_last_active_at,
            holder_session: stored.holder_session,
            not_duplicates: stored
                .not_duplicates
                .split(',')
                .filter_map(|number| number.parse().ok())
                .collect(),
            pending_prompt: stored.pending_prompt,
            platform_url: stored.platform_url,
            session_id: stored.session_id,
            state: stored.state,
            task_number: stored.task_number.and_then(|number| number.try_into().ok()),
            task_project: stored.task_project,
            task_title: stored.task_title,
            task_url: stored.task_url,
            title: stored.title,
        }
    }
}

/// Converts a task number for storage; numbers beyond `i64` cannot come
/// from a platform answer Agentty accepts.
fn stored_number(number: u64) -> i64 {
    i64::try_from(number).unwrap_or(i64::MAX)
}

#[async_trait]
impl PlatformTaskRepository for SqlitePlatformTaskRepository {
    async fn begin_platform_task(&self, task: NewPlatformTask<'_>) -> Result<bool, DbError> {
        let result = sqlx::query!(
            r"
INSERT INTO session_platform_task (session_id, state, platform_url, title, brief)
VALUES (?, 'checking', ?, ?, ?)
ON CONFLICT(session_id) DO NOTHING
",
            task.session_id,
            task.platform_url,
            task.title,
            task.brief
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn load_platform_task(
        &self,
        session_id: &str,
    ) -> Result<Option<PlatformTaskRow>, DbError> {
        let row = sqlx::query_as!(
            StoredPlatformTask,
            r#"
SELECT session_id AS "session_id!: String", state AS "state: PlatformTaskState", platform_url,
       title, brief, not_duplicates, pending_prompt, task_project, task_number, task_title,
       task_url, claim_id, holder_session, holder_last_active_at
FROM session_platform_task
WHERE session_id = ?
"#,
            session_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(PlatformTaskRow::from))
    }

    async fn load_waiting_platform_tasks(
        &self,
        project_id: i64,
    ) -> Result<Vec<PlatformTaskRow>, DbError> {
        let rows = sqlx::query_as!(
            StoredPlatformTask,
            r#"
SELECT task.session_id AS "session_id!: String", task.state AS "state: PlatformTaskState",
       task.platform_url, task.title, task.brief, task.not_duplicates, task.pending_prompt,
       task.task_project, task.task_number, task.task_title, task.task_url, task.claim_id,
       task.holder_session, task.holder_last_active_at
FROM session_platform_task AS task
JOIN session ON session.id = task.session_id
WHERE session.project_id = ? AND task.state != 'resolved'
ORDER BY task.session_id
"#,
            project_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(PlatformTaskRow::from).collect())
    }

    async fn save_platform_task_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query!(
            r"
UPDATE session_platform_task
SET pending_prompt = ?
WHERE session_id = ? AND pending_prompt IS NULL AND state != 'resolved'
",
            prompt,
            session_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn take_platform_task_prompt(&self, session_id: &str) -> Result<Option<String>, DbError> {
        let mut transaction = begin_write(&self.pool, "take platform task prompt").await?;
        let prompt = sqlx::query_scalar!(
            r"
SELECT pending_prompt
FROM session_platform_task
WHERE session_id = ?
",
            session_id
        )
        .fetch_optional(&mut *transaction)
        .await?
        .flatten();
        sqlx::query!(
            r"
UPDATE session_platform_task
SET pending_prompt = NULL
WHERE session_id = ?
",
            session_id
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(prompt)
    }

    async fn check_platform_task_again(
        &self,
        session_id: &str,
        platform_url: &str,
        not_duplicates: &[u64],
    ) -> Result<bool, DbError> {
        let not_duplicates = not_duplicates
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let result = sqlx::query!(
            r"
UPDATE session_platform_task
SET state = 'checking', platform_url = ?, not_duplicates = ?, task_project = NULL,
    task_number = NULL, task_title = NULL, task_url = NULL, holder_session = NULL,
    holder_last_active_at = NULL
WHERE session_id = ? AND state IN ('conflict', 'interrupted')
",
            platform_url,
            not_duplicates,
            session_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn hold_platform_task(
        &self,
        session_id: &str,
        task: PlatformTaskName<'_>,
        claim_id: &str,
    ) -> Result<bool, DbError> {
        let number = stored_number(task.number);
        let result = sqlx::query!(
            r"
UPDATE session_platform_task
SET state = 'resolved', task_project = ?, task_number = ?, task_title = ?, task_url = ?,
    claim_id = ?, holder_session = NULL, holder_last_active_at = NULL
WHERE session_id = ? AND state = 'checking'
",
            task.project,
            number,
            task.title,
            task.url,
            claim_id,
            session_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn record_platform_task_conflict(
        &self,
        session_id: &str,
        task: PlatformTaskName<'_>,
        holder_session: Option<&str>,
        holder_last_active_at: i64,
    ) -> Result<bool, DbError> {
        let number = stored_number(task.number);
        let result = sqlx::query!(
            r"
UPDATE session_platform_task
SET state = 'conflict', task_project = ?, task_number = ?, task_title = ?, task_url = ?,
    holder_session = ?, holder_last_active_at = ?
WHERE session_id = ? AND state = 'checking'
",
            task.project,
            number,
            task.title,
            task.url,
            holder_session,
            holder_last_active_at,
            session_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn resolve_platform_task(&self, session_id: &str) -> Result<bool, DbError> {
        let result = sqlx::query!(
            r"
UPDATE session_platform_task
SET state = 'resolved'
WHERE session_id = ? AND state != 'resolved'
",
            session_id
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn take_platform_task_claim(
        &self,
        session_id: &str,
    ) -> Result<Option<PlatformTaskClaim>, DbError> {
        let mut transaction = begin_write(&self.pool, "take platform task claim").await?;
        let claim = sqlx::query!(
            r#"
SELECT claim_id AS "claim_id!: String", platform_url,
       task_project AS "task_project!: String", task_number AS "task_number!: i64"
FROM session_platform_task
WHERE session_id = ? AND claim_id IS NOT NULL AND task_project IS NOT NULL
      AND task_number IS NOT NULL
"#,
            session_id
        )
        .fetch_optional(&mut *transaction)
        .await?;
        sqlx::query!(
            r"
UPDATE session_platform_task
SET claim_id = NULL
WHERE session_id = ?
",
            session_id
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(claim.map(|claim| PlatformTaskClaim {
            claim_id: claim.claim_id,
            platform_url: claim.platform_url,
            task_number: claim.task_number.try_into().unwrap_or_default(),
            task_project: claim.task_project,
        }))
    }

    async fn load_settled_platform_tasks(&self) -> Result<Vec<SettledPlatformTask>, DbError> {
        let rows = sqlx::query_as!(
            SettledPlatformTask,
            r#"
SELECT task.session_id AS "session_id!: String", session.status, session.title,
       review.web_url AS "review_request_url?: String"
FROM session_platform_task AS task
JOIN session ON session.id = task.session_id
LEFT JOIN session_review_request AS review ON review.session_id = task.session_id
WHERE task.claim_id IS NOT NULL AND session.status IN ('Merged', 'Done', 'Canceled')
ORDER BY task.session_id
"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    async fn forget_platform_task_claims(&self) -> Result<(), DbError> {
        sqlx::query!(
            r"
UPDATE session_platform_task
SET claim_id = NULL
WHERE claim_id IS NOT NULL
"
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn recover_platform_tasks(&self) -> Result<(), DbError> {
        sqlx::query!(
            r"
UPDATE session_platform_task
SET state = 'interrupted'
WHERE state = 'checking'
"
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "platform_task_test.rs"]
mod tests;
