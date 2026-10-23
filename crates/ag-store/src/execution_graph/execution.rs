use std::num::NonZeroU64;

use ag_session::{GraphContext, GraphExecutionRef, GraphResolvedInput, GraphResultRef};
use sqlx::SqliteConnection;

use super::{GraphExecutionRecord, SqliteGraphRepository};
use crate::DbError;

impl SqliteGraphRepository {
    pub(super) async fn link(
        connection: &mut SqliteConnection,
        record: &GraphExecutionRecord,
        now: i64,
    ) -> Result<bool, DbError> {
        if let Some((_, existing)) = Self::execution(connection, &record.execution).await? {
            if existing != *record {
                return Err(Self::invalid(
                    "Execution already has different graph inputs or ownership",
                ));
            }

            return Ok(false);
        }
        let graph = Self::current(connection, &record.graph_id, record.graph_revision).await?;
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == record.node_id)
            .ok_or_else(|| Self::invalid("Execution node does not exist"))?;
        match (&node.execution.context, &record.execution) {
            (GraphContext::Fresh, GraphExecutionRef::UtilityRun { .. }) => {}
            (
                GraphContext::TaskConversation { task_id },
                GraphExecutionRef::TaskAttempt {
                    task_id: actual, ..
                },
            ) if task_id == actual => {}
            _ => return Err(Self::invalid("Execution does not match node context")),
        }
        Self::authority(connection, &record.execution, graph.session_id.as_str()).await?;
        if record.inputs.len() != node.inputs.len() {
            return Err(Self::invalid("Execution must capture every declared input"));
        }
        let (attempt_id, run_id) = Self::execution_ids(&record.execution);
        let revision = Self::integer(record.graph_revision)?;
        let reference = serde_json::json!(record.execution).to_string();
        let inserted = sqlx::query!(
            "INSERT INTO execution_graph_execution (execution_graph_id, revision, node_id, \
             task_attempt_id, agent_run_id, reference, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            record.graph_id,
            revision,
            record.node_id,
            attempt_id,
            run_id,
            reference,
            now
        )
        .execute(&mut *connection)
        .await?;
        let execution_id = inserted.last_insert_rowid();
        for (position, input) in (0_i64..).zip(&record.inputs) {
            let stored = Self::result(connection, &input.result.id)
                .await?
                .ok_or_else(|| Self::invalid("Input result does not exist"))?;
            if stored.result.reference != input.result {
                return Err(Self::invalid(
                    "Input must match its exact persisted reference",
                ));
            }
            graph
                .bind_input(&record.node_id, &input.name, &stored.result)
                .map_err(|error| Self::invalid(error.to_string()))?;
            sqlx::query!(
                "INSERT INTO execution_graph_input (execution_graph_execution_id, name, position, \
                 execution_graph_result_id) VALUES (?, ?, ?, ?)",
                execution_id,
                input.name,
                position,
                input.result.id
            )
            .execute(&mut *connection)
            .await?;
        }

        Ok(true)
    }

    pub(super) async fn execution(
        connection: &mut SqliteConnection,
        execution: &GraphExecutionRef,
    ) -> Result<Option<(i64, GraphExecutionRecord)>, DbError> {
        execution
            .validate()
            .map_err(|error| Self::invalid(error.to_string()))?;
        let (attempt_id, run_id) = Self::execution_ids(execution);
        let row = sqlx::query!(
            "SELECT id, execution_graph_id, revision, node_id, reference FROM \
             execution_graph_execution WHERE task_attempt_id = ? OR agent_run_id = ?",
            attempt_id,
            run_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let reference: GraphExecutionRef = serde_json::from_str(&row.reference)
            .map_err(|error| Self::invalid(error.to_string()))?;
        if reference != *execution {
            return Err(Self::invalid(
                "Execution reference differs from stored authority",
            ));
        }
        let rows = sqlx::query!(
            "SELECT i.name, r.reference FROM execution_graph_input i JOIN execution_graph_result \
             r ON r.id = i.execution_graph_result_id WHERE i.execution_graph_execution_id = ? \
             ORDER BY i.position",
            row.id
        )
        .fetch_all(connection)
        .await?;
        let inputs = rows
            .into_iter()
            .map(|row| {
                let result: GraphResultRef = serde_json::from_str(&row.reference)
                    .map_err(|error| Self::invalid(error.to_string()))?;

                Ok(GraphResolvedInput {
                    name: row.name,
                    result,
                })
            })
            .collect::<Result<Vec<_>, DbError>>()?;
        let revision = u64::try_from(row.revision)
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or_else(|| Self::invalid("Invalid persisted graph revision"))?;

        Ok(Some((
            row.id,
            GraphExecutionRecord {
                execution: reference,
                graph_id: row.execution_graph_id,
                graph_revision: revision,
                inputs,
                node_id: row.node_id,
            },
        )))
    }

    /// Verifies record existence and session ownership; returns settlement.
    pub(super) async fn authority(
        connection: &mut SqliteConnection,
        execution: &GraphExecutionRef,
        session_id: &str,
    ) -> Result<bool, DbError> {
        match execution {
            GraphExecutionRef::TaskAttempt {
                attempt_id,
                operation_id,
                task_id,
            } => {
                let row = sqlx::query!(
                    "SELECT t.session_id, a.task_id, a.session_operation_id, o.session_id AS \
                     operation_session_id, o.status, o.cancel_requested FROM task_attempt a JOIN \
                     task t ON t.id = a.task_id JOIN session_operation o ON o.id = \
                     a.session_operation_id WHERE a.id = ?",
                    attempt_id
                )
                .fetch_optional(connection)
                .await?
                .ok_or_else(|| Self::invalid("Task attempt does not exist"))?;
                if row.session_id != session_id
                    || row.operation_session_id != session_id
                    || row.task_id != *task_id
                    || row.session_operation_id != *operation_id
                {
                    return Err(Self::invalid("Task attempt ownership does not match graph"));
                }

                Ok(row.status == "done" && row.cancel_requested == 0)
            }
            GraphExecutionRef::UtilityRun { run_id } => {
                let row = sqlx::query!(
                    "SELECT session_id, status FROM agent_run WHERE id = ?",
                    run_id
                )
                .fetch_optional(connection)
                .await?
                .ok_or_else(|| Self::invalid("Utility run does not exist"))?;
                if row.session_id.as_deref() != Some(session_id) {
                    return Err(Self::invalid("Utility run ownership does not match graph"));
                }

                Ok(row.status == "completed")
            }
        }
    }

    fn execution_ids(execution: &GraphExecutionRef) -> (Option<&str>, Option<&str>) {
        match execution {
            GraphExecutionRef::TaskAttempt { attempt_id, .. } => (Some(attempt_id), None),
            GraphExecutionRef::UtilityRun { run_id } => (None, Some(run_id)),
        }
    }
}
