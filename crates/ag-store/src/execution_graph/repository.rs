use std::num::NonZeroU64;
use std::sync::Arc;

use ag_protocol::TurnPrompt;
use ag_session::{
    ExecutionGraph, GraphExecutionRef, GraphResultRef, TaskChain, TaskChainReplyOutcome,
};
use async_trait::async_trait;
use sqlx::{SqliteConnection, SqlitePool};

use super::{
    GraphEvidence, GraphExecutionRecord, GraphNodeHistory, StoredGraphResult, TaskChainReplyRecord,
};
use crate::{DbError, TimestampSource};

/// Persistence only: no dispatch, capacity allocation, or automatic replay.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait GraphRepository: Send + Sync {
    /// Projects operation/run status, gates, results and clarification from
    /// one database snapshot. Never admits or recovers work.
    async fn progress(
        &self,
        graph_id: &str,
    ) -> Result<Option<ag_session::ExecutionGraphProgress>, DbError>;

    /// Revision-checked terminal cancellation. Admission closes atomically;
    /// active workers still own cleanup and resource release.
    async fn cancel(&self, request: &ag_session::GraphControlRequest) -> Result<(), DbError>;

    /// Records one explicit retry/answer against the latest execution. It is
    /// consumed by a later atomic claim, retaining the original exact inputs.
    async fn continue_node(
        &self,
        request: &ag_session::ContinueGraphNodeRequest,
    ) -> Result<bool, DbError>;

    /// Replays only this node's immutable attempts and explicit replies,
    /// including images attached to free-form replies.
    async fn execution_history(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<GraphNodeHistory>, DbError>;

    /// Requests cancellation of one queued or running task execution without
    /// closing graph admission. Queued work settles as canceled immediately;
    /// a running worker observes the flag. Returns whether it was active.
    async fn cancel_execution(&self, execution: &GraphExecutionRef) -> Result<bool, DbError>;

    /// Cancels the current workflow stage, including an unclaimed first turn
    /// or pending continuation. Unclaimed work receives a canceled receipt
    /// without taking resources or dispatching a worker.
    async fn cancel_task_chain_stage(
        &self,
        chain_id: &str,
    ) -> Result<Option<GraphExecutionRef>, DbError>;

    /// Saves a new task-chain graph together with its visible launch prompt
    /// and graph marker. Returns false for identical redelivery without
    /// appending again. Rejects other graph shapes and occupied task IDs.
    async fn start_task_chain(
        &self,
        graph: &ExecutionGraph,
        user_prompt: &str,
    ) -> Result<bool, DbError>;

    /// Projects one task-chain graph from a single snapshot; never dispatches.
    async fn load_task_chain(&self, chain_id: &str) -> Result<Option<TaskChain>, DbError>;

    /// Lists a session's task chains in creation order.
    async fn list_task_chains(&self, session_id: &str) -> Result<Vec<TaskChain>, DbError>;

    /// Records one natural-language reply bound to the exact execution it
    /// answers and appends it as a visible user prompt atomically. Returns
    /// false for identical redelivery; conflicting reuse is rejected.
    async fn record_task_chain_reply(
        &self,
        chain_id: &str,
        reply_id: &str,
        prompt: &TurnPrompt,
        source: &GraphExecutionRef,
    ) -> Result<bool, DbError>;

    /// Loads a reply owned by this session, or `None` for a new reply.
    async fn load_task_chain_reply(
        &self,
        session_id: &str,
        reply_id: &str,
    ) -> Result<Option<TaskChainReplyRecord>, DbError>;

    /// Records the first decision for a reply and returns the stored one;
    /// later decisions never replace it.
    async fn decide_task_chain_reply(
        &self,
        reply_id: &str,
        outcome: TaskChainReplyOutcome,
        action_id: Option<String>,
    ) -> Result<TaskChainReplyRecord, DbError>;

    /// Returns static task dispatch context UUIDs before session deletion
    /// cascades their receipts. Legacy links never allocate runtime resources.
    async fn list_task_context_ids(&self, session_id: &str) -> Result<Vec<String>, DbError>;
    /// Records an isolated task's observed response and provider continuation
    /// before ordered effects. Requires the exact running graph claim; usage
    /// is accounted once. This does not settle execution or release gates.
    async fn record_task_turn(
        &self,
        dispatch: &ag_worker::GraphDispatch,
        metadata: &crate::TaskAgentTurnMetadata,
    ) -> Result<bool, DbError>;

    /// Reads an observed task turn, including a response whose effects failed.
    /// The immutable graph receipt retains its request, inputs and assignment.
    async fn load_task_turn(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<crate::TaskAgentTurnMetadata>, DbError>;
    /// Saves a complete validated snapshot atomically. Creation requires
    /// revision one and no expected revision; updates require the current
    /// revision and its immediate successor. Identical historical redelivery
    /// returns false. Ownership is immutable. Revisions after execution links
    /// exist are rejected until a coordinator defines safe mutation policy.
    async fn save_graph(
        &self,
        graph: &ExecutionGraph,
        expected_revision: Option<NonZeroU64>,
    ) -> Result<bool, DbError>;

    /// Loads a specified immutable revision, or the current revision for None.
    async fn load_graph(
        &self,
        graph_id: &str,
        revision: Option<NonZeroU64>,
    ) -> Result<Option<ExecutionGraph>, DbError>;

    /// Lists current snapshots for one session in graph creation order.
    async fn list_graphs(&self, session_id: &str) -> Result<Vec<ExecutionGraph>, DbError>;

    /// Links an existing execution and captures all exact inputs atomically.
    /// Checks current revision, ownership, execution kind, and input gates.
    /// Identical redelivery retains inputs even after newer outputs exist.
    /// This does not admit work, enforce resource limits, or establish gates
    /// unrelated to inputs; a future coordinator must do so before dispatch.
    async fn link_execution(&self, record: &GraphExecutionRecord) -> Result<bool, DbError>;

    /// Loads an execution's original graph, node, and exact input receipt.
    async fn load_execution(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<GraphExecutionRecord>, DbError>;

    /// Lists a revision's execution receipts in link order from one snapshot.
    async fn list_executions(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
    ) -> Result<Vec<GraphExecutionRecord>, DbError>;

    /// Saves one immutable output per execution/output name, initially
    /// Produced. Versions are consecutive within the graph node's named
    /// output. Returns false for identical redelivery; conflicting
    /// identities/bodies fail.
    async fn produce_result(&self, reference: &GraphResultRef, body: &str)
    -> Result<bool, DbError>;

    /// Loads the body and evidence for an exact result identity in one
    /// snapshot.
    async fn load_result(&self, result_id: &str) -> Result<Option<StoredGraphResult>, DbError>;

    /// Lists retained versions of one node output in increasing version order.
    /// No version is implicitly selected or accepted.
    async fn list_results(
        &self,
        graph_id: &str,
        node_id: &str,
        output_name: &str,
    ) -> Result<Vec<StoredGraphResult>, DbError>;

    /// Records host validation for the exact reference after checking the
    /// stored schema, successful authority, and absence of clarification.
    /// Repeated identical evidence returns false and preserves its timestamp.
    async fn validate_result(
        &self,
        reference: &GraphResultRef,
        evidence: &GraphEvidence,
    ) -> Result<bool, DbError>;

    /// Records explicit host-attested acceptance of an exact validated version.
    /// No latest-result selection or task-completion side effect occurs.
    async fn accept_result(
        &self,
        reference: &GraphResultRef,
        evidence: &GraphEvidence,
    ) -> Result<bool, DbError>;
}

pub(crate) struct SqliteGraphRepository {
    pub(super) pool: SqlitePool,
    pub(super) timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteGraphRepository {
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    pub(super) fn invalid(reason: impl Into<String>) -> DbError {
        DbError::InvalidData {
            entity: "execution graph",
            reason: reason.into(),
        }
    }

    pub(super) fn integer(value: NonZeroU64) -> Result<i64, DbError> {
        i64::try_from(value.get())
            .map_err(|_| Self::invalid("Revision or version exceeds SQLite range"))
    }

    /// The first statement takes SQLite's writer lock before eligibility reads.
    pub(super) async fn lock(
        connection: &mut SqliteConnection,
        graph_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE execution_graph SET revision = revision WHERE id = ?",
            graph_id
        )
        .execute(connection)
        .await?;

        Ok(())
    }

    pub(super) async fn graph(
        connection: &mut SqliteConnection,
        graph_id: &str,
        revision: Option<NonZeroU64>,
    ) -> Result<Option<ExecutionGraph>, DbError> {
        let revision = revision.map(Self::integer).transpose()?;
        let row = sqlx::query!(
            "SELECT r.snapshot FROM execution_graph_revision r JOIN execution_graph g ON g.id = \
             r.execution_graph_id WHERE g.id = ? AND r.revision = COALESCE(?, g.revision)",
            graph_id,
            revision
        )
        .fetch_optional(connection)
        .await?;
        row.map(|row| {
            let graph: ExecutionGraph = serde_json::from_str(&row.snapshot)
                .map_err(|error| Self::invalid(error.to_string()))?;
            graph
                .validate()
                .map_err(|error| Self::invalid(error.to_string()))?;

            Ok(graph)
        })
        .transpose()
    }

    pub(super) async fn current(
        connection: &mut SqliteConnection,
        graph_id: &str,
        revision: NonZeroU64,
    ) -> Result<ExecutionGraph, DbError> {
        let graph = Self::graph(connection, graph_id, None)
            .await?
            .ok_or_else(|| Self::invalid("Graph does not exist"))?;
        if graph.revision != revision {
            return Err(Self::invalid("Stale graph revision"));
        }

        Ok(graph)
    }
}

#[async_trait]
impl GraphRepository for SqliteGraphRepository {
    async fn progress(
        &self,
        graph_id: &str,
    ) -> Result<Option<ag_session::ExecutionGraphProgress>, DbError> {
        self.graph_progress(graph_id).await
    }

    async fn cancel(&self, request: &ag_session::GraphControlRequest) -> Result<(), DbError> {
        self.cancel_graph(request).await
    }

    async fn continue_node(
        &self,
        request: &ag_session::ContinueGraphNodeRequest,
    ) -> Result<bool, DbError> {
        self.continue_graph_node(request).await
    }

    async fn execution_history(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<GraphNodeHistory>, DbError> {
        self.node_history(execution).await
    }

    async fn list_task_context_ids(&self, session_id: &str) -> Result<Vec<String>, DbError> {
        sqlx::query_scalar(
            "SELECT a.id FROM execution_graph_execution e JOIN task_attempt a ON a.id = \
             e.task_attempt_id JOIN task t ON t.id = a.task_id WHERE t.session_id = ? AND \
             e.is_dispatch_claimed = 1 ORDER BY e.id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    async fn record_task_turn(
        &self,
        dispatch: &ag_worker::GraphDispatch,
        metadata: &crate::TaskAgentTurnMetadata,
    ) -> Result<bool, DbError> {
        self.save_task_turn(dispatch, metadata).await
    }

    async fn load_task_turn(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<crate::TaskAgentTurnMetadata>, DbError> {
        let mut connection = self.pool.acquire().await?;
        let Some((id, _)) = Self::execution(&mut connection, execution).await? else {
            return Ok(None);
        };
        let metadata: Option<String> = sqlx::query_scalar(
            "SELECT metadata FROM execution_graph_task_turn WHERE execution_graph_execution_id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await?;

        metadata
            .map(|metadata| {
                let (
                    assistant_message,
                    harness_turn_report,
                    instruction_conversation_id,
                    input_tokens,
                    output_tokens,
                    provider_conversation_id,
                ) = serde_json::from_str(&metadata)
                    .map_err(|error| Self::invalid(error.to_string()))?;

                Ok(crate::TaskAgentTurnMetadata {
                    assistant_message,
                    harness_turn_report,
                    instruction_conversation_id,
                    input_tokens,
                    output_tokens,
                    provider_conversation_id,
                })
            })
            .transpose()
    }

    async fn save_graph(
        &self,
        graph: &ExecutionGraph,
        expected_revision: Option<NonZeroU64>,
    ) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        let saved = Self::save_in(
            &mut transaction,
            graph,
            expected_revision,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await?;
        transaction.commit().await?;

        Ok(saved)
    }

    async fn load_graph(
        &self,
        graph_id: &str,
        revision: Option<NonZeroU64>,
    ) -> Result<Option<ExecutionGraph>, DbError> {
        let mut connection = self.pool.acquire().await?;

        Self::graph(&mut connection, graph_id, revision).await
    }

    async fn list_graphs(&self, session_id: &str) -> Result<Vec<ExecutionGraph>, DbError> {
        let mut connection = self.pool.acquire().await?;

        Self::session_graphs(&mut connection, session_id).await
    }

    async fn link_execution(&self, record: &GraphExecutionRecord) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, &record.graph_id).await?;
        let inserted = Self::link(
            &mut transaction,
            record,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await?;
        transaction.commit().await?;

        Ok(inserted)
    }

    async fn load_execution(
        &self,
        execution: &GraphExecutionRef,
    ) -> Result<Option<GraphExecutionRecord>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let record = Self::execution(&mut transaction, execution).await?;
        transaction.commit().await?;

        Ok(record.map(|(_, record)| record))
    }

    async fn list_executions(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
    ) -> Result<Vec<GraphExecutionRecord>, DbError> {
        let revision = Self::integer(revision)?;
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query!(
            "SELECT reference FROM execution_graph_execution WHERE execution_graph_id = ? AND \
             revision = ? ORDER BY id",
            graph_id,
            revision
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let reference: GraphExecutionRef = serde_json::from_str(&row.reference)
                .map_err(|error| Self::invalid(error.to_string()))?;
            let (_, record) = Self::execution(&mut transaction, &reference)
                .await?
                .ok_or_else(|| Self::invalid("Execution receipt disappeared"))?;
            records.push(record);
        }
        transaction.commit().await?;

        Ok(records)
    }

    async fn produce_result(
        &self,
        reference: &GraphResultRef,
        body: &str,
    ) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, &reference.graph_id).await?;
        let inserted = Self::produce(
            &mut transaction,
            reference,
            body,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await?;
        transaction.commit().await?;

        Ok(inserted)
    }

    async fn load_result(&self, result_id: &str) -> Result<Option<StoredGraphResult>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let result = Self::result(&mut transaction, result_id).await?;
        transaction.commit().await?;

        Ok(result)
    }

    async fn list_results(
        &self,
        graph_id: &str,
        node_id: &str,
        output_name: &str,
    ) -> Result<Vec<StoredGraphResult>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query!(
            "SELECT id, reference, body FROM execution_graph_result WHERE execution_graph_id = ? \
             AND node_id = ? AND output_name = ? ORDER BY version",
            graph_id,
            node_id,
            output_name
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut results = Vec::with_capacity(rows.len());
        for row in rows {
            let result =
                Self::decode_result(&mut transaction, &row.id, &row.reference, row.body).await?;
            results.push(result);
        }
        transaction.commit().await?;

        Ok(results)
    }

    async fn validate_result(
        &self,
        reference: &GraphResultRef,
        evidence: &GraphEvidence,
    ) -> Result<bool, DbError> {
        self.attest(reference, evidence, "validation").await
    }

    async fn accept_result(
        &self,
        reference: &GraphResultRef,
        evidence: &GraphEvidence,
    ) -> Result<bool, DbError> {
        self.attest(reference, evidence, "acceptance").await
    }

    async fn start_task_chain(
        &self,
        graph: &ExecutionGraph,
        user_prompt: &str,
    ) -> Result<bool, DbError> {
        self.save_task_chain(graph, user_prompt).await
    }

    async fn load_task_chain(&self, chain_id: &str) -> Result<Option<TaskChain>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let chain = Self::task_chain(&mut transaction, chain_id).await?;
        transaction.commit().await?;

        Ok(chain)
    }

    async fn list_task_chains(&self, session_id: &str) -> Result<Vec<TaskChain>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let chains = Self::session_task_chains(&mut transaction, session_id).await?;
        transaction.commit().await?;

        Ok(chains)
    }

    async fn record_task_chain_reply(
        &self,
        chain_id: &str,
        reply_id: &str,
        prompt: &TurnPrompt,
        source: &GraphExecutionRef,
    ) -> Result<bool, DbError> {
        self.save_task_chain_reply(chain_id, reply_id, prompt, source)
            .await
    }

    async fn load_task_chain_reply(
        &self,
        session_id: &str,
        reply_id: &str,
    ) -> Result<Option<TaskChainReplyRecord>, DbError> {
        let mut connection = self.pool.acquire().await?;

        Self::task_chain_reply(&mut connection, session_id, reply_id).await
    }

    async fn decide_task_chain_reply(
        &self,
        reply_id: &str,
        outcome: TaskChainReplyOutcome,
        action_id: Option<String>,
    ) -> Result<TaskChainReplyRecord, DbError> {
        self.save_task_chain_reply_decision(reply_id, outcome, action_id)
            .await
    }

    async fn cancel_execution(&self, execution: &GraphExecutionRef) -> Result<bool, DbError> {
        self.cancel_one_execution(execution).await
    }

    async fn cancel_task_chain_stage(
        &self,
        chain_id: &str,
    ) -> Result<Option<GraphExecutionRef>, DbError> {
        self.cancel_chain_stage(chain_id).await
    }
}

