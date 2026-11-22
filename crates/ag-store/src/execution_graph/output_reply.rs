//! Durable chat replies to output versions awaiting acceptance and their
//! first decisions.

use ag_protocol::TurnPrompt;
use ag_session::{GraphOutputReplyOutcome, SessionMessageKind, stored_message_content};
use sqlx::SqliteConnection;

use super::{GraphOutputReplyRecord, SqliteGraphRepository};
use crate::DbError;
use crate::connection::begin_write;
use crate::session_message::SessionMessageStore;

/// Stored outcome identities; the table constraint admits only these.
const OUTCOMES: [(&str, GraphOutputReplyOutcome); 2] = [
    ("accept", GraphOutputReplyOutcome::Accept),
    ("revise", GraphOutputReplyOutcome::Revise),
];

type ReplyRow = (String, String, String, String, Option<String>, String);

impl SqliteGraphRepository {
    /// Records the reply and its visible prompt in one writer transaction.
    pub(super) async fn save_output_reply(
        &self,
        reply: &GraphOutputReplyRecord,
    ) -> Result<bool, DbError> {
        if reply.id.trim().is_empty() || reply.message.trim().is_empty() {
            return Err(Self::invalid("Reply identity and message must be nonempty"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = begin_write(&self.pool).await?;
        Self::lock(&mut transaction, &reply.reference.graph_id).await?;
        if let Some(existing) = Self::output_reply(&mut transaction, &reply.id).await? {
            // A redelivered reply may already carry its decision.
            let redelivered = GraphOutputReplyRecord {
                outcome: reply.outcome,
                ..existing
            };
            if redelivered != *reply {
                return Err(Self::invalid(
                    "Reply identity was already used with different inputs",
                ));
            }

            return Ok(false);
        }
        let stored = Self::result(&mut transaction, &reply.reference.id)
            .await?
            .filter(|stored| stored.result.reference == reply.reference)
            .ok_or_else(|| Self::invalid("Reply must name the exact persisted output"))?;
        if stored.acceptance.is_some() {
            return Err(Self::invalid("Accepted outputs do not accept replies"));
        }
        let graph = Self::current(
            &mut transaction,
            &reply.reference.graph_id,
            reply.reference.graph_revision,
        )
        .await?;
        sqlx::query(
            "INSERT INTO execution_graph_output_reply (id, execution_graph_result_id, execution, \
             message, attachments, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&reply.id)
        .bind(&reply.reference.id)
        .bind(serde_json::json!(reply.execution).to_string())
        .bind(&reply.message)
        .bind(serde_json::json!(reply.attachments).to_string())
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        let prompt = TurnPrompt {
            attachments: reply.attachments.clone(),
            ..TurnPrompt::from_text(reply.message.clone())
        };
        SessionMessageStore::append_normalized_in_transaction(
            &mut transaction,
            graph.session_id.as_str(),
            SessionMessageKind::UserPrompt,
            &stored_message_content(SessionMessageKind::UserPrompt, &prompt.transcript_text()),
            now,
        )
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    /// Keeps an earlier decision; a missing reply fails.
    pub(super) async fn save_output_reply_decision(
        &self,
        reply_id: &str,
        outcome: GraphOutputReplyOutcome,
    ) -> Result<GraphOutputReplyOutcome, DbError> {
        let outcome_id = OUTCOMES
            .iter()
            .find(|(_, candidate)| *candidate == outcome)
            .map(|(id, _)| *id);
        let mut transaction = begin_write(&self.pool).await?;
        sqlx::query(
            "UPDATE execution_graph_output_reply SET outcome = ? WHERE id = ? AND outcome IS NULL",
        )
        .bind(outcome_id)
        .bind(reply_id)
        .execute(&mut *transaction)
        .await?;
        let recorded = Self::output_reply(&mut transaction, reply_id)
            .await?
            .and_then(|reply| reply.outcome)
            .ok_or_else(|| Self::invalid("Reply does not exist"))?;
        transaction.commit().await?;

        Ok(recorded)
    }

    pub(super) async fn mark_output_reply_settled(&self, reply_id: &str) -> Result<(), DbError> {
        let updated =
            sqlx::query("UPDATE execution_graph_output_reply SET is_settled = 1 WHERE id = ?")
                .bind(reply_id)
                .execute(&self.pool)
                .await?;
        if updated.rows_affected() == 0 {
            return Err(Self::invalid("Reply does not exist"));
        }

        Ok(())
    }

    /// Replies to a canceled graph, or to an output version that was accepted
    /// or replaced by a newer version, are final.
    pub(super) async fn pending_output_replies(
        &self,
        session_id: &str,
    ) -> Result<Vec<GraphOutputReplyRecord>, DbError> {
        let rows: Vec<ReplyRow> = sqlx::query_as(
            "SELECT o.id, o.execution, o.message, o.attachments, o.outcome, r.reference FROM \
             execution_graph_output_reply o JOIN execution_graph_result r ON r.id = \
             o.execution_graph_result_id JOIN execution_graph g ON g.id = r.execution_graph_id \
             WHERE g.session_id = ? AND o.is_settled = 0 AND NOT EXISTS (SELECT 1 FROM \
             execution_graph_cancellation c WHERE c.execution_graph_id = g.id) AND NOT EXISTS \
             (SELECT 1 FROM execution_graph_evidence v WHERE v.execution_graph_result_id = r.id \
             AND v.kind = 'acceptance') AND NOT EXISTS (SELECT 1 FROM execution_graph_result n \
             WHERE n.execution_graph_id = r.execution_graph_id AND n.node_id = r.node_id AND \
             n.output_name = r.output_name AND n.version > r.version) ORDER BY o.rowid",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(Self::output_reply_record).collect()
    }

    async fn output_reply(
        connection: &mut SqliteConnection,
        reply_id: &str,
    ) -> Result<Option<GraphOutputReplyRecord>, DbError> {
        let row: Option<ReplyRow> = sqlx::query_as(
            "SELECT o.id, o.execution, o.message, o.attachments, o.outcome, r.reference FROM \
             execution_graph_output_reply o JOIN execution_graph_result r ON r.id = \
             o.execution_graph_result_id WHERE o.id = ?",
        )
        .bind(reply_id)
        .fetch_optional(connection)
        .await?;

        row.map(Self::output_reply_record).transpose()
    }

    fn output_reply_record(
        (id, execution, message, attachments, outcome, reference): ReplyRow,
    ) -> Result<GraphOutputReplyRecord, DbError> {
        let decoding = |error: serde_json::Error| Self::invalid(error.to_string());

        Ok(GraphOutputReplyRecord {
            attachments: serde_json::from_str(&attachments).map_err(decoding)?,
            execution: serde_json::from_str(&execution).map_err(decoding)?,
            id,
            message,
            outcome: outcome.and_then(|stored| {
                OUTCOMES
                    .iter()
                    .find(|(id, _)| *id == stored)
                    .map(|(_, outcome)| *outcome)
            }),
            reference: serde_json::from_str(&reference).map_err(decoding)?,
        })
    }
}
