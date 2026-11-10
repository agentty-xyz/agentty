//! Durable proposal reservations and atomic exact-version approval.

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    GraphProposalAction, GraphProposalApproval, GraphProposalState, GraphProposalVersion,
    GraphProposalVersionOutcome,
};
use async_trait::async_trait;
use sqlx::{Row, SqliteConnection};

use crate::DbError;
use crate::execution_graph::SqliteGraphRepository;

/// Narrow persistence boundary; querying never generates or dispatches work.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait GraphProposalRepository: Send + Sync {
    /// Captures original input and the first generation reservation once.
    async fn create(&self, state: &GraphProposalState) -> Result<bool, DbError>;
    /// Loads a current or historical immutable preview with its action
    /// receipts.
    async fn get(
        &self,
        id: &str,
        version: Option<u32>,
    ) -> Result<Option<GraphProposalState>, DbError>;
    /// Lists current proposals for a session, including pending restored work.
    async fn list(&self, session_id: &str) -> Result<Vec<GraphProposalState>, DbError>;
    /// Loads only the session's most recently created proposal, so frequent
    /// status reads stay independent of proposal history length.
    async fn latest(&self, session_id: &str) -> Result<Option<GraphProposalState>, DbError>;
    /// Reserves a bounded explicit retry or complete clarification answer set.
    async fn reserve(
        &self,
        id: &str,
        version: u32,
        action: &GraphProposalAction,
    ) -> Result<GraphProposalState, DbError>;
    /// Completes a reservation once; conflicting or canceled completions fail.
    async fn finish(
        &self,
        id: &str,
        version: u32,
        outcome: &GraphProposalVersionOutcome,
    ) -> Result<(), DbError>;
    /// Atomically publishes the inspected snapshot and approval receipt.
    /// Duplicate approval returns false and must never restart dispatch.
    async fn approve(&self, id: &str, version: u32) -> Result<bool, DbError>;
    /// Permanently discards one current unapproved proposal.
    async fn cancel(&self, id: &str, version: u32) -> Result<(), DbError>;
}

impl SqliteGraphRepository {
    async fn proposal_lock(connection: &mut SqliteConnection, id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE graph_proposal SET version = version WHERE id = ?")
            .bind(id)
            .execute(connection)
            .await?;

        Ok(())
    }

    async fn proposal_get(
        connection: &mut SqliteConnection,
        id: &str,
        version: Option<u32>,
    ) -> Result<Option<GraphProposalState>, DbError> {
        let row = sqlx::query(
            "SELECT p.capture, v.snapshot, v.is_cancelled, v.approval FROM graph_proposal p JOIN \
             graph_proposal_version v ON v.graph_proposal_id = p.id AND v.version = COALESCE(?, \
             p.version) WHERE p.id = ?",
        )
        .bind(version)
        .bind(id)
        .fetch_optional(connection)
        .await?;
        row.map(|row| {
            let mut state: GraphProposalState = serde_json::from_str(row.try_get("capture")?)
                .map_err(|error| Self::invalid(error.to_string()))?;
            state.version = serde_json::from_str(row.try_get("snapshot")?)
                .map_err(|error| Self::invalid(error.to_string()))?;
            state.version.is_cancelled = row.try_get("is_cancelled")?;
            let approval: Option<String> = row.try_get("approval")?;
            state.version.approval = approval
                .map(|value| {
                    serde_json::from_str(&value).map_err(|error| Self::invalid(error.to_string()))
                })
                .transpose()?;

            Ok(state)
        })
        .transpose()
    }

    async fn proposal_current(
        connection: &mut SqliteConnection,
        id: &str,
        version: u32,
    ) -> Result<GraphProposalState, DbError> {
        let state = Self::proposal_get(connection, id, None)
            .await?
            .ok_or_else(|| Self::invalid("Proposal is missing"))?;
        if state.version.number != version || state.version.is_cancelled {
            return Err(Self::invalid("Stale or canceled proposal version"));
        }

        Ok(state)
    }

    async fn proposal_insert_version(
        &self,
        connection: &mut SqliteConnection,
        id: &str,
        version: &GraphProposalVersion,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO graph_proposal_version (graph_proposal_id, version, snapshot, \
             is_generating, created_at) VALUES (?, ?, ?, 1, ?)",
        )
        .bind(id)
        .bind(version.number)
        .bind(serde_json::json!(version).to_string())
        .bind(self.timestamp_source.now_timestamp_seconds())
        .execute(connection)
        .await?;

        Ok(())
    }
}

