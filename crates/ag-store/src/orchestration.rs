//! Orchestration and orchestration-task persistence adapters.
//!
//! Task rows are written when the controller proposes a plan, before any child
//! session exists. That ordering is what makes fan-out idempotent: a restart or
//! a retry reuses the `(session_orchestration_id, task_key)` unique key instead
//! of creating a second child for the same subtask.

use std::sync::Arc;

use ag_session::IntegrationApproach;
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::timestamp::TimestampSource;
use crate::{DbError, DbResultExt, status};

/// Row returned when loading one `session_orchestration`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOrchestrationRow {
    /// Project containing the controller and all child sessions.
    pub controller_project_id: i64,
    /// Controller session that owns this orchestration.
    pub controller_session_id: String,
    /// Canonical single-goal statement approved for this campaign.
    pub goal_statement: String,
    /// Stable database identifier.
    pub id: i64,
    /// Maximum number of children allowed to run at once.
    pub max_parallelism: i64,
    /// Exact managed task whose questions are mirrored onto the controller.
    pub relayed_question_task_id: Option<i64>,
    /// Persisted orchestration status string.
    pub status: String,
    /// Monotonic identity for durable verification turns.
    pub verification_generation: i64,
}

/// Row returned when loading one `session_orchestration_task`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOrchestrationTaskRow {
    /// Serialized acceptance criteria checked during verification.
    pub acceptance_criteria: String,
    /// Serialized changed paths outside the task's expected planning areas.
    pub area_violations: String,
    /// Whether the latest child diff stayed within its expected planning areas.
    pub areas_compliant: Option<bool>,
    /// Number of child sessions created for this task so far.
    pub attempt_count: i64,
    /// Persisted added-line count from the latest child diff refresh.
    pub child_added_lines: i64,
    /// Latest assistant answer emitted by the linked child.
    pub child_answer: Option<String>,
    /// Persisted deleted-line count from the latest child diff refresh.
    pub child_deleted_lines: i64,
    /// Durable focused-review generation state observed on the child.
    pub child_focused_review_status: Option<String>,
    /// Latest focused-review markdown observed on the child.
    pub child_focused_review_text: Option<String>,
    /// Whether the latest child diff refresh found any content.
    pub child_has_diff: Option<bool>,
    /// Total input tokens observed on the linked child session.
    pub child_input_tokens: i64,
    /// Total output tokens observed on the linked child session.
    pub child_output_tokens: i64,
    /// Persisted clarification questions on the linked child.
    pub child_questions: Option<String>,
    /// Child session created for this task, when one exists.
    pub child_session_id: Option<String>,
    /// Persisted lifecycle status observed on the linked child session.
    pub child_status: Option<String>,
    /// Monotonic identity for durable feedback delivery attempts.
    pub continuation_generation: i64,
    /// Feedback prompt waiting to resume the existing managed child.
    pub continuation_prompt: Option<String>,
    /// Stable database identifier.
    pub id: i64,
    /// Number of bounded automatic spawn retries already consumed.
    pub infrastructure_retry_count: i64,
    /// Persisted execution behavior string.
    pub kind: String,
    /// Most recent failure detail, when the task failed.
    pub last_error: Option<String>,
    /// Stable integration order selected on the approval board.
    pub merge_position: i64,
    /// Standalone prompt handed to the child session.
    pub prompt: String,
    /// Bounded full report captured from a temporary research child.
    pub research_report: Option<String>,
    /// Bounded child-reported result summary used for fan-in, when present.
    pub result_summary: Option<String>,
    /// Number of automatic focused-review remediation turns already consumed.
    pub review_iteration: i64,
    /// Persisted task status string.
    pub status: String,
    /// Stable subtask key unique within the owning orchestration.
    pub task_key: String,
    /// Short human-readable task title.
    pub title: String,
    /// Serialized repository areas this task expects to touch.
    pub touched_areas: String,
    /// Controller explanation for the latest verification verdict.
    pub verification_reason: Option<String>,
    /// Latest controller verdict for this task.
    pub verification_verdict: Option<String>,
}

/// Task scope and child base needed to compute verification evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrchestrationTaskScopeRow {
    /// Base branch used by the managed child worktree.
    pub base_branch: String,
    /// Stable orchestration-task identifier.
    pub id: i64,
    /// Serialized repository areas assigned to this task.
    pub touched_areas: String,
}

/// Bulk-loaded controller progress and child adjacency for one session row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOrchestrationMetadataRow {
    /// Owning controller for an orchestration child, when this is a child row.
    pub controller_session_id: Option<String>,
    /// Latest orchestration status for a controller row, when this is a
    /// controller.
    pub orchestration_status: Option<String>,
    /// Number of child tasks currently creating or running.
    pub running_task_count: i64,
    /// Session receiving the derived metadata.
    pub session_id: String,
    /// Number of child tasks currently waiting for user input.
    pub waiting_task_count: i64,
}

