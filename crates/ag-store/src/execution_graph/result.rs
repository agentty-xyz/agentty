use ag_protocol::parse_protocol_response_strict;
use ag_session::{GraphResult, GraphResultRef, GraphResultState};
use sqlx::SqliteConnection;

use super::{GraphEvidence, SqliteGraphRepository, StoredGraphEvidence, StoredGraphResult};
use crate::DbError;

impl SqliteGraphRepository {
    pub(super) async fn produce(
        connection: &mut SqliteConnection,
        reference: &GraphResultRef,
        body: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        reference
            .validate()
            .map_err(|error| Self::invalid(error.to_string()))?;
        let version = Self::integer(reference.version)?;
        if let Some(stored) = Self::result(connection, &reference.id).await? {
            if stored.result.reference != *reference || stored.body != body {
                return Err(Self::invalid(
                    "Result identity already has different contents",
                ));
            }

            return Ok(false);
        }
        let graph =
            Self::current(connection, &reference.graph_id, reference.graph_revision).await?;
        let (execution_id, record) = Self::execution(connection, &reference.execution)
            .await?
            .ok_or_else(|| Self::invalid("Result requires a linked execution"))?;
        if record.graph_id != reference.graph_id
            || record.graph_revision != reference.graph_revision
            || record.node_id != reference.node_id
        {
            return Err(Self::invalid("Result does not match its linked execution"));
        }
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == reference.node_id)
            .ok_or_else(|| Self::invalid("Result node does not exist"))?;
        if !node
            .outputs
            .iter()
            .any(|output| output.name == reference.output_name && output.schema == reference.schema)
        {
            return Err(Self::invalid(
                "Result must match a declared output and schema",
            ));
        }
        let previous = sqlx::query!(r#"SELECT COALESCE(MAX(version), 0) AS "version!: i64" FROM execution_graph_result WHERE execution_graph_id = ? AND node_id = ? AND output_name = ?"#, reference.graph_id, reference.node_id, reference.output_name)
            .fetch_one(&mut *connection).await?;
        if previous.version.checked_add(1) != Some(version) {
            return Err(Self::invalid(
                "Output version must be the next consecutive version",
            ));
        }
        let serialized = serde_json::json!(reference).to_string();
        sqlx::query!(
            "INSERT INTO execution_graph_result (id, execution_graph_execution_id, \
             execution_graph_id, node_id, output_name, version, reference, body, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            reference.id,
            execution_id,
            reference.graph_id,
            reference.node_id,
            reference.output_name,
            version,
            serialized,
            body,
            now
        )
        .execute(connection)
        .await?;

        Ok(true)
    }

    pub(super) async fn result(
        connection: &mut SqliteConnection,
        result_id: &str,
    ) -> Result<Option<StoredGraphResult>, DbError> {
        let row = sqlx::query!(
            "SELECT reference, body FROM execution_graph_result WHERE id = ?",
            result_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        Self::decode_result(connection, result_id, &row.reference, row.body)
            .await
            .map(Some)
    }

    /// Decodes an already loaded immutable body and its snapshot evidence.
    pub(super) async fn decode_result(
        connection: &mut SqliteConnection,
        result_id: &str,
        reference: &str,
        body: String,
    ) -> Result<StoredGraphResult, DbError> {
        let reference: GraphResultRef =
            serde_json::from_str(reference).map_err(|error| Self::invalid(error.to_string()))?;
        reference
            .validate()
            .map_err(|error| Self::invalid(error.to_string()))?;
        let rows = sqlx::query!(
            "SELECT kind, actor, reason, recorded_at FROM execution_graph_evidence WHERE \
             execution_graph_result_id = ?",
            result_id
        )
        .fetch_all(connection)
        .await?;
        let mut acceptance = None;
        let mut validation = None;
        for row in rows {
            let evidence = StoredGraphEvidence {
                evidence: GraphEvidence {
                    actor: row.actor,
                    reason: row.reason,
                },
                recorded_at: row.recorded_at,
            };
            if row.kind == "validation" {
                validation = Some(evidence);
            } else {
                acceptance = Some(evidence);
            }
        }
        let state = if acceptance.is_some() {
            GraphResultState::Accepted
        } else if validation.is_some() {
            GraphResultState::Validated
        } else {
            GraphResultState::Produced
        };

        Ok(StoredGraphResult {
            acceptance,
            body,
            result: GraphResult { reference, state },
            validation,
        })
    }

    pub(super) async fn attest(
        &self,
        reference: &GraphResultRef,
        evidence: &GraphEvidence,
        kind: &str,
    ) -> Result<bool, DbError> {
        if evidence.actor.trim().is_empty() || evidence.reason.trim().is_empty() {
            return Err(Self::invalid("Evidence requires an actor and reason"));
        }
        let mut transaction = self.pool.begin().await?;
        Self::lock(&mut transaction, &reference.graph_id).await?;
        let graph = Self::current(
            &mut transaction,
            &reference.graph_id,
            reference.graph_revision,
        )
        .await?;
        let stored = Self::result(&mut transaction, &reference.id)
            .await?
            .ok_or_else(|| Self::invalid("Result does not exist"))?;
        let (_, record) = Self::execution(&mut transaction, &reference.execution)
            .await?
            .ok_or_else(|| Self::invalid("Result execution is missing"))?;
        Self::ensure_live_execution(&mut transaction, &graph, &record).await?;
        if stored.result.reference != *reference {
            return Err(Self::invalid(
                "Evidence must name the exact persisted reference",
            ));
        }
        let existing = if kind == "validation" {
            &stored.validation
        } else {
            &stored.acceptance
        };
        if let Some(existing) = existing {
            if existing.evidence != *evidence {
                return Err(Self::invalid(
                    "Evidence identity already has different contents",
                ));
            }

            return Ok(false);
        }
        if kind == "validation" {
            if !Self::authority(
                &mut transaction,
                &reference.execution,
                graph.session_id.as_str(),
            )
            .await?
            {
                return Err(Self::invalid(
                    "Validation requires successful settled execution",
                ));
            }
            let response = parse_protocol_response_strict(&stored.body, reference.schema)
                .map_err(|error| Self::invalid(error.to_string()))?;
            if !response.questions.is_empty() {
                return Err(Self::invalid(
                    "Clarification cannot satisfy result validation",
                ));
            }
        } else if stored.validation.is_none() {
            return Err(Self::invalid("Acceptance requires a validated result"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        sqlx::query!(
            "INSERT INTO execution_graph_evidence (execution_graph_result_id, kind, actor, \
             reason, recorded_at) VALUES (?, ?, ?, ?, ?)",
            reference.id,
            kind,
            evidence.actor,
            evidence.reason,
            now
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(true)
    }
}
