//! Task-only replay history, continuation, and idempotent usage accounting.

use ag_session::TaskAgentInput;
use sqlx::SqliteConnection;

use crate::task_agent::{TaskAgentContextRow, TaskAgentStore};
use crate::{DbError, session};

/// One observed model response, separate from operation completion.
///
/// Post-processing may still fail after the response was saved. Callers must
/// inspect the producing attempt's operation status before accepting output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentTurnRow {
    /// Host-serialized structured response, never application instructions.
    pub assistant_message: String,
    /// Attempt that produced this exact response.
    pub attempt_id: String,
    /// Explicit input delivered for this attempt, including clarification.
    pub input: TaskAgentInput,
    /// Reported input usage for this model turn.
    pub input_tokens: u64,
    /// Reported output usage for this model turn.
    pub output_tokens: u64,
}

/// Consistent snapshot used to prepare one task-owned continuation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentHistoryRow {
    /// Stable context identity, assignment, and immutable original inputs.
    pub context: TaskAgentContextRow,
    /// Bootstrap marker belonging to this task's provider conversation.
    pub instruction_conversation_id: Option<String>,
    /// Provider-native conversation belonging exclusively to this task.
    pub provider_conversation_id: Option<String>,
    /// Observed responses in attempt-admission order; excludes session chat.
    pub turns: Vec<TaskAgentTurnRow>,
}

/// Observed response and provider state to commit before post-processing.
///
/// The host owns response-schema validation. This adapter stores the exact
/// serialized response and accounts usage under the task's captured model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentTurnMetadata {
    /// Exact host-serialized structured response.
    pub assistant_message: String,
    /// Optional content-free native-harness report.
    pub harness_turn_report: Option<String>,
    /// Task-scoped instruction bootstrap marker.
    pub instruction_conversation_id: Option<String>,
    /// Reported input tokens; must fit SQLite's signed integer range.
    pub input_tokens: u64,
    /// Reported output tokens; must fit SQLite's signed integer range.
    pub output_tokens: u64,
    /// Task-scoped provider conversation to resume on the next attempt.
    pub provider_conversation_id: Option<String>,
}

/// Helpers run inside the task repository's read or write transaction.
pub(crate) struct TaskAgentTurnStore;