/// Values used to persist one planned orchestration task.
///
/// Owns its fields so the persistence trait method stays lifetime-free. A
/// borrowed variant forced the trait to carry a generic lifetime, which
/// `mockall::automock` drops in the generated mock. Owning the data is
/// allocation-cheap on this once-per-plan path.
pub struct PersistedOrchestrationTask {
    /// Serialized acceptance criteria checked during verification.
    pub acceptance_criteria: String,
    /// Persisted execution behavior.
    pub kind: String,
    /// Stable integration order selected on the approval board.
    pub merge_position: i64,
    /// Standalone prompt handed to the child session.
    pub prompt: String,
    /// Owning orchestration identifier.
    pub session_orchestration_id: i64,
    /// Stable subtask key unique within the owning orchestration.
    pub task_key: String,
    /// Short human-readable task title.
    pub title: String,
    /// Serialized repository areas this task expects to touch.
    pub touched_areas: String,
}

/// Orchestration persistence boundary used by the coordinator and tests.
///
/// The coordinator owns its own pool through this trait so reconciliation reads
/// never contend with the foreground session-runtime mailbox.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait OrchestrationRepository: Send + Sync {
    /// Inserts one orchestration and returns its stable identifier.
    async fn insert_orchestration(
        &self,
        controller_session_id: &str,
        status: &str,
        max_parallelism: i64,
    ) -> Result<i64, DbError>;

    /// Inserts one planned task, replacing any previous attempt that used the
    /// same `task_key` within the same orchestration.
    ///
    /// Re-proposing a task key preserves the existing row identity and its
    /// `attempt_count`, so a retry updates the plan in place instead of fanning
    /// out a duplicate child. The retry transaction detaches both persisted
    /// directions of any prior child link before replacement creation.
    async fn upsert_orchestration_task(
        &self,
        task: PersistedOrchestrationTask,
    ) -> Result<i64, DbError>;

    /// Loads the most recent orchestration owned by one controller session.
    async fn load_orchestration_for_controller(
        &self,
        controller_session_id: &str,
    ) -> Result<Option<SessionOrchestrationRow>, DbError>;

    /// Loads every orchestration whose persisted status is still active.
    async fn load_active_orchestrations(&self) -> Result<Vec<SessionOrchestrationRow>, DbError>;

    /// Loads managed `Reviewing` sessions whose incomplete focused-review
    /// state must be regenerated during app startup.
    async fn load_recoverable_focused_review_session_ids(
        &self,
        project_id: i64,
    ) -> Result<Vec<String>, DbError>;

    /// Loads controller progress and child adjacency for one project's
    /// sessions in a single query.
    async fn load_session_metadata_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionOrchestrationMetadataRow>, DbError>;

    /// Loads all tasks belonging to one orchestration in stable plan order.
    async fn load_orchestration_tasks(
        &self,
        session_orchestration_id: i64,
    ) -> Result<Vec<SessionOrchestrationTaskRow>, DbError>;

    /// Loads the destination selected for verified child branches.
    async fn load_orchestration_integration_approach(&self, id: i64) -> Result<String, DbError>;

    /// Loads the declared task scope linked to one managed child.
    async fn load_orchestration_task_scope_for_child(
        &self,
        child_session_id: &str,
    ) -> Result<Option<OrchestrationTaskScopeRow>, DbError>;

    /// Loads a child session already persisted for one orchestration task.
    async fn load_child_session_id_for_task(&self, task_id: i64)
    -> Result<Option<String>, DbError>;

    /// Atomically blocks new fan-out before cascade cancellation begins.
    async fn begin_orchestration_cancellation(&self, id: i64) -> Result<bool, DbError>;

    /// Atomically claims one planned task while its orchestration is running.
    async fn claim_orchestration_task(&self, id: i64) -> Result<bool, DbError>;

    /// Atomically claims one focused-review remediation turn and clears the
    /// consumed child review cache.
    async fn claim_orchestration_review_application(
        &self,
        id: i64,
        prompt: &str,
        iteration_limit: i64,
    ) -> Result<bool, DbError>;

    /// Atomically claims roll-up submission for one running orchestration.
    async fn claim_orchestration_rollup(&self, id: i64) -> Result<bool, DbError>;

    /// Completes a submitted roll-up unless cancellation won the state race.
    async fn complete_orchestration_rollup(&self, id: i64) -> Result<bool, DbError>;

    /// Persists one explicit controller verdict for a settled task.
    async fn record_orchestration_verdict(
        &self,
        id: i64,
        task_key: &str,
        is_pass: bool,
        reason: &str,
    ) -> Result<bool, DbError>;

    /// Archives a finalized campaign and makes its controller terminal.
    async fn complete_orchestration_campaign(&self, id: i64) -> Result<bool, DbError>;

    /// Loads the durable worker-operation status for one roll-up delivery.
    async fn load_rollup_operation_status(
        &self,
        operation_id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Updates one orchestration's persisted status.
    async fn update_orchestration_status(&self, id: i64, status: &str) -> Result<(), DbError>;

    /// Approves parked proposed tasks and resumes campaign execution.
    async fn approve_orchestration_plan(&self, id: i64) -> Result<bool, DbError>;

    /// Persists the selected integration destination and starts integration.
    async fn approve_orchestration_integration(
        &self,
        id: i64,
        approach: IntegrationApproach,
    ) -> Result<bool, DbError>;

    /// Updates plan metadata before fan-out begins.
    async fn update_orchestration_plan(
        &self,
        id: i64,
        goal_statement: &str,
        max_parallelism: i64,
    ) -> Result<(), DbError>;

    /// Routes feedback to a live managed child without replacing its branch.
    async fn queue_orchestration_continuation(
        &self,
        id: i64,
        prompt: &str,
        acceptance_criteria: &str,
        touched_areas: &str,
    ) -> Result<bool, DbError>;

    /// Returns previously verified tasks to fan-in before a follow-up wave.
    async fn reset_orchestration_verification(&self, id: i64) -> Result<(), DbError>;

    /// Records one infrastructure failure and returns the next task status.
    async fn record_orchestration_spawn_failure(
        &self,
        id: i64,
        error: &str,
        retry_limit: i64,
    ) -> Result<String, DbError>;

    /// Permanently transfers one managed child to ordinary user ownership.
    async fn detach_orchestration_child(&self, child_session_id: &str) -> Result<bool, DbError>;

    /// Claims one managed task's questions and mirrors them onto its
    /// controller.
    async fn surface_orchestration_questions(
        &self,
        session_orchestration_id: i64,
        task_id: i64,
        questions: &str,
    ) -> Result<bool, DbError>;

    /// Clears the exact question proxy claimed by one orchestration.
    async fn clear_orchestration_questions(
        &self,
        session_orchestration_id: i64,
    ) -> Result<(), DbError>;

    /// Links one child and counts the attempt only while fan-out still owns it.
    async fn link_orchestration_task_child(
        &self,
        id: i64,
        child_session_id: &str,
    ) -> Result<bool, DbError>;

    /// Updates one task's status and failure detail.
    async fn update_orchestration_task_status(
        &self,
        id: i64,
        status: &str,
        last_error: Option<String>,
    ) -> Result<(), DbError>;

    /// Records one child's bounded result summary for fan-in.
    async fn update_orchestration_task_result_summary(
        &self,
        id: i64,
        result_summary: &str,
    ) -> Result<(), DbError>;

    /// Records one research child's bounded report before its temporary
    /// worktree is discarded.
    async fn update_orchestration_task_research_report(
        &self,
        id: i64,
        research_report: &str,
    ) -> Result<(), DbError>;

    /// Records a mechanically computed touched-area planning comparison.
    async fn update_orchestration_task_area_compliance(
        &self,
        id: i64,
        areas_compliant: Option<bool>,
        area_violations: &str,
    ) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`OrchestrationRepository`].
