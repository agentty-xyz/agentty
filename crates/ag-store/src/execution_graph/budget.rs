//! Durable provider-call ledger: limits fixed at first launch, explicit
//! recorded extensions, and spending recorded as it happens.

use std::collections::BTreeMap;

use ag_session::{ExecutionGraph, ExtendGraphBudgetRequest};
use sqlx::SqliteConnection;

use super::SqliteGraphRepository;
use crate::DbError;
use crate::connection::begin_write;

/// One budget's stored accounting.
struct BudgetState {
    /// Limit fixed at first launch, before any extension.
    launch_limit: usize,
    /// Launch limit plus every recorded extension.
    limit: usize,
    spent: usize,
}

impl BudgetState {
    fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.spent)
    }
}

impl SqliteGraphRepository {
    /// Saves the snapshot and fixes its limits in one transaction, or checks
    /// that an earlier launch fixed the same limits. Returns remaining calls,
    /// including calls added by recorded extensions.
    pub(super) async fn launch_with_budgets(
        &self,
        graph: &ExecutionGraph,
        limits: &BTreeMap<String, usize>,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = begin_write(&self.pool).await?;
        Self::save_in(&mut transaction, graph, None, now).await?;
        let remaining = Self::open_budgets(&mut transaction, &graph.id, limits, now).await?;
        transaction.commit().await?;

        Ok(remaining)
    }

    /// Inserts limits for a graph without a ledger, otherwise requires the
    /// same launch limit set. A graph that executed before durable budgets
    /// existed cannot gain fresh limits, because its earlier spending is
    /// unknown.
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
                .map(|(id, state)| (id.clone(), state.launch_limit))
                .collect();
            if fixed != *limits {
                return Err(Self::invalid(
                    "Graph provider-call limits were fixed at first launch",
                ));
            }

            return Ok(existing
                .into_iter()
                .map(|(id, state)| (id, state.remaining()))
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

    /// Limits fixed at first launch, keyed by budget identity. Extensions
    /// are excluded, so a relaunch keeps matching the original launch.
    pub(super) async fn budget_limits_for(
        &self,
        graph_id: &str,
    ) -> Result<BTreeMap<String, usize>, DbError> {
        let mut connection = self.pool.acquire().await?;

        Ok(Self::budgets(&mut connection, graph_id)
            .await?
            .into_iter()
            .map(|(id, state)| (id, state.launch_limit))
            .collect())
    }

    /// Records one explicit extension and raises the budget's limit in one
    /// transaction. Returns `false` for an identical redelivery, which adds
    /// nothing.
    pub(super) async fn record_budget_extension(
        &self,
        request: &ExtendGraphBudgetRequest,
    ) -> Result<bool, DbError> {
        if !(1..=ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS).contains(&request.additional_calls)
        {
            return Err(Self::invalid(format!(
                "Add between 1 and {} provider calls",
                ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS
            )));
        }
        // Bounded by `MAX_ADDITIONAL_CALLS`, so it fits.
        let calls = i64::try_from(request.additional_calls).unwrap_or(i64::MAX);
        let graph_id = &request.graph.graph_id;
        let mut transaction = begin_write(&self.pool).await?;
        Self::lock(&mut transaction, graph_id).await?;
        let recorded: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT execution_graph_id, budget_id, additional_provider_call_count FROM \
             execution_graph_budget_extension WHERE id = ?",
        )
        .bind(&request.action_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(recorded) = recorded {
            if recorded != (graph_id.clone(), request.budget_id.clone(), calls) {
                return Err(Self::invalid(
                    "Budget extension identity was used for a different extension",
                ));
            }

            return Ok(false);
        }
        Self::current(&mut transaction, graph_id, request.graph.revision).await?;
        if Self::cancelled(&mut transaction, graph_id).await? {
            return Err(Self::invalid("Canceled graphs cannot gain provider calls"));
        }
        let updated = sqlx::query(
            "UPDATE execution_graph_budget SET provider_call_limit = provider_call_limit + ? \
             WHERE execution_graph_id = ? AND budget_id = ? AND provider_call_limit <= ?",
        )
        .bind(calls)
        .bind(graph_id)
        .bind(&request.budget_id)
        .bind(i64::MAX - calls)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(Self::invalid(
                "Graph provider budget does not exist or cannot grow further",
            ));
        }
        sqlx::query(
            "INSERT INTO execution_graph_budget_extension (id, execution_graph_id, budget_id, \
             additional_provider_call_count, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&request.action_id)
        .bind(graph_id)
        .bind(&request.budget_id)
        .bind(calls)
        .bind(self.timestamp_source.now_timestamp_seconds())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    /// Adds spent calls, or subtracts refunds, clamped to the current limit.
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

    /// Stored accounting per budget identity.
    async fn budgets(
        connection: &mut SqliteConnection,
        graph_id: &str,
    ) -> Result<BTreeMap<String, BudgetState>, DbError> {
        let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
            "SELECT b.budget_id, b.provider_call_limit, b.spent_provider_call_count, \
             COALESCE((SELECT SUM(x.additional_provider_call_count) FROM \
             execution_graph_budget_extension x WHERE x.execution_graph_id = b.execution_graph_id \
             AND x.budget_id = b.budget_id), 0) FROM execution_graph_budget b WHERE \
             b.execution_graph_id = ?",
        )
        .bind(graph_id)
        .fetch_all(connection)
        .await?;

        // Table constraints keep every count nonnegative, and extensions
        // are part of the limit.
        Ok(rows
            .into_iter()
            .map(|(id, limit, spent, extended)| {
                let count = |value: i64| usize::try_from(value).unwrap_or_default();

                (
                    id,
                    BudgetState {
                        launch_limit: count(limit.saturating_sub(extended)),
                        limit: count(limit),
                        spent: count(spent),
                    },
                )
            })
            .collect())
    }
}