#[async_trait]
impl GraphProposalRepository for SqliteGraphRepository {
    async fn create(&self, state: &GraphProposalState) -> Result<bool, DbError> {
        if state.input.id.trim().is_empty()
            || state.version.number != 1
            || state.input.request.text.trim().is_empty()
            || state.input.request.has_attachments()
            || state.limits.generations == 0
            || state.version.outcome != GraphProposalVersionOutcome::Generating
            || state.version.approval.is_some()
            || state.version.is_cancelled
        {
            return Err(Self::invalid(
                "Invalid initial proposal capture; attachment paths are unsupported",
            ));
        }
        let mut transaction = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO graph_proposal (id, session_id, version, capture, created_at) \
             VALUES (?, ?, 1, ?, ?)",
        )
        .bind(&state.input.id)
        .bind(state.input.session_id.as_str())
        .bind(serde_json::json!(state).to_string())
        .bind(self.timestamp_source.now_timestamp_seconds())
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        if inserted {
            self.proposal_insert_version(&mut transaction, &state.input.id, &state.version)
                .await?;
        } else {
            let existing = Self::proposal_get(&mut transaction, &state.input.id, None)
                .await?
                .ok_or_else(|| Self::invalid("Proposal disappeared"))?;
            if existing.input != state.input {
                return Err(Self::invalid(
                    "Proposal identity has different original input",
                ));
            }
        }
        transaction.commit().await?;