#[derive(Clone)]
pub(crate) struct SqliteOrchestrationRepository(SqlitePool, Arc<dyn TimestampSource>);

impl SqliteOrchestrationRepository {
    /// Creates an orchestration repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self(pool, timestamp_source)
    }

    async fn transition_orchestration_and_tasks(
        &self,
        id: i64,
        transition: OrchestrationTransition,
    ) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self.0.begin().await.db_context(transition.context)?;
        let result = sqlx::query(
            r"
UPDATE session_orchestration
SET status = ?,
    updated_at = ?
WHERE id = ?
  AND status = ?
",
        )
        .bind(transition.to_orchestration_status)
        .bind(now)
        .bind(id)
        .bind(transition.from_orchestration_status)
        .execute(&mut *transaction)
        .await
        .db_context(transition.context)?;
        if result.rows_affected() == 1 {
            sqlx::query(
                r"
UPDATE session_orchestration_task
SET status = ?,
    updated_at = ?
WHERE session_orchestration_id = ?
  AND status = ?
  AND (? = 0 OR verification_verdict = 'Pass')
",
            )
            .bind(transition.to_task_status)
            .bind(now)
            .bind(id)
            .bind(transition.from_task_status)
            .bind(i64::from(transition.require_pass_verdict))
            .execute(&mut *transaction)
            .await
            .db_context(transition.context)?;
        }
        transaction.commit().await.db_context(transition.context)?;

        Ok(result.rows_affected() == 1)
    }
}

#[async_trait]
impl OrchestrationRepository for SqliteOrchestrationRepository {
    async fn insert_orchestration(
        &self,
        controller_session_id: &str,
        status: &str,
        max_parallelism: i64,
    ) -> Result<i64, DbError> {
        status::validate_orchestration(status)?;
        let now = self.1.now_timestamp_seconds();

        let row = sqlx::query!(
            r#"
INSERT INTO session_orchestration (
    controller_session_id,
    goal_statement,
    status,
    max_parallelism,
    created_at,
    updated_at
)
VALUES (?, '', ?, ?, ?, ?)
RETURNING id AS "id!: i64"
"#,
            controller_session_id,
            status,
            max_parallelism,
            now,
            now
        )
        .fetch_one(&self.0)
        .await?;

        Ok(row.id)
    }

