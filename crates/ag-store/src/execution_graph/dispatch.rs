use std::num::NonZeroU64;
use std::path::Path;

use ag_contracts::OneShotError;
use ag_protocol::parse_protocol_response_strict;
use ag_session::{
    DurableMessageVisibility, ExecutionGraph, GraphCompletionGate, GraphContext, GraphExecutionRef,
    GraphNode, GraphResultRef, GraphResultState, GraphWorkspaceAccess, SubmitTaskRequest,
};
use ag_worker::{
    GraphAdmission, GraphDispatch, GraphDispatchRepository, GraphExecutionRecord, GraphInputValue,
};
use async_trait::async_trait;
use sqlx::SqliteConnection;

use super::{GraphEvidence, SqliteGraphRepository, StoredGraphResult};
use crate::DbError;
use crate::task::SqliteTaskRepository;

#[async_trait]
impl GraphDispatchRepository for SqliteGraphRepository {
    async fn graph(&self, id: &str, revision: NonZeroU64) -> Result<ExecutionGraph, OneShotError> {
        let mut connection = self.pool.acquire().await.map_err(Self::dispatch_error)?;

        Self::current(&mut connection, id, revision)
            .await
            .map_err(Self::dispatch_error)
    }

    async fn claim(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        folder: &Path,
    ) -> Result<GraphAdmission, OneShotError> {
        self.claim_node(graph_id, revision, node_id, execution_id, folder)
            .await
            .map_err(Self::dispatch_error)
    }

    async fn finish(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), OneShotError> {
        self.finish_node(dispatch, body)
            .await
            .map_err(Self::dispatch_error)
    }

    async fn fail(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), OneShotError> {
        self.fail_node(record, error)
            .await
            .map_err(Self::dispatch_error)
    }
}

impl SqliteGraphRepository {
    fn dispatch_error(error: impl std::fmt::Display) -> OneShotError {
        OneShotError::new(format!("Graph dispatch failed: {error}"))
    }

