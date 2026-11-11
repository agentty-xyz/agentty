use ag_protocol::{AgentResponse, ProtocolRequestProfile, parse_protocol_response_strict};
use ag_session::{
    AgentAnswerContent, ExecutionGraph, GraphExecutionRef, SessionMessageKind, TaskChain,
    node_question_notice, stored_message_content,
};
use ag_worker::GraphDispatch;

use super::SqliteGraphRepository;
use crate::session_message::SessionMessageStore;
use crate::{DbError, TaskAgentTurnMetadata, session};

impl SqliteGraphRepository {
    pub(super) async fn save_task_turn(
        &self,
        dispatch: &GraphDispatch,
        metadata: &TaskAgentTurnMetadata,
    ) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        let record = &dispatch.record;
        Self::lock(&mut transaction, &record.graph_id).await?;
        let graph =
            Self::current(&mut transaction, &record.graph_id, record.graph_revision).await?;
        let (id, existing) = Self::execution(&mut transaction, &record.execution)
            .await?
            .ok_or_else(|| Self::invalid("Missing graph task claim"))?;
        if existing != *record || !graph.nodes.contains(&dispatch.node) {
            return Err(Self::invalid("Task response differs from graph claim"));
        }
        Self::ensure_live_execution(&mut transaction, &graph, record).await?;
        let GraphExecutionRef::TaskAttempt { operation_id, .. } = &record.execution else {
            return Err(Self::invalid("Task response requires a task attempt"));
        };
        let response = parse_protocol_response_strict(
            &metadata.assistant_message,
            ProtocolRequestProfile::SessionTurn,
        )
        .map_err(|error| Self::invalid(error.to_string()))?;
        let encoded = serde_json::json!((
            &metadata.assistant_message,
            &metadata.harness_turn_report,
            &metadata.instruction_conversation_id,
            metadata.input_tokens,
            metadata.output_tokens,
            &metadata.provider_conversation_id,
        ))
        .to_string();
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT metadata FROM execution_graph_task_turn WHERE execution_graph_execution_id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(previous) = previous {
            if previous != encoded {
                return Err(Self::invalid("Graph task response is immutable"));
            }

            return Ok(false);
        }
        let running: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM session_operation WHERE id = ? AND status = 'running' \
             AND cancel_requested = 0)",
        )
        .bind(operation_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !running {
            return Err(Self::invalid("Graph task is not running"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        session::persist_turn_usage(
            &mut transaction,
            graph.session_id.as_str(),
            dispatch.node.agent.model().as_str(),
            now,
            metadata.input_tokens,
            metadata.output_tokens,
            metadata.harness_turn_report.as_deref(),
        )
        .await?;
        let question_message_id =
            Self::append_visible_turn_message(&mut transaction, &graph, dispatch, &response, now)
                .await?;
        sqlx::query(
            "INSERT INTO execution_graph_task_turn (execution_graph_execution_id, metadata, \
             session_message_id) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(encoded)
        .bind(question_message_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    /// Appends the response's visible chat message in the turn transaction.
    ///
    /// Task-chain role answers and generic node questions join the session
    /// transcript with their response receipt; other graph conversation
    /// answers stay task-only. Returns the id of an appended node question,
    /// which orders pending questions for chat reply routing.
    async fn append_visible_turn_message(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        graph: &ExecutionGraph,
        dispatch: &GraphDispatch,
        response: &AgentResponse,
        now: i64,
    ) -> Result<Option<i64>, DbError> {
        let (kind, message) = match TaskChain::role(&graph.id, &dispatch.node) {
            Some(agent) => {
                let text = response.to_display_text();
                let answer =
                    (!text.trim().is_empty()).then(|| AgentAnswerContent { agent, text }.encode());

                (SessionMessageKind::AgentAnswer, answer)
            }
            None => (
                SessionMessageKind::WorkflowNotice,
                node_question_notice(&dispatch.node.id, response),
            ),
        };
        let Some(message) = message else {
            return Ok(None);
        };
        let content = stored_message_content(kind, &message);
        let message_id = SessionMessageStore::append_normalized_in_transaction(
            transaction,
            graph.session_id.as_str(),
            kind,
            &content,
            now,
        )
        .await?;

        Ok(message_id.filter(|_| kind == SessionMessageKind::WorkflowNotice))
    }
}