    async fn upsert_orchestration_task(
        &self,
        task: PersistedOrchestrationTask,
    ) -> Result<i64, DbError> {
        let PersistedOrchestrationTask {
            acceptance_criteria,
            kind,
            merge_position,
            prompt,
            session_orchestration_id,
            task_key,
            title,
            touched_areas,
        } = task;
        kind.parse::<ag_session::OrchestrationTaskKind>()
            .map_err(|_| DbError::InvalidData {
                entity: "orchestration task kind",
                reason: format!("unknown persisted kind `{kind}`"),
            })?;
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("upsert orchestration task")?;

        let row = sqlx::query!(
            r#"
INSERT INTO session_orchestration_task (
    session_orchestration_id,
    task_key,
    title,
    prompt,
    touched_areas,
    acceptance_criteria,
    kind,
    merge_position,
    status,
    created_at,
    updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'Planned', ?, ?)
ON CONFLICT(session_orchestration_id, task_key) DO UPDATE
SET title = excluded.title,
    prompt = excluded.prompt,
    kind = excluded.kind,
    touched_areas = excluded.touched_areas,
    acceptance_criteria = excluded.acceptance_criteria,
    merge_position = excluded.merge_position,
    status = 'Planned',
    child_session_id = NULL,
    continuation_prompt = NULL,
    review_iteration = 0,
    research_report = NULL,
    result_summary = NULL,
    verification_reason = NULL,
    verification_verdict = NULL,
    last_error = NULL,
    updated_at = excluded.updated_at
RETURNING id AS "id!: i64"
"#,
            session_orchestration_id,
            task_key,
            title,
            prompt,
            touched_areas,
            acceptance_criteria,
            kind,
            merge_position,
            now,
            now
        )
        .fetch_one(&mut *transaction)
        .await
        .db_context("upsert orchestration task")?;

        sqlx::query!(
            r"
UPDATE session
SET orchestration_task_id = NULL,
    updated_at = ?
WHERE orchestration_task_id = ?
",
            now,
            row.id
        )
        .execute(&mut *transaction)
        .await
        .db_context("upsert orchestration task")?;

        transaction
            .commit()
            .await
            .db_context("upsert orchestration task")?;

        Ok(row.id)
    }

    async fn load_orchestration_for_controller(
        &self,
        controller_session_id: &str,
    ) -> Result<Option<SessionOrchestrationRow>, DbError> {
        let row = sqlx::query_as!(
            SessionOrchestrationRow,
            r#"
SELECT orchestration.id AS "id!: i64",
       session.project_id AS "controller_project_id!: i64",
       orchestration.controller_session_id,
       orchestration.goal_statement,
       orchestration.relayed_question_task_id,
       orchestration.status,
       orchestration.max_parallelism,
       orchestration.verification_generation
FROM session_orchestration AS orchestration
INNER JOIN session
ON session.id = orchestration.controller_session_id
WHERE orchestration.controller_session_id = ?
ORDER BY orchestration.id DESC
LIMIT 1
"#,
            controller_session_id
        )
        .fetch_optional(&self.0)
        .await?;
        if let Some(row) = &row {
            status::validate_orchestration(&row.status)?;
        }

        Ok(row)
    }

    async fn load_active_orchestrations(&self) -> Result<Vec<SessionOrchestrationRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionOrchestrationRow,
            r#"
SELECT orchestration.id AS "id!: i64",
       session.project_id AS "controller_project_id!: i64",
       orchestration.controller_session_id,
       orchestration.goal_statement,
       orchestration.relayed_question_task_id,
       orchestration.status,
       orchestration.max_parallelism,
       orchestration.verification_generation
FROM session_orchestration AS orchestration
INNER JOIN session
ON session.id = orchestration.controller_session_id
WHERE orchestration.status IN (
    'AwaitingApproval',
    'Running',
    'Verifying',
    'AwaitingIntegration',
    'Integrating',
    'Canceling'
)
ORDER BY orchestration.id
"#
        )
        .fetch_all(&self.0)
        .await?;
        for row in &rows {
            status::validate_orchestration(&row.status)?;
        }

        Ok(rows)
    }

    async fn load_recoverable_focused_review_session_ids(
        &self,
        project_id: i64,
    ) -> Result<Vec<String>, DbError> {
        let session_ids = sqlx::query_scalar!(
            r#"
SELECT child.id AS "id!: String"
FROM session_orchestration_task AS task
INNER JOIN session_orchestration AS orchestration
ON orchestration.id = task.session_orchestration_id
INNER JOIN session AS child
ON child.id = task.child_session_id
WHERE child.project_id = ?
  AND orchestration.status IN ('AwaitingApproval', 'Running')
  AND task.status = 'Reviewing'
  AND child.status IN ('Review', 'AgentReview')
  AND (
      child.focused_review_status IS NULL
      OR child.focused_review_status = 'Pending'
  )
ORDER BY task.id
"#,
            project_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(session_ids)
    }

    async fn load_session_metadata_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<SessionOrchestrationMetadataRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionOrchestrationMetadataRow,
            r#"
WITH latest_orchestration_id AS (
    SELECT controller_session_id,
           MAX(id) AS orchestration_id
    FROM session_orchestration
    GROUP BY controller_session_id
),
latest_orchestration AS (
    SELECT orchestration.id,
           orchestration.controller_session_id,
           orchestration.status
    FROM session_orchestration AS orchestration
    INNER JOIN latest_orchestration_id AS latest
    ON latest.orchestration_id = orchestration.id
),
controller_metadata AS (
    SELECT orchestration.controller_session_id AS session_id,
           orchestration.status AS orchestration_status,
           COALESCE(SUM(
               CASE WHEN task.status IN (
                        'Creating',
                        'Running',
                        'Reviewing',
                        'ReviewApplying',
                        'ContinuationPending'
                    )
                    THEN 1 ELSE 0 END
           ), 0) AS running_task_count,
           COALESCE(SUM(
               CASE WHEN task.status = 'WaitingForInput' THEN 1 ELSE 0 END
           ), 0) AS waiting_task_count
    FROM latest_orchestration AS orchestration
    LEFT JOIN session_orchestration_task AS task
    ON task.session_orchestration_id = orchestration.id
    GROUP BY orchestration.id
),
child_metadata AS (
    SELECT task.child_session_id AS session_id,
           orchestration.controller_session_id
    FROM session_orchestration_task AS task
    INNER JOIN session_orchestration AS orchestration
    ON orchestration.id = task.session_orchestration_id
    WHERE task.child_session_id IS NOT NULL
)
SELECT session.id AS "session_id!: String",
       child_metadata.controller_session_id,
       controller_metadata.orchestration_status,
       COALESCE(controller_metadata.running_task_count, 0) AS "running_task_count!: i64",
       COALESCE(controller_metadata.waiting_task_count, 0) AS "waiting_task_count!: i64"
FROM session
LEFT JOIN controller_metadata
ON controller_metadata.session_id = session.id
LEFT JOIN child_metadata
ON child_metadata.session_id = session.id
WHERE session.project_id = ?
  AND (
      controller_metadata.session_id IS NOT NULL
      OR child_metadata.session_id IS NOT NULL
  )
ORDER BY session.id
"#,
            project_id
        )
        .fetch_all(&self.0)
        .await?;
        for row in &rows {
            if let Some(orchestration_status) = &row.orchestration_status {
                status::validate_orchestration(orchestration_status)?;
            }
        }

        Ok(rows)
    }

    async fn load_orchestration_tasks(
        &self,
        session_orchestration_id: i64,
    ) -> Result<Vec<SessionOrchestrationTaskRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionOrchestrationTaskRow,
            r#"
