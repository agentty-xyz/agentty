use std::fmt::Write as _;

use ag_protocol::parse_protocol_response_strict;
use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, GraphContext, GraphControlRequest, GraphExecutionRef,
    GraphNodeContinuation,
};
use sqlx::SqliteConnection;

use super::{GraphExecutionRecord, SqliteGraphRepository};
use crate::DbError;

type HistoryRow = (i64, Option<String>, Option<String>, Option<String>);

impl SqliteGraphRepository {
    pub(super) async fn cancelled(
        connection: &mut SqliteConnection,
        graph_id: &str,
    ) -> Result<bool, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM execution_graph_cancellation WHERE execution_graph_id = \
             ?)",
        )
        .bind(graph_id)
        .fetch_one(connection)
        .await?)
    }

    pub(super) async fn ensure_live_execution(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        record: &GraphExecutionRecord,
    ) -> Result<(), DbError> {
        if Self::cancelled(connection, &graph.id).await?
            || (Self::is_dispatch_claimed(connection, &record.execution).await?
                && Self::node_execution(connection, graph, &record.node_id)
                    .await?
                    .as_ref()
                    != Some(record))
        {
            return Err(Self::invalid("Canceled graph or stale execution"));
        }
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM execution_graph_continuation c JOIN \
             execution_graph_execution e ON e.id = c.source_execution_graph_execution_id WHERE \
             e.reference = ? AND c.execution_graph_execution_id IS NULL)",
        )
        .bind(serde_json::json!(record.execution).to_string())
        .fetch_one(connection)
        .await?;
        if pending {
            return Err(Self::invalid(
                "Execution was superseded by explicit continuation intent",
            ));
        }

        Ok(())
    }

    async fn is_dispatch_claimed(
        connection: &mut SqliteConnection,
        execution: &GraphExecutionRef,
    ) -> Result<bool, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT is_dispatch_claimed FROM execution_graph_execution WHERE reference = ?",
        )
        .bind(serde_json::json!(execution).to_string())
        .fetch_one(connection)
        .await?)
    }

    pub(super) async fn cancel_graph(&self, request: &GraphControlRequest) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, &request.graph_id).await?;
        let graph = Self::current(&mut transaction, &request.graph_id, request.revision).await?;
        let mut completed = true;
        for node in &graph.nodes {
            let Some(record) = Self::node_execution(&mut transaction, &graph, &node.id).await?
            else {
                completed = false;
                break;
            };
            if !Self::settled_response(&mut transaction, &graph, &record).await? {
                completed = false;
                break;
            }
        }
        if completed {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO execution_graph_cancellation (execution_graph_id, recorded_at) VALUES \
             (?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(&request.graph_id)
        .bind(self.timestamp_source.now_timestamp_seconds())
        .execute(&mut *transaction)
        .await?;
        // Keep running operations owned by their workers until cleanup settles.
        sqlx::query(
            "UPDATE session_operation SET cancel_requested = 1 WHERE id IN (SELECT \
             a.session_operation_id FROM task_attempt a JOIN execution_graph_execution e ON \
             e.task_attempt_id = a.id WHERE e.execution_graph_id = ?) AND status IN ('queued', \
             'running')",
        )
        .bind(&request.graph_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(())
    }

    pub(super) async fn continue_graph_node(
        &self,
        request: &ContinueGraphNodeRequest,
    ) -> Result<bool, DbError> {
        // Task contexts use the attempt UUID as their runtime identity.
        if uuid::Uuid::parse_str(&request.action_id).is_err() {
            return Err(Self::invalid("Continuation requires a new execution UUID"));
        }
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, &request.graph_id).await?;
        let graph = Self::current(&mut transaction, &request.graph_id, request.revision).await?;
        let encoded = serde_json::json!(request).to_string();
        let existing: Option<String> =
            sqlx::query_scalar("SELECT request FROM execution_graph_continuation WHERE id = ?")
                .bind(&request.action_id)
                .fetch_optional(&mut *transaction)
                .await?;
        if let Some(existing) = existing {
            if existing != encoded {
                return Err(Self::invalid(
                    "Continuation identity has conflicting inputs",
                ));
            }

            return Ok(false);
        }
        let reused: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM task_attempt WHERE id = ? UNION ALL SELECT 1 FROM \
             agent_run WHERE id = ?)",
        )
        .bind(&request.action_id)
        .bind(&request.action_id)
        .fetch_one(&mut *transaction)
        .await?;
        if reused {
            return Err(Self::invalid(
                "Continuation execution identity is already in use",
            ));
        }
        let (id, record) = Self::execution(&mut transaction, &request.execution)
            .await?
            .ok_or_else(|| Self::invalid("Continuation source is missing"))?;
        if record.graph_id != graph.id
            || record.graph_revision != graph.revision
            || record.node_id != request.node_id
        {
            return Err(Self::invalid(
                "Continuation source differs from node or revision",
            ));
        }
        if !Self::is_dispatch_claimed(&mut transaction, &record.execution).await? {
            return Err(Self::invalid(
                "Continuation requires an admitted graph execution",
            ));
        }
        Self::ensure_live_execution(&mut transaction, &graph, &record).await?;
        let prompt =
            Self::continuation_prompt(&mut transaction, &graph, &record, id, &request.kind).await?;
        sqlx::query(
            "INSERT INTO execution_graph_continuation (id, execution_graph_id, revision, node_id, \
             source_execution_graph_execution_id, request, prompt) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&request.action_id)
        .bind(&graph.id)
        .bind(Self::integer(graph.revision)?)
        .bind(&request.node_id)
        .bind(id)
        .bind(encoded)
        .bind(prompt)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    async fn continuation_prompt(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        record: &GraphExecutionRecord,
        id: i64,
        kind: &GraphNodeContinuation,
    ) -> Result<String, DbError> {
        let (status, _) = Self::execution_state(connection, &record.execution).await?;
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == record.node_id)
            .ok_or_else(|| Self::invalid("Continuation node is missing"))?;
        let prompt = match kind {
            GraphNodeContinuation::Retry => {
                let body = Self::response_body(connection, id).await?;
                if !(matches!(status.as_str(), "failed" | "canceled")
                    || (matches!(status.as_str(), "done" | "completed") && body.is_none()))
                {
                    return Err(Self::invalid(
                        "Retry requires a failed or interrupted execution",
                    ));
                }

                "Explicitly retry this node's original request. Retain its exact bound inputs."
                    .to_string()
            }
            GraphNodeContinuation::Answer { answers } => {
                if !matches!(
                    node.execution.context,
                    GraphContext::TaskConversation { .. }
                ) || !Self::authority(connection, &record.execution, graph.session_id.as_str())
                    .await?
                {
                    return Err(Self::invalid(
                        "Clarification requires a settled task conversation",
                    ));
                }
                let body = Self::response_body(connection, id)
                    .await?
                    .ok_or_else(|| Self::invalid("Clarification response is missing"))?;
                let response =
                    parse_protocol_response_strict(&body, node.execution.response_profile)
                        .map_err(|error| Self::invalid(error.to_string()))?;
                if answers.is_empty()
                    || answers.len() != response.questions.len()
                    || answers
                        .iter()
                        .zip(&response.questions)
                        .any(|(answer, question)| {
                            answer.question != question.text || answer.answer.trim().is_empty()
                        })
                {
                    return Err(Self::invalid(
                        "Reply must answer the exact complete ordered question set",
                    ));
                }

                format!(
                    "Explicit clarification answers (JSON data):\n{}",
                    serde_json::json!(answers)
                )
            }
        };
        Ok(prompt)
    }

    pub(super) async fn response_body(
        connection: &mut SqliteConnection,
        execution_id: i64,
    ) -> Result<Option<String>, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT body FROM execution_graph_response WHERE execution_graph_execution_id = ?",
        )
        .bind(execution_id)
        .fetch_optional(connection)
        .await?)
    }

    pub(super) async fn execution_state(
        connection: &mut SqliteConnection,
        execution: &GraphExecutionRef,
    ) -> Result<(String, Option<String>), DbError> {
        let (query, id) = match execution {
            GraphExecutionRef::TaskAttempt { operation_id, .. } => (
                "SELECT status, last_error FROM session_operation WHERE id = ?",
                operation_id,
            ),
            GraphExecutionRef::UtilityRun { run_id } => (
                "SELECT status, last_error FROM agent_run WHERE id = ?",
                run_id,
            ),
        };

        Ok(sqlx::query_as(query).bind(id).fetch_one(connection).await?)
    }

    pub(super) async fn node_history(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<String>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let Some((current_id, record)) = Self::execution(&mut transaction, execution).await? else {
            return Ok(None);
        };
        let rows: Vec<HistoryRow> = sqlx::query_as(
            "SELECT e.id, c.prompt, r.body, t.metadata FROM execution_graph_execution e LEFT JOIN \
             execution_graph_continuation c ON c.execution_graph_execution_id = e.id LEFT JOIN \
             execution_graph_response r ON r.execution_graph_execution_id = e.id LEFT JOIN \
             execution_graph_task_turn t ON t.execution_graph_execution_id = e.id WHERE \
             e.execution_graph_id = ? AND e.revision = ? AND e.node_id = ? AND e.id <= ? ORDER BY \
             e.id",
        )
        .bind(&record.graph_id)
        .bind(Self::integer(record.graph_revision)?)
        .bind(&record.node_id)
        .bind(current_id)
        .fetch_all(&mut *transaction)
        .await?;
        let mut history = String::new();
        for (id, prompt, body, metadata) in rows {
            if let Some(prompt) = prompt {
                let _ = writeln!(history, "User: {prompt}");
            }
            if id == current_id {
                break;
            }
            if let Some(body) = body {
                let _ = writeln!(history, "Assistant (protocol data): {body}");
            } else if let Some(metadata) = metadata {
                let metadata: serde_json::Value = serde_json::from_str(&metadata)
                    .map_err(|error| Self::invalid(error.to_string()))?;
                let _ = writeln!(
                    history,
                    "Assistant (observed before failed effects): {}",
                    metadata[0]
                );
            }
        }
        transaction.commit().await?;

        Ok((!history.is_empty()).then_some(history))
    }
}
