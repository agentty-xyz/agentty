//! Append-only, revision-checked graph expansion.

use std::collections::BTreeSet;

use ag_session::{ExecutionGraphStatus, ExpandExecutionGraphRequest};
use sqlx::SqliteConnection;

use super::SqliteGraphRepository;
use crate::DbError;
use crate::connection::begin_write;

impl SqliteGraphRepository {
    /// Records one expansion in its own writer transaction.
    pub(super) async fn record_expansion(
        &self,
        request: &ExpandExecutionGraphRequest,
    ) -> Result<bool, DbError> {
        let mut transaction = begin_write(&self.pool, "expand graph").await?;
        let expanded = Self::expand_in(
            &mut transaction,
            request,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await?;
        transaction.commit().await?;

        Ok(expanded)
    }

    /// Appends `request.nodes` as the next revision inside the caller's
    /// writer transaction. Returns `false` for an identical redelivery.
    ///
    /// The expected revision must still be current, the graph must be
    /// neither canceled nor completed, and appended nodes must spend from the
    /// graph's existing provider-call budgets, so an expansion never adds
    /// credit.
    pub(super) async fn expand_in(
        connection: &mut SqliteConnection,
        request: &ExpandExecutionGraphRequest,
        now: i64,
    ) -> Result<bool, DbError> {
        if request.id.trim().is_empty() {
            return Err(Self::invalid("Graph expansion requires an identity"));
        }
        let graph_id = &request.graph.graph_id;
        let base_revision = Self::integer(request.graph.revision)?;
        let nodes = serde_json::json!(request.nodes).to_string();
        let recorded: Option<(String, i64, String)> = sqlx::query_as(
            "SELECT execution_graph_id, revision, nodes FROM execution_graph_expansion WHERE id = \
             ?",
        )
        .bind(&request.id)
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(recorded) = recorded {
            if recorded != (graph_id.clone(), base_revision.saturating_add(1), nodes) {
                return Err(Self::invalid(
                    "Graph expansion identity was used for a different expansion",
                ));
            }

            return Ok(false);
        }
        let current = Self::graph(&mut *connection, graph_id, None)
            .await?
            .ok_or_else(|| Self::invalid("Graph does not exist"))?;
        if current.revision != request.graph.revision {
            return Err(Self::invalid(
                "Graph expansion must extend the current revision",
            ));
        }
        if Self::cancelled(&mut *connection, graph_id).await? {
            return Err(Self::invalid("Canceled graphs cannot be expanded"));
        }
        if Self::progress_in(&mut *connection, &current).await?.status
            == ExecutionGraphStatus::Completed
        {
            return Err(Self::invalid("Completed graphs cannot be expanded"));
        }
        // Ledgered graphs spend from their recorded budgets; others from the
        // host budgets their existing nodes already share.
        let mut budgets: BTreeSet<String> = sqlx::query_scalar(
            "SELECT budget_id FROM execution_graph_budget WHERE execution_graph_id = ?",
        )
        .bind(graph_id)
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .collect();
        if budgets.is_empty() {
            budgets = current
                .nodes
                .iter()
                .filter_map(|node| node.execution.shared_budget_id.clone())
                .collect();
        }
        let spends_existing_credit = |budget: &Option<String>| match budget {
            Some(id) => budgets.contains(id),
            None => budgets.is_empty(),
        };
        if !request
            .nodes
            .iter()
            .all(|node| spends_existing_credit(&node.execution.shared_budget_id))
        {
            return Err(Self::invalid(
                "Expanded nodes must spend from the graph's existing provider budgets",
            ));
        }
        let expanded = current
            .expand(request.nodes.clone())
            .map_err(|error| Self::invalid(error.to_string()))?;
        let revision = Self::integer(expanded.revision)?;
        sqlx::query("UPDATE execution_graph SET revision = ? WHERE id = ?")
            .bind(revision)
            .bind(graph_id)
            .execute(&mut *connection)
            .await?;
        Self::insert_snapshot(&mut *connection, &expanded, now).await?;
        sqlx::query(
            "INSERT INTO execution_graph_expansion (id, execution_graph_id, revision, nodes, \
             created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&request.id)
        .bind(graph_id)
        .bind(revision)
        .bind(nodes)
        .bind(now)
        .execute(&mut *connection)
        .await?;

        Ok(true)
    }
}