SELECT task.id AS "id!: i64",
       task.acceptance_criteria,
       task.area_violations,
       task.areas_compliant AS "areas_compliant?: bool",
       task.attempt_count,
       COALESCE(child.added_lines, 0) AS "child_added_lines!: i64",
       (
           SELECT message.content
           FROM session_message AS message
           WHERE message.session_id = task.child_session_id
             AND message.kind = 'assistant_answer'
           ORDER BY message.position DESC
           LIMIT 1
       ) AS child_answer,
       COALESCE(child.deleted_lines, 0) AS "child_deleted_lines!: i64",
       child.focused_review_status AS child_focused_review_status,
       child.focused_review_text AS child_focused_review_text,
       child.has_diff AS "child_has_diff?: bool",
       COALESCE(child.input_tokens, 0) AS "child_input_tokens!: i64",
       COALESCE(child.output_tokens, 0) AS "child_output_tokens!: i64",
       task.child_session_id,
       child.status AS child_status,
       child.questions AS child_questions,
       task.continuation_generation,
       task.continuation_prompt,
       task.infrastructure_retry_count,
       task.kind,
       task.last_error,
       task.merge_position,
       task.prompt,
       task.research_report,
       task.result_summary,
       task.review_iteration,
       task.status,
       task.task_key,
       task.touched_areas,
       task.title,
       task.verification_reason,
       task.verification_verdict
FROM session_orchestration_task AS task
LEFT JOIN session AS child
ON child.id = task.child_session_id
WHERE task.session_orchestration_id = ?
ORDER BY task.merge_position, task.id
"#,
            session_orchestration_id
        )
        .fetch_all(&self.0)
        .await?;
        for row in &rows {
            status::validate_orchestration_task(&row.status)?;
            row.kind
                .parse::<ag_session::OrchestrationTaskKind>()
                .map_err(|_| DbError::InvalidData {
                    entity: "orchestration task kind",
                    reason: format!("unknown persisted kind `{}`", row.kind),
                })?;
            if let Some(child_status) = &row.child_status {
                status::validate_session(child_status)?;
            }
        }

        Ok(rows)
    }

    async fn load_orchestration_integration_approach(&self, id: i64) -> Result<String, DbError> {
        sqlx::query_scalar::<_, String>(
            "SELECT integration_approach FROM session_orchestration WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&self.0)
        .await
        .db_context("load orchestration integration approach")
    }

    async fn load_orchestration_task_scope_for_child(
        &self,
        child_session_id: &str,
    ) -> Result<Option<OrchestrationTaskScopeRow>, DbError> {
        sqlx::query_as!(
            OrchestrationTaskScopeRow,
            r#"
SELECT child.base_branch,
       task.id AS "id!: i64",
       task.touched_areas
FROM session_orchestration_task AS task
INNER JOIN session AS child
ON child.id = task.child_session_id
WHERE child.id = ?
  AND child.role = 'OrchestrationWorker'
  AND task.kind = 'Implementation'
"#,
            child_session_id
        )
        .fetch_optional(&self.0)
        .await
        .db_context("load orchestration task scope for child")
    }

    async fn load_child_session_id_for_task(
        &self,
        task_id: i64,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query!(
            r#"
SELECT id AS "id!: String"
FROM session
WHERE orchestration_task_id = ?
"#,
            task_id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| row.id))
    }

    async fn begin_orchestration_cancellation(&self, id: i64) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration
SET status = 'Canceling',
    updated_at = ?
WHERE id = ?
  AND status IN (
      'AwaitingApproval',
      'Running',
      'Verifying',
      'AwaitingIntegration',
      'Integrating',
      'Canceling'
  )
",
            now,
            id
        )
        .execute(&self.0)
        .await
        .db_context("begin orchestration cancellation")?;

        Ok(result.rows_affected() == 1)
    }

    async fn claim_orchestration_task(&self, id: i64) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET status = 'Creating',
    last_error = NULL,
    updated_at = ?
