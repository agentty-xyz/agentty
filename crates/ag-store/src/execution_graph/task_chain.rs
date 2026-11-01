use ag_protocol::{ProtocolRequestProfile, TurnPrompt, parse_protocol_response_strict};
use ag_session::{
    AcceptedTaskPlan, ExecutionGraph, GraphExecutionRef, SessionMessageKind,
    TASK_CHAIN_PLAN_OUTPUT, TaskAgentRole, TaskChain, TaskChainPhase, TaskChainReplyOutcome,
    stored_message_content,
};
use sqlx::SqliteConnection;

use super::{SqliteGraphRepository, TaskChainReplyRecord};
use crate::DbError;
use crate::session_message::SessionMessageStore;

type ReplyRow = (String, String, String, Option<String>, Option<String>);

impl SqliteGraphRepository {
    /// Current workflow snapshots reserve both role IDs before either task
    /// exists. Ordinary admission checks this inside its writer transaction;
    /// graph dispatch alone uses the shared task claim directly.
    pub(crate) async fn require_unreserved_task(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<(), DbError> {
        for role in [TaskAgentRole::Planner, TaskAgentRole::Implementer] {
            let suffix = format!(":{}", role.id());
            let Some(graph_id) = task_id
                .strip_prefix("chain:")
                .and_then(|id| id.strip_suffix(&suffix))
            else {
                continue;
            };
            if Self::graph(connection, graph_id, None)
                .await?
                .as_ref()
                .is_some_and(Self::is_task_chain)
            {
                return Err(Self::invalid(
                    "Workflow task identities require graph admission",
                ));
            }
        }

        Ok(())
    }

    pub(super) async fn save_task_chain(
        &self,
        graph: &ExecutionGraph,
        user_prompt: &str,
    ) -> Result<bool, DbError> {
        if !Self::is_task_chain(graph) {
            return Err(Self::invalid("Graph is not a task chain"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if Self::graph(&mut transaction, &graph.id, None)
            .await?
            .is_none()
        {
            for role in [TaskAgentRole::Planner, TaskAgentRole::Implementer] {
                let task_id = TaskChain::task_id(&graph.id, role);
                let occupied: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task WHERE id = ?)")
                        .bind(&task_id)
                        .fetch_one(&mut *transaction)
                        .await?;
                if occupied {
                    return Err(Self::invalid("Workflow task identity already exists"));
                }
            }
        }
        if !Self::save_in(&mut transaction, graph, None, now).await? {
            return Ok(false);
        }
        for (kind, raw_content) in [
            (SessionMessageKind::UserPrompt, user_prompt),
            (SessionMessageKind::AgentGraph, graph.id.as_str()),
        ] {
            let content = stored_message_content(kind, raw_content);
            SessionMessageStore::append_normalized_in_transaction(
                &mut transaction,
                graph.session_id.as_str(),
                kind,
                &content,
                now,
            )
            .await?;
        }
        transaction.commit().await?;

        Ok(true)
    }

    pub(super) async fn task_chain(
        connection: &mut SqliteConnection,
        chain_id: &str,
    ) -> Result<Option<TaskChain>, DbError> {
        let Some(graph) = Self::graph(connection, chain_id, None).await? else {
            return Ok(None);
        };

        Self::project_task_chain(connection, &graph).await
    }

    pub(super) async fn session_task_chains(
        connection: &mut SqliteConnection,
        session_id: &str,
    ) -> Result<Vec<TaskChain>, DbError> {
        let mut chains = Vec::new();
        for graph in Self::session_graphs(connection, session_id).await? {
            if let Some(chain) = Self::project_task_chain(connection, &graph).await? {
                chains.push(chain);
            }
        }

        Ok(chains)
    }

    pub(super) async fn save_task_chain_reply(
        &self,
        chain_id: &str,
        reply_id: &str,
        prompt: &TurnPrompt,
        source: &GraphExecutionRef,
    ) -> Result<bool, DbError> {
        if reply_id.trim().is_empty() || prompt.text.trim().is_empty() {
            return Err(Self::invalid("Reply identity and message must be nonempty"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, chain_id).await?;
        let existing: Option<ReplyRow> = sqlx::query_as(
            "SELECT execution_graph_id, source_reference, prompt, outcome, action_id FROM \
             execution_graph_reply WHERE id = ?",
        )
        .bind(reply_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(existing) = existing {
            let existing = Self::reply_record(existing)?;
            if existing.chain_id != chain_id
                || existing.prompt != *prompt
                || existing.source != *source
            {
                return Err(Self::invalid(
                    "Reply identity was already used with different inputs",
                ));
            }

            return Ok(false);
        }
        let graph = Self::graph(&mut transaction, chain_id, None)
            .await?
            .filter(Self::is_task_chain)
            .ok_or_else(|| Self::invalid("Workflow does not exist"))?;
        let (_, record) = Self::execution(&mut transaction, source)
            .await?
            .ok_or_else(|| Self::invalid("Reply source execution is missing"))?;
        if record.graph_id != graph.id {
            return Err(Self::invalid("Reply source belongs to another workflow"));
        }
        sqlx::query(
            "INSERT INTO execution_graph_reply (id, execution_graph_id, source_reference, prompt, \
             created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(reply_id)
        .bind(chain_id)
        .bind(serde_json::json!(source).to_string())
        .bind(serde_json::json!(prompt).to_string())
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        let content =
            stored_message_content(SessionMessageKind::UserPrompt, &prompt.transcript_text());
        SessionMessageStore::append_normalized_in_transaction(
            &mut transaction,
            graph.session_id.as_str(),
            SessionMessageKind::UserPrompt,
            &content,
            now,
        )
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    pub(super) async fn task_chain_reply(
        connection: &mut SqliteConnection,
        session_id: &str,
        reply_id: &str,
    ) -> Result<Option<TaskChainReplyRecord>, DbError> {
        let row: Option<ReplyRow> = sqlx::query_as(
            "SELECT r.execution_graph_id, r.source_reference, r.prompt, r.outcome, r.action_id \
             FROM execution_graph_reply r JOIN execution_graph g ON g.id = r.execution_graph_id \
             WHERE r.id = ? AND g.session_id = ?",
        )
        .bind(reply_id)
        .bind(session_id)
        .fetch_optional(connection)
        .await?;

        row.map(Self::reply_record).transpose()
    }

    pub(super) async fn save_task_chain_reply_decision(
        &self,
        reply_id: &str,
        outcome: TaskChainReplyOutcome,
        action_id: Option<String>,
    ) -> Result<TaskChainReplyRecord, DbError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE execution_graph_reply SET outcome = ?, action_id = ? WHERE id = ? AND outcome \
             IS NULL",
        )
        .bind(outcome.id())
        .bind(action_id)
        .bind(reply_id)
        .execute(&mut *transaction)
        .await?;
        let row: ReplyRow = sqlx::query_as(
            "SELECT execution_graph_id, source_reference, prompt, outcome, action_id FROM \
             execution_graph_reply WHERE id = ?",
        )
        .bind(reply_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| Self::invalid("Reply does not exist"))?;
        transaction.commit().await?;

        Self::reply_record(row)
    }

    pub(super) async fn cancel_one_execution(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<bool, DbError> {
        let GraphExecutionRef::TaskAttempt { operation_id, .. } = execution else {
            return Err(Self::invalid(
                "Only task executions can be canceled individually",
            ));
        };
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if Self::execution(&mut transaction, execution)
            .await?
            .is_none()
        {
            return Err(Self::invalid("Graph execution does not exist"));
        }
        // Running work keeps its status until its worker settles cleanup.
        let canceled = sqlx::query(
            "UPDATE session_operation SET cancel_requested = 1, status = CASE WHEN status = \
             'queued' THEN 'canceled' ELSE status END, finished_at = CASE WHEN status = 'queued' \
             THEN ? ELSE finished_at END WHERE id = ? AND status IN ('queued', 'running')",
        )
        .bind(now)
        .bind(operation_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(canceled.rows_affected() == 1)
    }

    pub(super) async fn cancel_chain_stage(
        &self,
        chain_id: &str,
    ) -> Result<Option<GraphExecutionRef>, DbError> {
        let chain = {
            let mut connection = self.pool.acquire().await?;
            Self::task_chain(&mut connection, chain_id)
                .await?
                .ok_or_else(|| Self::invalid("Workflow does not exist"))?
        };
        if !matches!(
            chain.phase,
            TaskChainPhase::Planning | TaskChainPhase::Implementing
        ) {
            return Ok(None);
        }
        // Materialize a cancellation receipt even when Env conflicts delayed
        // admission. This call never dispatches the claim to a worker.
        let admission = self
            .claim_node(
                chain_id,
                chain.revision,
                chain.current_role.id(),
                (&uuid::Uuid::new_v4().to_string(), std::path::Path::new(".")),
                true,
            )
            .await?;
        let execution = match admission {
            ag_worker::GraphAdmission::Claimed(dispatch) => dispatch.record.execution,
            ag_worker::GraphAdmission::Existing(record) => record.execution,
            ag_worker::GraphAdmission::Blocked => return Ok(None),
        };
        self.cancel_one_execution(&execution).await?;

        Ok(Some(execution))
    }

    fn is_task_chain(graph: &ExecutionGraph) -> bool {
        matches!(
            graph.nodes.as_slice(),
            [planner, implementer]
                if TaskChain::role(&graph.id, planner) == Some(TaskAgentRole::Planner)
                    && TaskChain::role(&graph.id, implementer)
                        == Some(TaskAgentRole::Implementer)
        )
    }

    async fn project_task_chain(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
    ) -> Result<Option<TaskChain>, DbError> {
        if !Self::is_task_chain(graph) {
            return Ok(None);
        }
        let progress = Self::progress_in(connection, graph).await?;
        let accepted = Self::accepted_plan(connection, graph).await?;

        Ok(TaskChain::project(graph, &progress, accepted))
    }

    /// Reads the single plan version whose acceptance evidence exists.
    async fn accepted_plan(
        connection: &mut SqliteConnection,
        graph: &ExecutionGraph,
    ) -> Result<Option<AcceptedTaskPlan>, DbError> {
        let planner = TaskAgentRole::Planner.id();
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT r.reference, r.body FROM execution_graph_result r JOIN \
             execution_graph_evidence v ON v.execution_graph_result_id = r.id AND v.kind = \
             'acceptance' WHERE r.execution_graph_id = ? AND r.node_id = ? AND r.output_name = ? \
             ORDER BY r.version DESC LIMIT 1",
        )
        .bind(&graph.id)
        .bind(planner)
        .bind(TASK_CHAIN_PLAN_OUTPUT)
        .fetch_optional(connection)
        .await?;
        let Some((reference, body)) = row else {
            return Ok(None);
        };
        let reference: ag_session::GraphResultRef =
            serde_json::from_str(&reference).map_err(|error| Self::invalid(error.to_string()))?;
        let GraphExecutionRef::TaskAttempt {
            attempt_id,
            task_id,
            ..
        } = reference.execution
        else {
            return Err(Self::invalid("Accepted plan has no task attempt"));
        };
        let response = parse_protocol_response_strict(&body, ProtocolRequestProfile::SessionTurn)
            .map_err(|error| Self::invalid(error.to_string()))?;

        Ok(Some(AcceptedTaskPlan {
            attempt_id,
            task_id,
            text: response.answer,
        }))
    }

    fn reply_record(
        (chain_id, source, prompt, outcome, action_id): ReplyRow,
    ) -> Result<TaskChainReplyRecord, DbError> {
        let decode = |error: serde_json::Error| Self::invalid(error.to_string());

        Ok(TaskChainReplyRecord {
            action_id,
            chain_id,
            outcome: outcome
                .map(|outcome| {
                    TaskChainReplyOutcome::from_id(&outcome)
                        .ok_or_else(|| Self::invalid("Unknown reply outcome"))
                })
                .transpose()?,
            prompt: serde_json::from_str(&prompt).map_err(decode)?,
            source: serde_json::from_str(&source).map_err(decode)?,
        })
    }
}

#[cfg(test)]
#[path = "task_chain_test.rs"]
mod tests;