impl TaskAgentTurnStore {
    pub(crate) async fn load(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<Option<TaskAgentHistoryRow>, DbError> {
        let Some(context) = TaskAgentStore::load(connection, task_id).await? else {
            return Ok(None);
        };
        let continuation = sqlx::query!(
            "SELECT provider_conversation_id, instruction_conversation_id FROM task_agent_context \
             WHERE task_id = ?",
            task_id
        )
        .fetch_one(&mut *connection)
        .await?;
        let rows = sqlx::query!(
            r#"SELECT a.id AS "attempt_id!", a.message, r.assistant_message,
                      r.input_tokens, r.output_tokens
               FROM task_agent_turn r JOIN task_attempt a ON a.id = r.task_attempt_id
               WHERE a.task_id = ? ORDER BY a.rowid"#,
            task_id
        )
        .fetch_all(&mut *connection)
        .await?;
        let mut turns = Vec::with_capacity(rows.len());
        for row in rows {
            turns.push(TaskAgentTurnRow {
                assistant_message: row.assistant_message,
                attempt_id: row.attempt_id,
                input: serde_json::from_str(&row.message)
                    .map_err(|_| Self::invalid("Invalid task attempt input"))?,
                input_tokens: Self::read_count(row.input_tokens)?,
                output_tokens: Self::read_count(row.output_tokens)?,
            });
        }

        Ok(Some(TaskAgentHistoryRow {
            context,
            instruction_conversation_id: continuation.instruction_conversation_id,
            provider_conversation_id: continuation.provider_conversation_id,
            turns,
        }))
    }

    pub(crate) async fn save(
        connection: &mut SqliteConnection,
        attempt_id: &str,
        metadata: &TaskAgentTurnMetadata,
        now: i64,
    ) -> Result<(), DbError> {
        if metadata.assistant_message.trim().is_empty() {
            return Err(Self::invalid("Task response must be nonempty"));
        }
        let input_tokens = Self::write_count(metadata.input_tokens)?;
        let output_tokens = Self::write_count(metadata.output_tokens)?;
        // Acquire the writer lock before testing lifecycle or idempotency.
        let inserted = sqlx::query!(
            r#"INSERT INTO task_agent_turn (
                   task_attempt_id, assistant_message, input_tokens, output_tokens,
                   provider_conversation_id, instruction_conversation_id, harness_turn_report
               ) SELECT a.id, ?, ?, ?, ?, ?, ? FROM task_attempt a
                 JOIN task_agent_context c ON c.task_id = a.task_id
                 JOIN session_operation o ON o.id = a.session_operation_id
               WHERE a.id = ? AND o.status = 'running' AND o.cancel_requested = 0
               ON CONFLICT(task_attempt_id) DO NOTHING"#,
            metadata.assistant_message,
            input_tokens,
            output_tokens,
            metadata.provider_conversation_id,
            metadata.instruction_conversation_id,
            metadata.harness_turn_report,
            attempt_id
        )
        .execute(&mut *connection)
        .await?;
        if inserted.rows_affected() == 0 {
            let row = sqlx::query!(
                "SELECT assistant_message, input_tokens, output_tokens, provider_conversation_id, \
                 instruction_conversation_id, harness_turn_report FROM task_agent_turn WHERE \
                 task_attempt_id = ?",
                attempt_id
            )
            .fetch_optional(&mut *connection)
            .await?;
            let Some(row) = row else {
                return Err(Self::invalid(
                    "Task attempt is not running or has no assigned context",
                ));
            };
            let recorded = TaskAgentTurnMetadata {
                assistant_message: row.assistant_message,
                harness_turn_report: row.harness_turn_report,
                instruction_conversation_id: row.instruction_conversation_id,
                input_tokens: Self::read_count(row.input_tokens)?,
                output_tokens: Self::read_count(row.output_tokens)?,
                provider_conversation_id: row.provider_conversation_id,
            };
            if recorded != *metadata {
                return Err(Self::invalid(
                    "Task response was already recorded with different output",
                ));
            }

            // An old response must never rewind a newer task continuation.
            return Ok(());
        }
        let owner = sqlx::query!(
            r#"SELECT t.session_id, c.model, a.task_id FROM task_attempt a
               JOIN task t ON t.id = a.task_id
               JOIN task_agent_context c ON c.task_id = a.task_id WHERE a.id = ?"#,
            attempt_id
        )
        .fetch_one(&mut *connection)
        .await?;
        sqlx::query!(
            "UPDATE task_agent_context SET provider_conversation_id = ?, \
             instruction_conversation_id = ? WHERE task_id = ?",
            metadata.provider_conversation_id,
            metadata.instruction_conversation_id,
            owner.task_id
        )
        .execute(&mut *connection)
        .await?;
        session::persist_turn_usage(
            connection,
            &owner.session_id,
            &owner.model,
            now,
            metadata.input_tokens,
            metadata.output_tokens,
            metadata.harness_turn_report.as_deref(),
        )
        .await?;

        Ok(())
    }

    fn write_count(value: u64) -> Result<i64, DbError> {
        i64::try_from(value).map_err(|_| Self::invalid("Task usage exceeds SQLite's integer range"))
    }

    fn read_count(value: i64) -> Result<u64, DbError> {
        u64::try_from(value).map_err(|_| Self::invalid("Task usage must be nonnegative"))
    }

    fn invalid(reason: &str) -> DbError {
        DbError::InvalidData {
            entity: "task agent turn",
            reason: reason.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "task_agent_turn_test.rs"]
mod tests;
