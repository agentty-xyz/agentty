use ag_protocol::{ProtocolRequestProfile, parse_protocol_response_strict};
use ag_session::{
    AgentAnswerContent, GraphExecutionRef, SessionMessageKind, TaskChain, stored_message_content,
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
        sqlx::query(
            "INSERT INTO execution_graph_task_turn (execution_graph_execution_id, metadata) \
             VALUES (?, ?)",
        )
        .bind(id)
        .bind(encoded)
        .execute(&mut *transaction)
        .await?;
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
        // Task-chain role answers join the visible session transcript with
        // their response receipt; other graph conversations stay task-only.
        let text = response.to_display_text();
        if let Some(agent) = TaskChain::role(&graph.id, &dispatch.node)
            && !text.trim().is_empty()
        {
            let answer = AgentAnswerContent { agent, text }.encode();
            let content = stored_message_content(SessionMessageKind::AgentAnswer, &answer);
            SessionMessageStore::append_normalized_in_transaction(
                &mut transaction,
                graph.session_id.as_str(),
                SessionMessageKind::AgentAnswer,
                &content,
                now,
            )
            .await?;
        }
        transaction.commit().await?;

        Ok(true)
    }
}