WHERE id = ?
  AND status = 'Planned'
  AND EXISTS (
      SELECT 1
      FROM session_orchestration
      WHERE session_orchestration.id = session_orchestration_task.session_orchestration_id
        AND session_orchestration.status = 'Running'
  )
",
            now,
            id
        )
        .execute(&self.0)
        .await
        .db_context("claim orchestration task")?;

        Ok(result.rows_affected() == 1)
    }

    async fn claim_orchestration_review_application(
        &self,
        id: i64,
        prompt: &str,
        iteration_limit: i64,
    ) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("claim orchestration review application")?;
        let claim = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET continuation_generation = continuation_generation + 1,
    continuation_prompt = ?,
    review_iteration = review_iteration + 1,
    status = 'ReviewApplying',
    updated_at = ?
WHERE id = ?
  AND status = 'Reviewing'
  AND review_iteration < ?
  AND child_session_id IS NOT NULL
",
            prompt,
            now,
            id,
            iteration_limit
        )
        .execute(&mut *transaction)
        .await
        .db_context("claim orchestration review application")?;
        if claim.rows_affected() == 0 {
            transaction
                .rollback()
                .await
                .db_context("claim orchestration review application")?;

            return Ok(false);
        }

        sqlx::query!(
            r"
UPDATE session
SET focused_review_status = NULL,
    focused_review_diff_hash = NULL,
    focused_review_text = NULL,
    updated_at = ?
WHERE id = (
    SELECT child_session_id
    FROM session_orchestration_task
    WHERE id = ?
)
",
            now,
            id
        )
        .execute(&mut *transaction)
        .await
        .db_context("claim orchestration review application")?;
        transaction
            .commit()
            .await
            .db_context("claim orchestration review application")?;

        Ok(true)
    }

    async fn claim_orchestration_rollup(&self, id: i64) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration
SET status = 'Verifying',
    verification_generation = verification_generation + 1,
    updated_at = ?
WHERE id = ?
  AND status = 'Running'
",
            now,
            id
        )
        .execute(&self.0)
        .await
        .db_context("claim orchestration rollup")?;

        Ok(result.rows_affected() == 1)
    }

    async fn complete_orchestration_rollup(&self, id: i64) -> Result<bool, DbError> {
        self.transition_orchestration_and_tasks(
            id,
            OrchestrationTransition {
                context: "complete orchestration rollup",
                from_orchestration_status: "Verifying",
                from_task_status: "Ready",
                require_pass_verdict: true,
                to_orchestration_status: "AwaitingIntegration",
                to_task_status: "AwaitingIntegration",
            },
        )
        .await
    }

    async fn record_orchestration_verdict(
        &self,
        id: i64,
        task_key: &str,
        is_pass: bool,
        reason: &str,
    ) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let verdict = if is_pass { "Pass" } else { "Flag" };
        let result = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET verification_reason = ?,
    verification_verdict = ?,
    updated_at = ?
WHERE session_orchestration_id = ?
  AND task_key = ?
  AND status IN ('Ready', 'Reported')
  AND EXISTS (
      SELECT 1
      FROM session_orchestration
      WHERE id = ?
        AND status = 'Verifying'
  )
