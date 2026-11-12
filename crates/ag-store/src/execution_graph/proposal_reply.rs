//! Durable chat replies to proposal versions and their first decisions.

use ag_session::{GraphProposalReplyOutcome, SessionMessageKind, stored_message_content};
use sqlx::SqliteConnection;

use super::{GraphProposalReplyRecord, SqliteGraphRepository};
use crate::DbError;
use crate::session_message::SessionMessageStore;

/// Stored outcome identities; the table constraint admits only these.
const OUTCOMES: [(&str, GraphProposalReplyOutcome); 3] = [
    ("approve", GraphProposalReplyOutcome::Approve),
    ("cancel", GraphProposalReplyOutcome::Cancel),
    ("revise", GraphProposalReplyOutcome::Revise),
];

type ReplyRow = (String, String, u32, String, Option<String>, bool, String);

impl SqliteGraphRepository {
    /// Records the reply and its visible prompt in one writer transaction.
    pub(super) async fn save_proposal_reply(
        &self,
        reply_id: &str,
        proposal_id: &str,
        version: u32,
        message: &str,
    ) -> Result<bool, DbError> {
        if reply_id.trim().is_empty() || message.trim().is_empty() {
            return Err(Self::invalid("Reply identity and message must be nonempty"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        Self::proposal_lock(&mut transaction, proposal_id).await?;
        if let Some(existing) = Self::proposal_reply(&mut transaction, reply_id).await? {
            if existing.proposal_id != proposal_id
                || existing.version != version
                || existing.message != message
            {
                return Err(Self::invalid(
                    "Reply identity was already used with different inputs",
                ));
            }

            return Ok(false);
        }
        let state = Self::proposal_current(&mut transaction, proposal_id, version).await?;
        if state.version.approval.is_some() {
            return Err(Self::invalid("Approved proposals do not accept replies"));
        }
        sqlx::query(
            "INSERT INTO graph_proposal_reply (id, graph_proposal_id, version, message, \
             created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(reply_id)
        .bind(proposal_id)
        .bind(version)
        .bind(message)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        SessionMessageStore::append_normalized_in_transaction(
            &mut transaction,
            state.input.session_id.as_str(),
            SessionMessageKind::UserPrompt,
            &stored_message_content(SessionMessageKind::UserPrompt, message),
            now,
        )
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    /// Keeps an earlier decision; a missing reply fails.
    pub(super) async fn save_proposal_reply_decision(
        &self,
        reply_id: &str,
        outcome: GraphProposalReplyOutcome,
    ) -> Result<GraphProposalReplyRecord, DbError> {
        let outcome_id = OUTCOMES
            .iter()
            .find(|(_, candidate)| *candidate == outcome)
            .map(|(id, _)| *id);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE graph_proposal_reply SET outcome = ? WHERE id = ? AND outcome IS NULL")
            .bind(outcome_id)
            .bind(reply_id)
            .execute(&mut *transaction)
            .await?;
        let reply = Self::proposal_reply(&mut transaction, reply_id)
            .await?
            .ok_or_else(|| Self::invalid("Reply does not exist"))?;
        transaction.commit().await?;

        Ok(reply)
    }

    pub(super) async fn settle_proposal_reply(&self, reply_id: &str) -> Result<(), DbError> {
        let updated = sqlx::query("UPDATE graph_proposal_reply SET is_settled = 1 WHERE id = ?")
            .bind(reply_id)
            .execute(&self.pool)
            .await?;
        if updated.rows_affected() == 0 {
            return Err(Self::invalid("Reply does not exist"));
        }

        Ok(())
    }

    /// Replies to a superseded, approved, or canceled version are final.
    pub(super) async fn unsettled_proposal_replies(
        &self,
        session_id: &str,
    ) -> Result<Vec<GraphProposalReplyRecord>, DbError> {
        let rows: Vec<ReplyRow> = sqlx::query_as(
            "SELECT r.id, r.graph_proposal_id, r.version, r.message, r.outcome, r.is_settled, \
             p.session_id FROM graph_proposal_reply r JOIN graph_proposal p ON p.id = \
             r.graph_proposal_id JOIN graph_proposal_version v ON v.graph_proposal_id = p.id AND \
             v.version = p.version WHERE p.session_id = ? AND r.version = p.version AND \
             r.is_settled = 0 AND v.is_cancelled = 0 AND v.approval IS NULL ORDER BY r.rowid",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Self::proposal_reply_record).collect())
    }

    async fn proposal_reply(
        connection: &mut SqliteConnection,
        reply_id: &str,
    ) -> Result<Option<GraphProposalReplyRecord>, DbError> {
        let row: Option<ReplyRow> = sqlx::query_as(
            "SELECT r.id, r.graph_proposal_id, r.version, r.message, r.outcome, r.is_settled, \
             p.session_id FROM graph_proposal_reply r JOIN graph_proposal p ON p.id = \
             r.graph_proposal_id WHERE r.id = ?",
        )
        .bind(reply_id)
        .fetch_optional(connection)
        .await?;

        Ok(row.map(Self::proposal_reply_record))
    }

    fn proposal_reply_record(
        (id, proposal_id, version, message, outcome, is_settled, session_id): ReplyRow,
    ) -> GraphProposalReplyRecord {
        GraphProposalReplyRecord {
            id,
            is_settled,
            message,
            outcome: outcome.and_then(|stored| {
                OUTCOMES
                    .iter()
                    .find(|(id, _)| *id == stored)
                    .map(|(_, outcome)| *outcome)
            }),
            proposal_id,
            session_id,
            version,
        }
    }
}
