//! Durable provider-call ledger: limits fixed at first launch and spending
//! recorded as it happens.

use std::collections::BTreeMap;

use ag_session::ExecutionGraph;
use sqlx::SqliteConnection;

use super::SqliteGraphRepository;
use crate::DbError;

impl SqliteGraphRepository {
    /// Saves the snapshot and fixes its limits in one transaction, or checks
    /// that an earlier launch fixed the same limits. Returns remaining calls.
    pub(super) async fn launch_with_budgets(
        &self,
        graph: &ExecutionGraph,
        limits: &BTreeMap<String, usize>,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        Self::save_in(&mut transaction, graph, None, now).await?;
        let remaining = Self::open_budgets(&mut transaction, &graph.id, limits, now).await?;
        transaction.commit().await?;

        Ok(remaining)
    }

    /// Inserts limits for a graph without a ledger, otherwise requires the
    /// same limit set. A graph that executed before durable budgets existed
    /// cannot gain fresh limits, because its earlier spending is unknown.
    pub(super) async fn open_budgets(
        connection: &mut SqliteConnection,
        graph_id: &str,
        limits: &BTreeMap<String, usize>,
        now: i64,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let existing = Self::budgets(&mut *connection, graph_id).await?;
        if !existing.is_empty() {
            let fixed: BTreeMap<_, _> = existing
                .iter()
                .map(|(id, (limit, _))| (id.clone(), *limit))
                .collect();
            if fixed != *limits {
                return Err(Self::invalid(
                    "Graph provider-call limits were fixed at first launch",
                ));
            }

            return Ok(existing
                .into_iter()
                .map(|(id, (limit, spent))| (id, limit.saturating_sub(spent)))
                .collect());
        }
        if limits.is_empty() {
            return Ok(BTreeMap::new());
        }
        let executed: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM execution_graph_execution WHERE execution_graph_id = ? LIMIT 1",
        )
        .bind(graph_id)
        .fetch_optional(&mut *connection)
        .await?;
        if executed.is_some() {
            return Err(Self::invalid(
                "Graph executed before durable provider budgets; cancel it and start a new one",
            ));
        }
        for (budget_id, limit) in limits {
            let limit = i64::try_from(*limit)
                .map_err(|_| Self::invalid("Provider-call limit exceeds SQLite range"))?;
            sqlx::query(
                "INSERT INTO execution_graph_budget (execution_graph_id, budget_id, \
                 provider_call_limit, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(graph_id)
            .bind(budget_id)
            .bind(limit)
            .bind(now)
            .execute(&mut *connection)
            .await?;
        }

        Ok(limits.clone())
    }

    /// Limits fixed at first launch, keyed by budget identity.
    pub(super) async fn budget_limits_for(
        &self,
        graph_id: &str,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let mut connection = self.pool.acquire().await?;

        Ok(Self::budgets(&mut connection, graph_id)
            .await?
            .into_iter()
            .map(|(id, (limit, _))| (id, limit))
            .collect())
    }

    /// Adds spent calls, or subtracts refunds, clamped to the fixed limit.
    pub(super) async fn add_budget_spending(
        &self,
        graph_id: &str,
        budget_id: &str,
        calls: i64,
    ) -> Result<(), DbError> {
        let updated = sqlx::query(
            "UPDATE execution_graph_budget SET spent_provider_call_count = MAX(0, \
             MIN(provider_call_limit, spent_provider_call_count + ?)) WHERE execution_graph_id = \
             ? AND budget_id = ?",
        )
        .bind(calls)
        .bind(graph_id)
        .bind(budget_id)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(Self::invalid("Graph provider budget does not exist"));
        }

        Ok(())
    }

    /// Fixed limit and spent calls per budget identity.
    async fn budgets(
        connection: &mut SqliteConnection,
        graph_id: &str,
    ) -> Result<BTreeMap<String, (usize, usize)>, DbError> {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT budget_id, provider_call_limit, spent_provider_call_count FROM \
             execution_graph_budget WHERE execution_graph_id = ?",
        )
        .bind(graph_id)
        .fetch_all(connection)
        .await?;

        // Table constraints keep both counts nonnegative.
        Ok(rows
            .into_iter()
            .map(|(id, limit, spent)| {
                (
                    id,
                    (
                        usize::try_from(limit).unwrap_or_default(),
                        usize::try_from(spent).unwrap_or_default(),
                    ),
                )
            })
            .collect())
    }
}
