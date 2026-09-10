//! Durable workspace preparation and first-prompt handoff.

use ag_session::{SessionMessageKind, stored_message_content};
use async_trait::async_trait;

use crate::session::SqliteSessionRepository;
use crate::session_message::SessionMessageStore;
use crate::{DbError, PersistedSessionCreation};

/// Workspace readiness, independent of the session's conversation status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum SessionPreparationState {
    /// A worker owns workspace setup.
    Preparing,
    /// Workspace setup completed successfully.
    Ready,
    /// Setup or prompt handoff needs an explicit retry.
    Failed,
    /// Cancellation prevents further setup or prompt dispatch.
    Canceled,
}

/// Recoverable inputs for workspace setup and a deferred first turn.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct SessionPreparationRow {
    /// Most recent setup failure, if any.
    pub error: Option<String>,
    /// Serialized structured prompt, retained until execution starts.
    pub prompt: Option<String>,
    /// Stable owner of the worktree.
    pub session_id: String,
    /// Branch or frozen commit from which to create the worktree.
    pub start_ref: String,
    /// Current workspace preparation state.
    pub state: SessionPreparationState,
}

/// Persistence boundary for resumable workspace setup.
#[async_trait]
pub trait SessionPreparationRepository: Send + Sync {
    /// Atomically reserves a session and its pending workspace setup.
    async fn reserve_session(&self, session: PersistedSessionCreation<'_>) -> Result<(), DbError>;
    /// Registers lazy draft or fork setup without replacing an existing
    /// attempt.
    async fn insert_session_preparation(
        &self,
        session_id: &str,
        start_ref: &str,
    ) -> Result<(), DbError>;
    /// Loads preparation state for one session.
    async fn load_session_preparation(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionPreparationRow>, DbError>;
    /// Loads preparation state for one project's session list.
    async fn load_session_preparations(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionPreparationRow>, DbError>;
    /// Saves the first prompt without overwriting an earlier accepted
    /// submission.
    async fn save_preparation_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<bool, DbError>;
    /// Transitions setup unless cancellation has already won the race.
    async fn update_session_preparation(
        &self,
        session_id: &str,
        state: SessionPreparationState,
        error: Option<&str>,
    ) -> Result<bool, DbError>;
    /// Cancels setup and returns whether its active worker owns cleanup.
    /// The claim races atomically with the worker's completion transition.
    async fn cancel_session_preparation(&self, session_id: &str) -> Result<bool, DbError>;
    /// Returns queue or execution evidence, excluding failed operations that
    /// never started and remain retryable.
    async fn preparation_prompt_operation_status(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError>;
    /// Removes a failed handoff only when execution never began, allowing
    /// its stable operation identifier to be submitted again.
    async fn reclaim_preparation_prompt_operation(&self, session_id: &str) -> Result<(), DbError>;
    /// Atomically records execution start, persists the user transcript, and
    /// acknowledges the saved payload and ends draft staging. Initial prompts
    /// reuse an existing matching transcript row; replies always append.
    /// Returns false if cancellation or completion already won.
    async fn begin_preparation_prompt_operation(
        &self,
        session_id: &str,
        transcript_text: &str,
    ) -> Result<bool, DbError>;
    /// Acknowledges recovered execution of a saved prompt.
    async fn clear_preparation_prompt(&self, session_id: &str) -> Result<(), DbError>;
    /// Makes interrupted setup retryable without automatically replaying a
    /// turn.
    async fn recover_session_preparations(&self) -> Result<(), DbError>;
}

#[async_trait]
impl SessionPreparationRepository for SqliteSessionRepository {
    async fn reserve_session(&self, session: PersistedSessionCreation<'_>) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;
        let session_id = session.id;
        let start_ref = session.base_branch;
        Self::insert_with_draft_mode(&mut *transaction, self.now(), session).await?;
        sqlx::query(
            "INSERT INTO session_preparation (session_id, state, start_ref) VALUES (?, \
             'preparing', ?)",
        )
        .bind(session_id)
        .bind(start_ref)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(())
    }

    async fn insert_session_preparation(
        &self,
        session_id: &str,
        start_ref: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO session_preparation (session_id, state, start_ref) VALUES (?, \
             'preparing', ?) ON CONFLICT(session_id) DO NOTHING",
        )
        .bind(session_id)
        .bind(start_ref)
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn load_session_preparation(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionPreparationRow>, DbError> {
        Ok(sqlx::query_as(
            "SELECT session_id, state, start_ref, prompt, error FROM session_preparation WHERE \
             session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.0)
        .await?)
    }

    async fn load_session_preparations(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionPreparationRow>, DbError> {
        Ok(sqlx::query_as(
            "SELECT session_id, state, start_ref, session_preparation.prompt, error FROM \
             session_preparation JOIN session ON session.id = session_id WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_all(&self.0)
        .await?)
    }

    async fn save_preparation_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE session_preparation SET prompt = ? WHERE session_id = ? AND prompt IS NULL \
             AND state != 'canceled'",
        )
        .bind(prompt)
        .bind(session_id)
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn update_session_preparation(
        &self,
        session_id: &str,
        state: SessionPreparationState,
        error: Option<&str>,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE session_preparation SET state = ?, error = ? WHERE session_id = ? AND state \
             != 'canceled'",
        )
        .bind(state)
        .bind(error)
        .bind(session_id)
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn cancel_session_preparation(&self, session_id: &str) -> Result<bool, DbError> {
        let claimed = sqlx::query(
            "UPDATE session_preparation SET state = 'canceled', error = NULL WHERE session_id = ? \
             AND state = 'preparing'",
        )
        .bind(session_id)
        .execute(&self.0)
        .await?
        .rows_affected()
            == 1;
        if !claimed {
            self.update_session_preparation(session_id, SessionPreparationState::Canceled, None)
                .await?;
        }

        Ok(claimed)
    }

    async fn preparation_prompt_operation_status(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT status FROM session_operation WHERE id = ? AND (status != 'failed' OR \
             started_at IS NOT NULL)",
        )
        .bind(format!("workspace:{session_id}"))
        .fetch_optional(&self.0)
        .await?)
    }

    async fn clear_preparation_prompt(&self, session_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE session_preparation SET prompt = NULL WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.0)
            .await?;

        Ok(())
    }

    async fn reclaim_preparation_prompt_operation(&self, session_id: &str) -> Result<(), DbError> {
        sqlx::query(
            "DELETE FROM session_operation WHERE id = ? AND status = 'failed' AND started_at IS \
             NULL",
        )
        .bind(format!("workspace:{session_id}"))
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn begin_preparation_prompt_operation(
        &self,
        session_id: &str,
        transcript_text: &str,
    ) -> Result<bool, DbError> {
        let mut transaction = self.0.begin().await?;
        let operation_id = format!("workspace:{session_id}");
        let operation_kind: Option<String> = sqlx::query_scalar(
            "UPDATE session_operation SET status = 'running', started_at = ?, heartbeat_at = ?, \
             last_error = NULL WHERE id = ? AND status = 'queued' AND cancel_requested = 0 AND \
             EXISTS (SELECT 1 FROM session_preparation WHERE session_id = ? AND state = 'ready' \
             AND prompt IS NOT NULL) RETURNING kind",
        )
        .bind(self.now())
        .bind(self.now())
        .bind(operation_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(operation_kind) = operation_kind.as_deref() {
            let content = stored_message_content(SessionMessageKind::UserPrompt, transcript_text);
            let recorded = operation_kind == "start_prompt"
                && sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM session_message WHERE session_id = ? AND kind = \
                     'user_prompt' AND content = ?)",
                )
                .bind(session_id)
                .bind(&content)
                .fetch_one(&mut *transaction)
                .await?;
            if !recorded {
                SessionMessageStore::append_normalized_in_transaction(
                    &mut transaction,
                    session_id,
                    SessionMessageKind::UserPrompt,
                    &content,
                    self.now(),
                )
                .await?;
            }
            sqlx::query("UPDATE session SET is_draft = 0 WHERE id = ?")
                .bind(session_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("UPDATE session_preparation SET prompt = NULL WHERE session_id = ?")
                .bind(session_id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;

        Ok(operation_kind.is_some())
    }

    async fn recover_session_preparations(&self) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE session_preparation SET state = 'failed', error = 'Workspace setup was \
             interrupted. Your saved prompt is retained.' WHERE state = 'preparing' OR (state = \
             'ready' AND prompt IS NOT NULL)",
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "session_preparation_test.rs"]
mod tests;