    async fn claim_node(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        node_id: &str,
        execution_id: &str,
        folder: &Path,
    ) -> Result<GraphAdmission, DbError> {
        if execution_id.trim().is_empty() || folder.as_os_str().is_empty() {
            return Err(Self::invalid(
                "Dispatch requires an execution identity and Env folder",
            ));
        }
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, graph_id).await?;
        let graph = Self::current(&mut transaction, graph_id, revision).await?;
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| Self::invalid("Dispatch node does not exist"))?;
        if let Some(record) = Self::node_execution(&mut transaction, &graph, node_id).await? {
            return Ok(GraphAdmission::Existing(record));
        }
        let Some(inputs) = Self::ready_inputs(&mut transaction, &graph, node).await? else {
            return Ok(GraphAdmission::Blocked);
        };
        if Self::env_busy(&mut transaction, &graph, node).await? {
            return Ok(GraphAdmission::Blocked);
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let execution = match &node.execution.context {
            GraphContext::Fresh => {
                let purpose = format!("graph:{}", node.id);
                let inserted = sqlx::query(
                    "INSERT INTO agent_run (id, parent_id, session_id, project_id, folder, \
                     purpose, status, queued_at) SELECT ?, ?, ?, (SELECT project_id FROM session \
                     WHERE id = ?), ?, ?, 'queued', ? WHERE NOT EXISTS (SELECT 1 FROM \
                     agent_run_closed_session WHERE session_id = ?)",
                )
                .bind(execution_id)
                .bind(&graph.id)
                .bind(graph.session_id.as_str())
                .bind(graph.session_id.as_str())
                .bind(folder.to_string_lossy().as_ref())
                .bind(purpose)
                .bind(now)
                .bind(graph.session_id.as_str())
                .execute(&mut *transaction)
                .await?;
                if inserted.rows_affected() != 1 {
                    return Err(Self::invalid("Session utility admission is closed"));
                }
                GraphExecutionRef::UtilityRun {
                    run_id: execution_id.into(),
                }
            }
            GraphContext::TaskConversation { task_id } => {
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task WHERE id = ?)")
                        .bind(task_id)
                        .fetch_one(&mut *transaction)
                        .await?;
                if exists {
                    return Err(Self::invalid("Graph task identity is already in use"));
                }
                let request = SubmitTaskRequest {
                    attempt_id: execution_id.into(),
                    message: serde_json::json!(node.request).to_string(),
                    objective: node.objective.clone(),
                    task_id: task_id.clone(),
                    visibility: DurableMessageVisibility::Hidden,
                };
                SqliteTaskRepository::claim_attempt_in_transaction(
                    &mut transaction,
                    graph.session_id.as_str(),
                    &request,
                    "graph_node",
                    None,
                    now,
                )
                .await?;
                GraphExecutionRef::TaskAttempt {
                    attempt_id: execution_id.into(),
                    operation_id: request.operation_id(),
                    task_id: task_id.clone(),
                }
            }
        };
        let record = GraphExecutionRecord {
            execution,
            graph_id: graph.id.clone(),
            graph_revision: graph.revision,
            inputs: inputs.iter().map(|input| input.reference.clone()).collect(),
            node_id: node.id.clone(),
        };
        Self::link(&mut transaction, &record, now).await?;
        Self::mark_dispatch_claim(&mut transaction, &record.execution).await?;
        transaction.commit().await?;

        Ok(GraphAdmission::Claimed(Box::new(GraphDispatch {
            inputs,
            node: node.clone(),
            record,
        })))
    }

    async fn mark_dispatch_claim(
        connection: &mut SqliteConnection,
        execution: &GraphExecutionRef,
    ) -> Result<(), DbError> {
        let reference = serde_json::json!(execution).to_string();
        sqlx::query(
            "UPDATE execution_graph_execution SET is_dispatch_claimed = 1 WHERE reference = ?",
        )
        .bind(reference)
        .execute(connection)
        .await?;

        Ok(())
    }

    async fn node_execution(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        node_id: &str,
    ) -> Result<Option<GraphExecutionRecord>, DbError> {
        let reference: Option<String> = sqlx::query_scalar(
            "SELECT reference FROM execution_graph_execution WHERE execution_graph_id = ? AND \
             revision = ? AND node_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&graph.id)
        .bind(Self::integer(graph.revision)?)
        .bind(node_id)
        .fetch_optional(&mut *connection)
        .await?;
        let Some(reference) = reference else {
            return Ok(None);
        };
        let reference =
            serde_json::from_str(&reference).map_err(|error| Self::invalid(error.to_string()))?;

        Ok(Self::execution(connection, &reference)
            .await?
            .map(|(_, record)| record))
    }

    async fn ready_inputs(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        node: &GraphNode,
    ) -> Result<Option<Vec<GraphInputValue>>, DbError> {
        for dependency in &node.dependencies {
            let Some(record) = Self::node_execution(connection, graph, &dependency.node_id).await?
            else {
                return Ok(None);
            };
            let satisfied = match &dependency.gate {
                GraphCompletionGate::ExecutionSucceeded => {
                    Self::settled_response(connection, graph, &record).await?
                }
                GraphCompletionGate::ResultValidated { output_name } => Self::eligible_result(
                    connection,
                    graph,
                    &dependency.node_id,
                    output_name,
                    false,
                )
                .await?
                .is_some(),
                GraphCompletionGate::ResultAccepted { output_name } => {
                    Self::eligible_result(connection, graph, &dependency.node_id, output_name, true)
                        .await?
                        .is_some()
                }
            };
            if !satisfied {
                return Ok(None);
            }
        }
        let mut inputs = Vec::new();
        for input in &node.inputs {
            let accepted = node.dependencies.iter().any(|dependency| {
                dependency.node_id == input.producer_node_id && matches!(
                    &dependency.gate, GraphCompletionGate::ResultAccepted { output_name } if output_name == &input.output_name
                )
            });
            let Some(result) = Self::eligible_result(
                connection,
                graph,
                &input.producer_node_id,
                &input.output_name,
                accepted,
            )
            .await?
            else {
                return Ok(None);
            };
            let reference = graph
                .bind_input(&node.id, &input.name, &result.result)
                .map_err(|error| Self::invalid(error.to_string()))?;
            inputs.push(GraphInputValue {
                body: result.body,
                reference,
            });
        }

        Ok(Some(inputs))
    }

    async fn settled_response(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        record: &GraphExecutionRecord,
    ) -> Result<bool, DbError> {
        if !Self::authority(connection, &record.execution, graph.session_id.as_str()).await? {
            return Ok(false);
        }
        let (id, _) = Self::execution(connection, &record.execution)
            .await?
            .ok_or_else(|| Self::invalid("Missing execution"))?;
        let body: Option<String> = sqlx::query_scalar(
            "SELECT body FROM execution_graph_response WHERE execution_graph_execution_id = ?",
        )
        .bind(id)
        .fetch_optional(connection)
        .await?;
        let Some(body) = body else { return Ok(false) };
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == record.node_id)
            .ok_or_else(|| Self::invalid("Missing node"))?;
        let response = parse_protocol_response_strict(&body, node.execution.response_profile)
            .map_err(|error| Self::invalid(error.to_string()))?;

        Ok(response.questions.is_empty())
    }

    async fn eligible_result(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        node_id: &str,
        output: &str,
        accepted: bool,
    ) -> Result<Option<StoredGraphResult>, DbError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT r.id, r.reference, r.body FROM execution_graph_result r JOIN \
             execution_graph_execution e ON e.id = r.execution_graph_execution_id WHERE \
             e.execution_graph_id = ? AND e.revision = ? AND e.node_id = ? AND r.output_name = ? \
             ORDER BY r.version DESC",
        )
        .bind(&graph.id)
        .bind(Self::integer(graph.revision)?)
        .bind(node_id)
        .bind(output)
        .fetch_all(&mut *connection)
        .await?;
        for (id, reference, body) in rows {
            let result = Self::decode_result(connection, &id, &reference, body).await?;
            if (result.result.state == GraphResultState::Accepted
                || (!accepted && result.result.state == GraphResultState::Validated))
                && Self::authority(
                    connection,
                    &result.result.reference.execution,
                    graph.session_id.as_str(),
                )
                .await?
            {
                return Ok(Some(result));
            }
        }

        Ok(None)
    }

    /// Durable graph claims exclude conflicting active work across graphs.
    /// Other host work must also use the shared `GraphResources` Env lease.
    async fn env_busy(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        node: &GraphNode,
    ) -> Result<bool, DbError> {
        let active: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT e.reference FROM session_operation o LEFT JOIN task_attempt a ON \
             a.session_operation_id = o.id LEFT JOIN execution_graph_execution e ON \
             e.task_attempt_id = a.id WHERE o.session_id = ? AND o.status IN ('queued', \
             'running') UNION ALL SELECT e.reference FROM agent_run r LEFT JOIN \
             execution_graph_execution e ON e.agent_run_id = r.id WHERE r.session_id = ? AND \
             r.status IN ('queued', 'running')",
        )
        .bind(graph.session_id.as_str())
        .bind(graph.session_id.as_str())
        .fetch_all(&mut *connection)
        .await?;
        for reference in active {
            if node.execution.workspace_access == GraphWorkspaceAccess::ExclusiveWrite {
                return Ok(true);
            }
            let Some(reference) = reference else {
                return Ok(true);
            };
            let execution = serde_json::from_str(&reference)
                .map_err(|error| Self::invalid(error.to_string()))?;
            let (_, record) = Self::execution(connection, &execution)
                .await?
                .ok_or_else(|| Self::invalid("Missing active execution"))?;
            let other = Self::current(connection, &record.graph_id, record.graph_revision).await?;
            let writer = other.nodes.iter().any(|candidate| {
                candidate.id == record.node_id
                    && candidate.execution.workspace_access == GraphWorkspaceAccess::ExclusiveWrite
            });
            if writer {
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn finish_node(&self, dispatch: &GraphDispatch, body: &str) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        let record = &dispatch.record;
        Self::lock(&mut transaction, &record.graph_id).await?;
        let graph =
            Self::current(&mut transaction, &record.graph_id, record.graph_revision).await?;
        let (id, existing) = Self::execution(&mut transaction, &record.execution)
            .await?
            .ok_or_else(|| Self::invalid("Missing admitted execution"))?;
        if existing != *record || !graph.nodes.iter().any(|node| node == &dispatch.node) {
            return Err(Self::invalid("Settlement differs from admitted execution"));
        }
        if !Self::authority(
            &mut transaction,
            &record.execution,
            graph.session_id.as_str(),
        )
        .await?
        {
            return Err(Self::invalid(
                "Graph response requires successful authoritative settlement",
            ));
        }
        let response =
            parse_protocol_response_strict(body, dispatch.node.execution.response_profile)
                .map_err(|error| Self::invalid(error.to_string()))?;
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT body FROM execution_graph_response WHERE execution_graph_execution_id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(previous) = previous {
            if previous != body {
                return Err(Self::invalid("Execution response is immutable"));
            }
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO execution_graph_response (execution_graph_execution_id, body) VALUES (?, \
             ?)",
        )
        .bind(id)
        .bind(body)
        .execute(&mut *transaction)
        .await?;
        let now = self.timestamp_source.now_timestamp_seconds();
        for output in &dispatch.node.outputs {
            let version: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(version), 0) FROM execution_graph_result WHERE \
                 execution_graph_id = ? AND node_id = ? AND output_name = ?",
            )
            .bind(&record.graph_id)
            .bind(&record.node_id)
            .bind(&output.name)
            .fetch_one(&mut *transaction)
            .await?;
            let reference = GraphResultRef {
                execution: record.execution.clone(),
                graph_id: record.graph_id.clone(),
                graph_revision: record.graph_revision,
                id: format!("graph-response:{id}:{}", output.name),
                node_id: record.node_id.clone(),
                output_name: output.name.clone(),
                schema: output.schema,
                version: version
                    .checked_add(1)
                    .and_then(|version| u64::try_from(version).ok())
                    .and_then(NonZeroU64::new)
                    .ok_or_else(|| Self::invalid("Invalid result version"))?,
            };
            Self::produce(&mut transaction, &reference, body, now).await?;
            if response.questions.is_empty() {
                let evidence = GraphEvidence {
                    actor: "static-graph-executor".into(),
                    reason: "Schema-valid question-free settled response".into(),
                };
                sqlx::query(
                    "INSERT INTO execution_graph_evidence (execution_graph_result_id, kind, \
                     actor, reason, recorded_at) VALUES (?, 'validation', ?, ?, ?)",
                )
                .bind(&reference.id)
                .bind(evidence.actor)
                .bind(evidence.reason)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            }
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn fail_node(&self, record: &GraphExecutionRecord, error: &str) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, &record.graph_id).await?;
        let (_, existing) = Self::execution(&mut transaction, &record.execution)
            .await?
            .ok_or_else(|| Self::invalid("Missing admitted execution"))?;
        if existing != *record {
            return Err(Self::invalid("Failure differs from admitted execution"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        match &record.execution {
            GraphExecutionRef::UtilityRun { run_id } => {
                sqlx::query(
                    "UPDATE agent_run SET status = 'failed', finished_at = ?, last_error = ? \
                     WHERE id = ? AND status IN ('queued', 'running')",
                )
                .bind(now)
                .bind(error)
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            }
            GraphExecutionRef::TaskAttempt { operation_id, .. } => {
                sqlx::query(
                    "UPDATE session_operation SET status = 'failed', finished_at = ?, last_error \
                     = ? WHERE id = ? AND status IN ('queued', 'running')",
                )
                .bind(now)
                .bind(error)
                .bind(operation_id)
                .execute(&mut *transaction)
                .await?;
            }
        }
        transaction.commit().await?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "dispatch_invariant_test.rs"]
mod tests;