",
            reason,
            verdict,
            now,
            id,
            task_key,
            id
        )
        .execute(&self.0)
        .await
        .db_context("record orchestration verdict")?;

        Ok(result.rows_affected() == 1)
    }

    async fn complete_orchestration_campaign(&self, id: i64) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("complete orchestration campaign")?;
        let orchestration = sqlx::query!(
            r#"
UPDATE session_orchestration
SET status = 'Done',
    updated_at = ?
WHERE id = ?
  AND status IN ('AwaitingIntegration', 'Integrating')
RETURNING controller_session_id AS "controller_session_id!: String"
"#,
            now,
            id
        )
        .fetch_optional(&mut *transaction)
        .await
        .db_context("complete orchestration campaign")?;
        if let Some(orchestration) = &orchestration {
            sqlx::query!(
                r"
UPDATE session
SET status = 'Done',
    questions = '',
    updated_at = ?
WHERE id = ?
  AND role = 'Orchestrator'
  AND status IN ('Review', 'Question')
",
                now,
                orchestration.controller_session_id
            )
            .execute(&mut *transaction)
            .await
            .db_context("complete orchestration campaign")?;
        }
        transaction
            .commit()
            .await
            .db_context("complete orchestration campaign")?;

        Ok(orchestration.is_some())
    }

    async fn load_rollup_operation_status(
        &self,
        operation_id: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query!(
            r#"
SELECT status AS "status!: String"
FROM session_operation
WHERE id = ?
"#,
            operation_id
        )
        .fetch_optional(&self.0)
        .await?;
        if let Some(row) = &row {
            status::validate_operation(&row.status)?;
        }

        Ok(row.map(|row| row.status))
    }

    async fn update_orchestration_status(&self, id: i64, status: &str) -> Result<(), DbError> {
        status::validate_orchestration(status)?;
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration
SET status = ?,
    updated_at = ?
WHERE id = ?
",
            status,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn approve_orchestration_plan(&self, id: i64) -> Result<bool, DbError> {
        self.transition_orchestration_and_tasks(
            id,
            OrchestrationTransition {
                context: "approve orchestration plan",
                from_orchestration_status: "AwaitingApproval",
                from_task_status: "Proposed",
                require_pass_verdict: false,
                to_orchestration_status: "Running",
                to_task_status: "Planned",
            },
        )
        .await
    }

    async fn approve_orchestration_integration(
        &self,
        id: i64,
        approach: IntegrationApproach,
    ) -> Result<bool, DbError> {
        let approach = approach.to_string();
        let now = self.1.now_timestamp_seconds();
        let result = sqlx::query(
            r"
UPDATE session_orchestration
SET integration_approach = ?,
    status = 'Integrating',
    updated_at = ?
WHERE id = ?
  AND status = 'AwaitingIntegration'
",
        )
        .bind(approach)
        .bind(now)
        .bind(id)
        .execute(&self.0)
        .await
        .db_context("approve orchestration integration")?;

        Ok(result.rows_affected() == 1)
    }

    async fn update_orchestration_plan(
        &self,
        id: i64,
        goal_statement: &str,
        max_parallelism: i64,
    ) -> Result<(), DbError> {
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration
SET goal_statement = ?,
    max_parallelism = ?,
    updated_at = ?
WHERE id = ?
  AND status = 'AwaitingApproval'
",
            goal_statement,
            max_parallelism,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn queue_orchestration_continuation(
        &self,
        id: i64,
        prompt: &str,
        acceptance_criteria: &str,
        touched_areas: &str,
    ) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("queue orchestration continuation")?;
        let result = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET acceptance_criteria = ?,
    area_violations = '[]',
    areas_compliant = NULL,
    continuation_generation = continuation_generation + 1,
    continuation_prompt = ?,
    review_iteration = 0,
    status = 'ContinuationPending',
    touched_areas = ?,
    result_summary = NULL,
    verification_verdict = NULL,
    verification_reason = NULL,
    last_error = NULL,
    updated_at = ?
WHERE id = ?
  AND child_session_id IS NOT NULL
  AND status IN ('Ready', 'AwaitingIntegration', 'IntegrationFailed')
",
            acceptance_criteria,
            prompt,
            touched_areas,
            now,
            id
        )
        .execute(&mut *transaction)
        .await?;

        if result.rows_affected() == 1 {
            sqlx::query!(
                r"
UPDATE session
SET focused_review_status = NULL,
    focused_review_diff_hash = NULL,
    focused_review_text = NULL,
    updated_at = ?
WHERE id = (
    SELECT child_session_id
    FROM session_orchestration_task
    WHERE id = ?
)
",
                now,
                id
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;

        Ok(result.rows_affected() == 1)
    }

    async fn reset_orchestration_verification(&self, id: i64) -> Result<(), DbError> {
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration_task
SET status = 'Ready',
    verification_reason = NULL,
    verification_verdict = NULL,
    updated_at = ?
WHERE session_orchestration_id = ?
  AND status = 'AwaitingIntegration'
",
            now,
            id
        )
        .execute(&self.0)
        .await
        .db_context("reset orchestration verification")?;

        Ok(())
    }

    async fn record_orchestration_spawn_failure(
        &self,
        id: i64,
        error: &str,
        retry_limit: i64,
    ) -> Result<String, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("record orchestration spawn failure")?;
        sqlx::query!(
            r"
UPDATE session
SET orchestration_task_id = NULL,
    updated_at = ?
WHERE orchestration_task_id = ?
",
            now,
            id
        )
        .execute(&mut *transaction)
        .await
        .db_context("record orchestration spawn failure")?;
        let row = sqlx::query!(
            r#"
UPDATE session_orchestration_task
SET child_session_id = NULL,
    infrastructure_retry_count = infrastructure_retry_count + 1,
    status = CASE
        WHEN infrastructure_retry_count < ? THEN 'Planned'
        ELSE 'Failed'
    END,
    last_error = ?,
    updated_at = ?
WHERE id = ?
RETURNING status AS "status!: String"
"#,
            retry_limit,
            error,
            now,
            id
        )
        .fetch_one(&mut *transaction)
        .await
        .db_context("record orchestration spawn failure")?;
        transaction
            .commit()
            .await
            .db_context("record orchestration spawn failure")?;
        status::validate_orchestration_task(&row.status)?;

        Ok(row.status)
    }

    async fn detach_orchestration_child(&self, child_session_id: &str) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("detach orchestration child")?;
        let task = sqlx::query!(
            r#"
SELECT orchestration_task_id AS "task_id!: i64"
FROM session
WHERE id = ?
  AND role = 'OrchestrationWorker'
  AND orchestration_task_id IS NOT NULL
"#,
            child_session_id
        )
        .fetch_optional(&mut *transaction)
        .await
        .db_context("detach orchestration child")?;
        let Some(task) = task else {
            transaction
                .commit()
                .await
                .db_context("detach orchestration child")?;

            return Ok(false);
        };

        sqlx::query!(
            r"
UPDATE session_orchestration_task
SET child_session_id = NULL,
    status = 'Detached',
    updated_at = ?
WHERE id = ?
",
            now,
            task.task_id
        )
        .execute(&mut *transaction)
        .await
        .db_context("detach orchestration child")?;
        sqlx::query!(
            r"
UPDATE session
SET orchestration_task_id = NULL,
    role = 'Worker',
    updated_at = ?
WHERE id = ?
",
            now,
            child_session_id
        )
        .execute(&mut *transaction)
        .await
        .db_context("detach orchestration child")?;
        transaction
            .commit()
            .await
            .db_context("detach orchestration child")?;

        Ok(true)
    }

    async fn surface_orchestration_questions(
        &self,
        session_orchestration_id: i64,
        task_id: i64,
        questions: &str,
    ) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("surface orchestration questions")?;
        let claim = sqlx::query!(
            r"
UPDATE session_orchestration
SET relayed_question_task_id = ?,
    updated_at = ?
WHERE id = ?
  AND relayed_question_task_id IS NULL
  AND EXISTS (
      SELECT 1
      FROM session_orchestration_task AS task
      WHERE task.id = ?
        AND task.session_orchestration_id = session_orchestration.id
        AND task.status = 'WaitingForInput'
        AND task.child_session_id IS NOT NULL
  )
  AND EXISTS (
      SELECT 1
      FROM session AS controller
      WHERE controller.id = session_orchestration.controller_session_id
        AND controller.role = 'Orchestrator'
        AND controller.status IN ('Review', 'Question')
        AND COALESCE(controller.questions, '') = ''
  )
",
            task_id,
            now,
            session_orchestration_id,
            task_id
        )
        .execute(&mut *transaction)
        .await
        .db_context("surface orchestration questions")?;
        if claim.rows_affected() == 0 {
            transaction
                .rollback()
                .await
                .db_context("surface orchestration questions")?;

            return Ok(false);
        }
        sqlx::query!(
            r"
UPDATE session
SET questions = ?,
    status = 'Question',
    updated_at = ?
WHERE id = (
    SELECT controller_session_id
    FROM session_orchestration
    WHERE id = ?
)
",
            questions,
            now,
            session_orchestration_id
        )
        .execute(&mut *transaction)
        .await
        .db_context("surface orchestration questions")?;
        transaction
            .commit()
            .await
            .db_context("surface orchestration questions")?;

        Ok(true)
    }

    async fn clear_orchestration_questions(
        &self,
        session_orchestration_id: i64,
    ) -> Result<(), DbError> {
        let now = self.1.now_timestamp_seconds();
        let mut transaction = self
            .0
            .begin()
            .await
            .db_context("clear orchestration questions")?;
        sqlx::query!(
            r"
UPDATE session
SET questions = '',
    status = 'Review',
    updated_at = ?
WHERE id = (
    SELECT controller_session_id
    FROM session_orchestration
    WHERE id = ?
      AND relayed_question_task_id IS NOT NULL
)
  AND role = 'Orchestrator'
  AND status = 'Question'
",
            now,
            session_orchestration_id
        )
        .execute(&mut *transaction)
        .await
        .db_context("clear orchestration questions")?;
        sqlx::query!(
            r"
UPDATE session_orchestration
SET relayed_question_task_id = NULL,
    updated_at = ?
WHERE id = ?
  AND relayed_question_task_id IS NOT NULL
",
            now,
            session_orchestration_id
        )
        .execute(&mut *transaction)
        .await
        .db_context("clear orchestration questions")?;
        transaction
            .commit()
            .await
            .db_context("clear orchestration questions")?;

        Ok(())
    }

    async fn link_orchestration_task_child(
        &self,
        id: i64,
        child_session_id: &str,
    ) -> Result<bool, DbError> {
        let now = self.1.now_timestamp_seconds();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET child_session_id = ?,
    status = 'Running',
    attempt_count = attempt_count + 1,
    updated_at = ?
WHERE id = ?
  AND status = 'Creating'
  AND EXISTS (
      SELECT 1
      FROM session_orchestration
      WHERE session_orchestration.id = session_orchestration_task.session_orchestration_id
        AND session_orchestration.status = 'Running'
  )
",
            child_session_id,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn update_orchestration_task_status(
        &self,
        id: i64,
        status: &str,
        last_error: Option<String>,
    ) -> Result<(), DbError> {
        status::validate_orchestration_task(status)?;
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration_task
SET status = ?,
    last_error = ?,
    updated_at = ?
WHERE id = ?
",
            status,
            last_error,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_orchestration_task_result_summary(
        &self,
        id: i64,
        result_summary: &str,
    ) -> Result<(), DbError> {
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration_task
SET result_summary = ?,
    updated_at = ?
WHERE id = ?
",
            result_summary,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_orchestration_task_research_report(
        &self,
        id: i64,
        research_report: &str,
    ) -> Result<(), DbError> {
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration_task
SET research_report = ?,
    updated_at = ?
WHERE id = ?
  AND kind = 'Research'
",
            research_report,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn update_orchestration_task_area_compliance(
        &self,
        id: i64,
        areas_compliant: Option<bool>,
        area_violations: &str,
    ) -> Result<(), DbError> {
        let now = self.1.now_timestamp_seconds();

        sqlx::query!(
            r"
UPDATE session_orchestration_task
SET area_violations = ?,
    areas_compliant = ?,
    updated_at = ?
WHERE id = ?
",
            area_violations,
            areas_compliant,
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

struct OrchestrationTransition {
    context: &'static str,
    from_orchestration_status: &'static str,
    from_task_status: &'static str,
    require_pass_verdict: bool,
    to_orchestration_status: &'static str,
    to_task_status: &'static str,
}

#[cfg(test)]
#[path = "orchestration_test.rs"]
mod tests;