impl SqliteGraphRepository {
    /// Saves one validated snapshot inside the caller's writer transaction.
    pub(super) async fn save_in(
        transaction: &mut SqliteConnection,
        graph: &ExecutionGraph,
        expected_revision: Option<NonZeroU64>,
        now: i64,
    ) -> Result<bool, DbError> {
        graph
            .validate()
            .map_err(|error| Self::invalid(error.to_string()))?;
        let revision = Self::integer(graph.revision)?;
        Self::lock(&mut *transaction, &graph.id).await?;
        if let Some(existing) =
            Self::graph(&mut *transaction, &graph.id, Some(graph.revision)).await?
        {
            if existing != *graph {
                return Err(Self::invalid(
                    "Revision identity already has different contents",
                ));
            }

            return Ok(false);
        }
        let current = Self::graph(&mut *transaction, &graph.id, None).await?;
        if let Some(current) = current {
            if current.session_id != graph.session_id
                || expected_revision != Some(current.revision)
                || current.revision.get().checked_add(1) != Some(graph.revision.get())
            {
                return Err(Self::invalid(
                    "Revision must follow the expected revision and retain ownership",
                ));
            }
            let linked = sqlx::query!(
                "SELECT id FROM execution_graph_execution WHERE execution_graph_id = ? LIMIT 1",
                graph.id
            )
            .fetch_optional(&mut *transaction)
            .await?;
            if linked.is_some() {
                return Err(Self::invalid("Cannot revise a graph with execution links"));
            }
            sqlx::query!(
                "UPDATE execution_graph SET revision = ? WHERE id = ?",
                revision,
                graph.id
            )
            .execute(&mut *transaction)
            .await?;
        } else {
            if expected_revision.is_some() || revision != 1 {
                return Err(Self::invalid(
                    "New graphs require revision one and no expected revision",
                ));
            }
            let session_id = graph.session_id.as_str();
            sqlx::query!(
                "INSERT INTO execution_graph (id, session_id, revision) VALUES (?, ?, ?)",
                graph.id,
                session_id,
                revision
            )
            .execute(&mut *transaction)
            .await?;
        }
        let snapshot = serde_json::json!(graph).to_string();
        sqlx::query!(
            "INSERT INTO execution_graph_revision (execution_graph_id, revision, snapshot, \
             created_at) VALUES (?, ?, ?, ?)",
            graph.id,
            revision,
            snapshot,
            now
        )
        .execute(&mut *transaction)
        .await?;

        Ok(true)
    }

    /// Lists current snapshots for one session in graph creation order.
    pub(super) async fn session_graphs(
        connection: &mut SqliteConnection,
        session_id: &str,
    ) -> Result<Vec<ExecutionGraph>, DbError> {
        let rows = sqlx::query!(
            "SELECT r.snapshot FROM execution_graph g JOIN execution_graph_revision r ON \
             r.execution_graph_id = g.id AND r.revision = g.revision WHERE g.session_id = ? ORDER \
             BY g.rowid",
            session_id
        )
        .fetch_all(connection)
        .await?;
        rows.into_iter()
            .map(|row| {
                let graph: ExecutionGraph = serde_json::from_str(&row.snapshot)
                    .map_err(|error| Self::invalid(error.to_string()))?;
                graph
                    .validate()
                    .map_err(|error| Self::invalid(error.to_string()))?;

                Ok(graph)
            })
            .collect()
    }
}
