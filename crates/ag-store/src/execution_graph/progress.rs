use ag_protocol::parse_protocol_response_strict;
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphBlockedReason,
    GraphExecutionRef, GraphNode, GraphNodeProgress, GraphNodeStatus,
};
use sqlx::SqliteConnection;

use super::SqliteGraphRepository;
use crate::DbError;

impl SqliteGraphRepository {
    pub(super) async fn graph_progress(
        &self,
        graph_id: &str,
    ) -> Result<Option<ExecutionGraphProgress>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let Some(graph) = Self::graph(&mut transaction, graph_id, None).await? else {
            return Ok(None);
        };
        let progress = Self::progress_in(&mut transaction, &graph).await?;
        transaction.commit().await?;

        Ok(Some(progress))
    }

    /// Projects one loaded graph inside the caller's snapshot.
    pub(super) async fn progress_in(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
    ) -> Result<ExecutionGraphProgress, DbError> {
        let cancelled = Self::cancelled(connection, &graph.id).await?;
        let mut nodes = Vec::new();
        for node in &graph.nodes {
            let mut progress = Self::node_progress(connection, graph, node).await?;
            if cancelled && progress.status == GraphNodeStatus::Pending {
                progress.status = GraphNodeStatus::Cancelled;
                progress.blocked.clear();
            }
            nodes.push(progress);
        }
        let status = if cancelled {
            ExecutionGraphStatus::Cancelled
        } else if nodes
            .iter()
            .all(|node| node.status == GraphNodeStatus::Succeeded)
        {
            ExecutionGraphStatus::Completed
        } else if nodes.iter().any(|node| {
            matches!(
                node.status,
                GraphNodeStatus::Failed | GraphNodeStatus::Cancelled
            )
        }) {
            ExecutionGraphStatus::Failed
        } else if nodes.iter().any(|node| {
            matches!(
                node.status,
                GraphNodeStatus::Queued | GraphNodeStatus::Running
            ) || (node.status == GraphNodeStatus::Pending
                && node.blocked == [GraphBlockedReason::AwaitingDispatch])
        }) {
            ExecutionGraphStatus::Running
        } else {
            ExecutionGraphStatus::Blocked
        };

        Ok(ExecutionGraphProgress {
            graph_id: graph.id.clone(),
            nodes,
            revision: graph.revision,
            session_id: graph.session_id.clone(),
            status,
        })
    }

    async fn node_progress(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        node: &GraphNode,
    ) -> Result<GraphNodeProgress, DbError> {
        let references: Vec<String> = sqlx::query_scalar(
            "SELECT reference FROM execution_graph_execution WHERE execution_graph_id = ? AND \
             revision = ? AND node_id = ? ORDER BY id",
        )
        .bind(&graph.id)
        .bind(Self::integer(graph.revision)?)
        .bind(&node.id)
        .fetch_all(&mut *connection)
        .await?;
        let executions = references
            .into_iter()
            .map(|reference| {
                serde_json::from_str::<GraphExecutionRef>(&reference)
                    .map_err(|error| Self::invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut progress = GraphNodeProgress {
            blocked: Vec::new(),
            executions,
            last_error: None,
            node_id: node.id.clone(),
            questions: Vec::new(),
            results: Vec::new(),
            status: GraphNodeStatus::Pending,
        };
        if let Some(execution) = progress.executions.last() {
            let (id, _) = Self::execution(connection, execution)
                .await?
                .ok_or_else(|| Self::invalid("Progress execution is missing"))?;
            let (status, error) = Self::execution_state(connection, execution).await?;
            progress.last_error = error;
            progress.status = match status.as_str() {
                "queued" => GraphNodeStatus::Queued,
                "running" => GraphNodeStatus::Running,
                "canceled" => GraphNodeStatus::Cancelled,
                "done" | "completed" => {
                    if let Some(body) = Self::response_body(connection, id).await? {
                        progress.questions =
                            parse_protocol_response_strict(&body, node.execution.response_profile)
                                .map_err(|error| Self::invalid(error.to_string()))?
                                .questions;
                        if progress.questions.is_empty() {
                            GraphNodeStatus::Succeeded
                        } else {
                            GraphNodeStatus::AwaitingAnswers
                        }
                    } else {
                        progress.last_error = Some(
                            "Execution settled without a graph response; explicit retry required"
                                .into(),
                        );
                        GraphNodeStatus::Failed
                    }
                }
                _ => GraphNodeStatus::Failed,
            };
            let results: Vec<(String, String, String)> = sqlx::query_as(
                "SELECT id, reference, body FROM execution_graph_result WHERE \
                 execution_graph_execution_id = ? ORDER BY version, id",
            )
            .bind(id)
            .fetch_all(&mut *connection)
            .await?;
            for (id, reference, body) in results {
                let result = Self::decode_result(connection, &id, &reference, body).await?;
                progress.results.push(result.result);
            }
            let pending: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM execution_graph_continuation WHERE \
                 source_execution_graph_execution_id = ? AND execution_graph_execution_id IS NULL)",
            )
            .bind(id)
            .fetch_one(&mut *connection)
            .await?;
            if !pending {
                return Ok(progress);
            }
            progress.status = GraphNodeStatus::Pending;
            progress.questions.clear();
        }
        progress.blocked =
            Self::blocked_reasons(connection, graph, node, progress.executions.is_empty()).await?;

        Ok(progress)
    }

    async fn blocked_reasons(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
        node: &GraphNode,
        first_attempt: bool,
    ) -> Result<Vec<GraphBlockedReason>, DbError> {
        let mut reasons = Vec::new();
        // New nodes evaluate all gates. Explicit continuations retain inputs
        // from their source receipt instead of selecting newer results.
        if first_attempt {
            for dependency in &node.dependencies {
                if !Self::dependency_satisfied(connection, graph, dependency).await? {
                    reasons.push(GraphBlockedReason::Dependency {
                        gate: dependency.gate.clone(),
                        node_id: dependency.node_id.clone(),
                    });
                }
            }
        }
        if Self::env_busy(connection, graph, node).await? {
            reasons.push(GraphBlockedReason::EnvironmentBusy);
        }
        if reasons.is_empty() {
            reasons.push(GraphBlockedReason::AwaitingDispatch);
        }

        Ok(reasons)
    }
}

#[cfg(test)]
#[path = "progress_test.rs"]
mod tests;
