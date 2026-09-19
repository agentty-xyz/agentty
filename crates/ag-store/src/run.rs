use std::sync::Arc;

use ag_contracts::OneShotError;
use ag_worker::{RunInfo, RunRepository, RunState};
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::TimestampSource;

pub(crate) struct SqliteRunRepository {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteRunRepository {
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    fn error(error: &sqlx::Error) -> OneShotError {
        OneShotError::new(format!("Agent run persistence failed: {error}"))
    }
}

#[async_trait]
impl RunRepository for SqliteRunRepository {
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError> {
        let inserted = sqlx::query(
            "INSERT INTO agent_run (id, parent_id, session_id, project_id, folder, purpose, \
             status, queued_at) SELECT ?, ?, ?, COALESCE(?, (SELECT project_id FROM session WHERE \
             id = ?)), ?, ?, 'queued', ? WHERE NOT EXISTS (SELECT 1 FROM agent_run_closed_session \
             WHERE session_id = ?)",
        )
        .bind(&run.id)
        .bind(&run.parent_id)
        .bind(&run.session_id)
        .bind(run.project_id)
        .bind(&run.session_id)
        .bind(run.folder.to_string_lossy().as_ref())
        .bind(&run.purpose)
        .bind(self.timestamp_source.now_timestamp_seconds())
        .bind(&run.session_id)
        .execute(&self.pool)
        .await
        .map_err(|error| Self::error(&error))?;
        if inserted.rows_affected() == 0 {
            return Err(OneShotError::new("[Stopped] Session agent runs are closed"));
        }

        Ok(())
    }

    async fn close_session(&self, session_id: &str) -> Result<(), OneShotError> {
        sqlx::query("INSERT OR IGNORE INTO agent_run_closed_session (session_id) VALUES (?)")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|error| Self::error(&error))?;

        Ok(())
    }

    async fn transition(
        &self,
        id: &str,
        state: RunState,
        error: Option<&str>,
    ) -> Result<(), OneShotError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        sqlx::query(
            "UPDATE agent_run SET status = ?, started_at = CASE WHEN ? = 'running' THEN ? ELSE \
             started_at END, heartbeat_at = ?, finished_at = CASE WHEN ? IN ('completed', \
             'failed', 'canceled') THEN ? ELSE finished_at END, last_error = ? WHERE id = ? AND \
             status IN ('queued', 'running') AND (? != 'running' OR status = 'queued') AND ? != \
             'queued'",
        )
        .bind(state.as_str())
        .bind(state.as_str())
        .bind(now)
        .bind(now)
        .bind(state.as_str())
        .bind(now)
        .bind(error)
        .bind(id)
        .bind(state.as_str())
        .bind(state.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| Self::error(&error))?;

        Ok(())
    }

    async fn heartbeat(&self, id: &str) -> Result<(), OneShotError> {
        sqlx::query("UPDATE agent_run SET heartbeat_at = ? WHERE id = ? AND status = 'running'")
            .bind(self.timestamp_source.now_timestamp_seconds())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|error| Self::error(&error))?;

        Ok(())
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        sqlx::query(
            "UPDATE agent_run SET status = 'failed', finished_at = ?, last_error = 'Interrupted \
             by application restart' WHERE status IN ('queued', 'running')",
        )
        .bind(self.timestamp_source.now_timestamp_seconds())
        .execute(&self.pool)
        .await
        .map_err(|error| Self::error(&error))?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