        Ok(inserted)
    }

    async fn get(
        &self,
        id: &str,
        version: Option<u32>,
    ) -> Result<Option<GraphProposalState>, DbError> {
        let mut connection = self.pool.acquire().await?;

        Self::proposal_get(&mut connection, id, version).await
    }

    async fn list(&self, session_id: &str) -> Result<Vec<GraphProposalState>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM graph_proposal WHERE session_id = ? ORDER BY rowid")
                .bind(session_id)
                .fetch_all(&mut *transaction)
                .await?;
        let mut states = Vec::new();
        for id in ids {
            if let Some(state) = Self::proposal_get(&mut transaction, &id, None).await? {
                states.push(state);
            }
        }
        transaction.commit().await?;

        Ok(states)
    }

    async fn latest(&self, session_id: &str) -> Result<Option<GraphProposalState>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM graph_proposal WHERE session_id = ? ORDER BY rowid DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let state = match id {
            Some(id) => Self::proposal_get(&mut transaction, &id, None).await?,
            None => None,
        };
        transaction.commit().await?;

        Ok(state)
    }

    async fn reserve(
        &self,
        id: &str,
        version: u32,
        action: &GraphProposalAction,
    ) -> Result<GraphProposalState, DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::proposal_lock(&mut transaction, id).await?;
        let mut state = Self::proposal_current(&mut transaction, id, version).await?;
        if state.version.approval.is_some() || version >= state.limits.generations {
            return Err(Self::invalid(
                "Proposal is approved or generation limit is exhausted",
            ));
        }
        let answers = match action {
            GraphProposalAction::Regenerate => state.version.answers.clone(),
            GraphProposalAction::Answer(answers) => {
                let GraphProposalVersionOutcome::Generated { response, .. } =
                    &state.version.outcome
                else {
                    return Err(Self::invalid("Proposal has no clarification questions"));
                };
                let GraphProposalOutcome::Clarification { questions } = &response.outcome else {
                    return Err(Self::invalid("Proposal has no clarification questions"));
                };
                if answers.len() != questions.len()
                    || answers.iter().any(|answer| answer.trim().is_empty())
                {
                    return Err(Self::invalid("Answer the complete exact question set"));
                }
                let mut history = state.version.answers.clone();
                history.extend(questions.iter().zip(answers).map(|(question, answer)| {
                    serde_json::json!({"question": question.text, "answer": answer}).to_string()
                }));
                history
            }
            GraphProposalAction::Revise(feedback) => {
                if feedback.trim().is_empty() {
                    return Err(Self::invalid("Revision feedback is empty"));
                }
                let entry = match &state.version.outcome {
                    GraphProposalVersionOutcome::Generated { response, .. } => {
                        match &response.outcome {
                            GraphProposalOutcome::Clarification { questions } => {
                                let questions: Vec<&str> = questions
                                    .iter()
                                    .map(|question| question.text.as_str())
                                    .collect();

                                serde_json::json!({"questions": questions, "reply": feedback})
                            }
                            GraphProposalOutcome::Proposal { .. } => {
                                serde_json::json!({"feedback": feedback})
                            }
                        }
                    }
                    _ => serde_json::json!({"feedback": feedback}),
                };
                let mut history = state.version.answers.clone();
                history.push(entry.to_string());

                history
            }
            _ => return Err(Self::invalid("Action cannot reserve generation")),
        };
        if serde_json::json!(answers).to_string().len() > state.limits.bytes {
            return Err(Self::invalid(
                "Clarification history exceeds captured size limit",
            ));
        }
        state.version = GraphProposalVersion {
            answers,
            approval: None,
            is_cancelled: false,
            number: version + 1,
            outcome: GraphProposalVersionOutcome::Generating,
            parent_id: format!("graph-proposal:{id}:{}", version + 1),
            utility_run_id: None,
        };
        sqlx::query("UPDATE graph_proposal SET version = ? WHERE id = ?")
            .bind(state.version.number)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        self.proposal_insert_version(&mut transaction, id, &state.version)
            .await?;
        transaction.commit().await?;

        Ok(state)
    }

    async fn finish(
        &self,
        id: &str,
        version: u32,
        outcome: &GraphProposalVersionOutcome,
    ) -> Result<(), DbError> {
        if matches!(outcome, GraphProposalVersionOutcome::Generating) {
            return Err(Self::invalid("Generation completion must be terminal"));
        }
        let mut transaction = self.pool.begin().await?;
        Self::proposal_lock(&mut transaction, id).await?;
        let mut state = Self::proposal_current(&mut transaction, id, version).await?;
        if state.version.outcome != GraphProposalVersionOutcome::Generating {
            if state.version.outcome == *outcome {
                return Ok(());
            }
            return Err(Self::invalid("Proposal version is immutable"));
        }
        state.version.utility_run_id = sqlx::query_scalar(
            "SELECT id FROM agent_run WHERE parent_id = ? AND session_id = ? ORDER BY rowid LIMIT \
             1",
        )
        .bind(&state.version.parent_id)
        .bind(state.input.session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        state.version.outcome = outcome.clone();
        sqlx::query(
            "UPDATE graph_proposal_version SET snapshot = ?, is_generating = 0 WHERE \
             graph_proposal_id = ? AND version = ?",
        )
        .bind(serde_json::json!(state.version).to_string())
        .bind(id)
        .bind(version)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(())
    }

    async fn approve(&self, id: &str, version: u32) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::proposal_lock(&mut transaction, id).await?;
        let state = Self::proposal_current(&mut transaction, id, version).await?;
        if state.version.approval.is_some() {
            return Ok(false);
        }
        let GraphProposalVersionOutcome::Generated {
            launch: Some(launch),
            response,
        } = &state.version.outcome
        else {
            return Err(Self::invalid("Only a resolved preview can be approved"));
        };
        let GraphProposalOutcome::Proposal { proposal } = &response.outcome else {
            return Err(Self::invalid("Clarification cannot be approved"));
        };
        let expected = ag_session::GraphProposalResolution {
            agent: state.agent,
            answers: &state.version.answers,
            definitions: &state.definitions,
            graph_id: &format!("proposal:{id}:{version}"),
            limits: &state.limits,
            permission: state.permission,
            request: &state.input.request,
            session_id: &state.input.session_id,
        }
        .resolve(proposal)
        .map_err(Self::invalid)?;
        if launch.graph != expected
            || launch.provider_call_limits
                != std::collections::BTreeMap::from([(
                    format!("{}:calls", expected.id),
                    state.limits.provider_calls,
                )])
        {
            return Err(Self::invalid(
                "Resolved snapshot or limits do not match inspected proposal",
            ));
        }
        Self::save_in(
            &mut transaction,
            &launch.graph,
            None,
            self.timestamp_source.now_timestamp_seconds(),
        )
        .await?;
        let receipt = GraphProposalApproval {
            graph_id: launch.graph.id.clone(),
            limits: state.limits,
            version,
        };
        sqlx::query(
            "UPDATE graph_proposal_version SET approval = ? WHERE graph_proposal_id = ? AND \
             version = ?",
        )
        .bind(serde_json::json!(receipt).to_string())
        .bind(id)
        .bind(version)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    async fn cancel(&self, id: &str, version: u32) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        Self::proposal_lock(&mut transaction, id).await?;
        let state = Self::proposal_get(&mut transaction, id, None)
            .await?
            .ok_or_else(|| Self::invalid("Proposal is missing"))?;
        if state.version.number != version || state.version.approval.is_some() {
            return Err(Self::invalid("Stale or already approved proposal"));
        }
        sqlx::query(
            "UPDATE graph_proposal_version SET is_cancelled = 1 WHERE graph_proposal_id = ? AND \
             version = ?",
        )
        .bind(id)
        .bind(version)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(())
    }
}
