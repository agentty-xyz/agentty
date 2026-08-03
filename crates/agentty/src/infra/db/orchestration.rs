//! Orchestration and orchestration-task persistence adapters.
//!
//! Task rows are written when the controller proposes a plan, before any child
//! session exists. That ordering is what makes fan-out idempotent: a restart or
//! a retry reuses the `(session_orchestration_id, task_key)` unique key instead
//! of creating a second child for the same subtask.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use super::status;
use crate::domain::orchestration::IntegrationApproach;
use crate::infra::clock::{self, Clock};
use crate::infra::db::{DbError, DbResultExt};

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
    /// Serialized changed paths outside the task's declared areas.
    pub area_violations: String,
    /// Whether the latest child diff stayed within its declared areas.
    pub areas_compliant: Option<bool>,
    /// Number of child sessions created for this task so far.
    pub attempt_count: i64,
    /// Persisted added-line count from the latest child diff refresh.
    pub child_added_lines: i64,
    /// Persisted deleted-line count from the latest child diff refresh.
    pub child_deleted_lines: i64,
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
    /// Cumulative summary observed on the linked child session.
    pub child_summary: Option<String>,
    /// Monotonic identity for durable feedback delivery attempts.
    pub continuation_generation: i64,
    /// Feedback prompt waiting to resume the existing managed child.
    pub continuation_prompt: Option<String>,
    /// Stable database identifier.
    pub id: i64,
    /// Most recent failure detail, when the task failed.
    pub last_error: Option<String>,
    /// Number of bounded automatic spawn retries already consumed.
    pub infrastructure_retry_count: i64,
    /// Stable integration order selected on the approval board.
    pub merge_position: i64,
    /// Standalone prompt handed to the child session.
    pub prompt: String,
    /// Bounded child-reported result summary used for fan-in, when present.
    pub result_summary: Option<String>,
    /// Persisted task status string.
    pub status: String,
    /// Stable subtask key unique within the owning orchestration.
    pub task_key: String,
    /// Serialized repository areas this task expects to touch.
    pub touched_areas: String,
    /// Short human-readable task title.
    pub title: String,
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
#[cfg_attr(test, mockall::automock)]
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

    /// Records mechanically computed touched-area compliance evidence.
    async fn update_orchestration_task_area_compliance(
        &self,
        id: i64,
        areas_compliant: bool,
        area_violations: &str,
    ) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`OrchestrationRepository`].
#[derive(Clone)]
pub(crate) struct SqliteOrchestrationRepository(SqlitePool, Arc<dyn Clock>);

struct OrchestrationTransition {
    context: &'static str,
    from_orchestration_status: &'static str,
    from_task_status: &'static str,
    require_pass_verdict: bool,
    to_orchestration_status: &'static str,
    to_task_status: &'static str,
}

impl SqliteOrchestrationRepository {
    /// Creates an orchestration repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        Self(pool, clock)
    }

    /// Returns the shared persistence timestamp in Unix seconds.
    fn now(&self) -> i64 {
        clock::unix_timestamp_seconds(self.1.as_ref())
    }

    async fn transition_orchestration_and_tasks(
        &self,
        id: i64,
        transition: OrchestrationTransition,
    ) -> Result<bool, DbError> {
        let now = self.now();
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
        let now = self.now();

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
            merge_position,
            prompt,
            session_orchestration_id,
            task_key,
            title,
            touched_areas,
        } = task;
        let now = self.now();
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
    merge_position,
    status,
    created_at,
    updated_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, 'Planned', ?, ?)
ON CONFLICT(session_orchestration_id, task_key) DO UPDATE
SET title = excluded.title,
    prompt = excluded.prompt,
    touched_areas = excluded.touched_areas,
    acceptance_criteria = excluded.acceptance_criteria,
    merge_position = excluded.merge_position,
    status = 'Planned',
    child_session_id = NULL,
    result_summary = NULL,
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
               CASE WHEN task.status IN ('Creating', 'Running', 'ContinuationPending')
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
       COALESCE(child.deleted_lines, 0) AS "child_deleted_lines!: i64",
       child.has_diff AS "child_has_diff?: bool",
       COALESCE(child.input_tokens, 0) AS "child_input_tokens!: i64",
       COALESCE(child.output_tokens, 0) AS "child_output_tokens!: i64",
       task.child_session_id,
       child.status AS child_status,
       child.questions AS child_questions,
       child.summary AS child_summary,
       task.continuation_generation,
       task.continuation_prompt,
       task.infrastructure_retry_count,
       task.last_error,
       task.merge_position,
       task.prompt,
       task.result_summary,
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
        let now = self.now();

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
        let now = self.now();

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

    async fn claim_orchestration_rollup(&self, id: i64) -> Result<bool, DbError> {
        let now = self.now();

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
        let now = self.now();
        let verdict = if is_pass { "Pass" } else { "Flag" };
        let result = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET verification_reason = ?,
    verification_verdict = ?,
    updated_at = ?
WHERE session_orchestration_id = ?
  AND task_key = ?
  AND status = 'Ready'
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
        let now = self.now();
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
        let now = self.now();

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
        let now = self.now();
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
        let now = self.now();

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
    ) -> Result<bool, DbError> {
        let now = self.now();
        let result = sqlx::query!(
            r"
UPDATE session_orchestration_task
SET acceptance_criteria = ?,
    area_violations = '[]',
    areas_compliant = NULL,
    continuation_generation = continuation_generation + 1,
    continuation_prompt = ?,
    status = 'ContinuationPending',
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
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn reset_orchestration_verification(&self, id: i64) -> Result<(), DbError> {
        let now = self.now();

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
        let now = self.now();
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
        let now = self.now();
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
        let now = self.now();
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
        let now = self.now();
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
        let now = self.now();

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
        let now = self.now();

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
        let now = self.now();

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

    async fn update_orchestration_task_area_compliance(
        &self,
        id: i64,
        areas_compliant: bool,
        area_violations: &str,
    ) -> Result<(), DbError> {
        let now = self.now();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::{AgentKind, ReasoningLevel, SpeedMode};
    use crate::domain::orchestration::{OrchestrationStatus, OrchestrationTaskStatus};
    use crate::infra::db::{AppRepositories, PersistedSessionCreation, SessionRow};

    /// Inserts one project plus one controller session and returns the
    /// repository bundle ready for orchestration persistence assertions.
    async fn controller_fixture() -> AppRepositories {
        let database = AppRepositories::in_memory().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id: "controller",
                is_draft: false,
                model: AgentKind::Codex.default_model().as_str(),
                orchestration_task_id: None,
                parent_session_id: None,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                role: Some("Orchestrator"),
                speed_mode: SpeedMode::Normal,
                status: "Review",
            })
            .await
            .expect("failed to insert controller session");

        database
    }

    #[tokio::test]
    async fn hydration_rejects_unknown_orchestration_status() {
        // Arrange
        let (database, pool) = AppRepositories::in_memory_with_pool().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/invalid-orchestration", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("controller", "gpt-5.6-sol", "main", "Draft", project_id)
            .await
            .expect("failed to insert controller session");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", "Running", 1)
            .await
            .expect("failed to insert orchestration");
        sqlx::query("UPDATE session_orchestration SET status = 'Unknown' WHERE id = ?")
            .bind(orchestration_id)
            .execute(&pool)
            .await
            .expect("failed to corrupt orchestration status");

        // Act
        let error = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect_err("invalid status should fail hydration");

        // Assert
        assert!(matches!(
            error,
            DbError::InvalidStatus {
                entity: "orchestration",
                value,
            } if value == "Unknown"
        ));
    }

    #[tokio::test]
    async fn rollup_recovery_failures_report_semantic_operation_context() {
        // Arrange
        let (database, pool) = AppRepositories::in_memory_with_pool().await;
        sqlx::query("DROP TABLE session_orchestration")
            .execute(&pool)
            .await
            .expect("failed to drop orchestration table");

        // Act
        let claim_error = database
            .orchestrations()
            .claim_orchestration_rollup(1)
            .await
            .expect_err("claim should fail");
        let completion_error = database
            .orchestrations()
            .complete_orchestration_rollup(1)
            .await
            .expect_err("completion should fail");

        // Assert
        assert!(matches!(
            claim_error,
            DbError::QueryContext {
                operation: "claim orchestration rollup",
                ..
            }
        ));
        assert!(matches!(
            completion_error,
            DbError::QueryContext {
                operation: "complete orchestration rollup",
                ..
            }
        ));
    }

    /// Builds one planned task payload for `orchestration_id`.
    fn planned_task(session_orchestration_id: i64, task_key: &str) -> PersistedOrchestrationTask {
        PersistedOrchestrationTask {
            acceptance_criteria: format!(r#"["Complete {task_key}"]"#),
            merge_position: 0,
            prompt: format!("Complete {task_key}"),
            session_orchestration_id,
            task_key: task_key.to_string(),
            title: format!("Task {task_key}"),
            touched_areas: format!(r#"["crates/{task_key}/"]"#),
        }
    }

    /// Persists one worker session carrying the durable reverse task link.
    async fn insert_orchestration_child(
        database: &AppRepositories,
        project_id: i64,
        session_id: &str,
        task_id: i64,
    ) {
        database
            .sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id: session_id,
                is_draft: false,
                model: AgentKind::Codex.default_model().as_str(),
                orchestration_task_id: Some(task_id),
                parent_session_id: None,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                role: Some("OrchestrationWorker"),
                speed_mode: SpeedMode::Normal,
                status: "Review",
            })
            .await
            .expect("failed to insert orchestration child");
    }

    async fn insert_waiting_orchestration_task(
        database: &AppRepositories,
        project_id: i64,
        session_orchestration_id: i64,
        task_key: &str,
        child_session_id: &str,
    ) -> i64 {
        let task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(session_orchestration_id, task_key))
            .await
            .expect("failed to insert waiting task");
        assert!(
            database
                .orchestrations()
                .claim_orchestration_task(task_id)
                .await
                .expect("failed to claim waiting task")
        );
        insert_orchestration_child(database, project_id, child_session_id, task_id).await;
        assert!(
            database
                .orchestrations()
                .link_orchestration_task_child(task_id, child_session_id)
                .await
                .expect("failed to link waiting child")
        );
        database
            .orchestrations()
            .update_orchestration_task_status(
                task_id,
                &OrchestrationTaskStatus::WaitingForInput.to_string(),
                None,
            )
            .await
            .expect("failed to wait for child question");

        task_id
    }

    async fn surface_and_clear_orchestration_questions(
        database: &AppRepositories,
        session_orchestration_id: i64,
        task_id: i64,
        questions: &str,
    ) -> bool {
        let surfaced = database
            .orchestrations()
            .surface_orchestration_questions(session_orchestration_id, task_id, questions)
            .await
            .expect("failed to surface child question");
        database
            .orchestrations()
            .clear_orchestration_questions(session_orchestration_id)
            .await
            .expect("failed to clear child question");

        surfaced
    }

    async fn load_detached_campaign_state(
        database: &AppRepositories,
        orchestration_id: i64,
    ) -> (SessionOrchestrationTaskRow, SessionRow, SessionRow) {
        let task = database
            .orchestrations()
            .load_orchestration_tasks(orchestration_id)
            .await
            .expect("failed to load detached task")
            .remove(0);
        let child = database
            .sessions()
            .load_session("child-alpha")
            .await
            .expect("failed to load detached child")
            .expect("detached child should exist");
        let controller = database
            .sessions()
            .load_session("controller")
            .await
            .expect("failed to load controller")
            .expect("controller should exist");

        (task, child, controller)
    }

    #[tokio::test]
    /// Persists one plan before any child exists and loads it back for the
    /// owning controller session.
    async fn test_planned_orchestration_round_trips_before_any_child_exists() {
        // Arrange
        let database = controller_fixture().await;

        // Act
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration(
                "controller",
                &OrchestrationStatus::AwaitingApproval.to_string(),
                3,
            )
            .await
            .expect("failed to insert orchestration");
        database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
            .await
            .expect("failed to insert task");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load orchestration")
            .expect("orchestration should exist");
        let tasks = database
            .orchestrations()
            .load_orchestration_tasks(orchestration_id)
            .await
            .expect("failed to load tasks");

        // Assert
        assert_eq!(orchestration.controller_project_id, 1);
        assert_eq!(orchestration.controller_session_id, "controller");
        assert_eq!(orchestration.max_parallelism, 3);
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::AwaitingApproval.to_string()
        );
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_key, "alpha");
        assert_eq!(tasks[0].child_session_id, None);
        assert_eq!(tasks[0].attempt_count, 0);
        assert_eq!(tasks[0].touched_areas, r#"["crates/alpha/"]"#);
    }

    #[tokio::test]
    /// Reuses the same row when a retry re-proposes an existing task key, so a
    /// respawn cannot fan out a duplicate child for one subtask.
    async fn test_retry_with_same_task_key_updates_the_existing_row() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to load project");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let first_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
            .await
            .expect("failed to insert task");
        let first_claim = database
            .orchestrations()
            .claim_orchestration_task(first_id)
            .await
            .expect("failed to claim first attempt");
        insert_orchestration_child(&database, project_id, "child-old", first_id).await;
        let first_link = database
            .orchestrations()
            .link_orchestration_task_child(first_id, "child-old")
            .await
            .expect("failed to link old child");
        database
            .orchestrations()
            .update_orchestration_task_status(
                first_id,
                &OrchestrationTaskStatus::Failed.to_string(),
                Some("agent crashed".to_string()),
            )
            .await
            .expect("failed to fail task");

        // Act
        let retried_id = database
            .orchestrations()
            .upsert_orchestration_task(PersistedOrchestrationTask {
                title: "Task alpha, retried".to_string(),
                ..planned_task(orchestration_id, "alpha")
            })
            .await
            .expect("failed to retry task");
        let detached_child = database
            .orchestrations()
            .load_child_session_id_for_task(first_id)
            .await
            .expect("failed to check detached child");
        let retry_claim = database
            .orchestrations()
            .claim_orchestration_task(retried_id)
            .await
            .expect("failed to claim replacement attempt");
        insert_orchestration_child(&database, project_id, "child-replacement", retried_id).await;
        let replacement_link = database
            .orchestrations()
            .link_orchestration_task_child(retried_id, "child-replacement")
            .await
            .expect("failed to link replacement child");
        let linked_child = database
            .orchestrations()
            .load_child_session_id_for_task(first_id)
            .await
            .expect("failed to check replacement child");
        let tasks = database
            .orchestrations()
            .load_orchestration_tasks(orchestration_id)
            .await
            .expect("failed to load tasks");

        // Assert
        assert!(first_claim);
        assert!(first_link);
        assert!(retry_claim);
        assert!(replacement_link);
        assert_eq!(retried_id, first_id);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Task alpha, retried");
        assert_eq!(
            tasks[0].status,
            OrchestrationTaskStatus::Running.to_string()
        );
        assert_eq!(tasks[0].attempt_count, 2);
        assert_eq!(
            tasks[0].child_session_id.as_deref(),
            Some("child-replacement")
        );
        assert_eq!(tasks[0].last_error, None);
        assert_eq!(detached_child, None);
        assert_eq!(linked_child.as_deref(), Some("child-replacement"));
    }

    #[tokio::test]
    /// Links a created child, counts the attempt, and exposes its observed
    /// session state through the task snapshot.
    async fn test_child_linkage_counts_attempts_and_loads_observed_state() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("child-a", "gpt-5.6-sol", "main", "Draft", project_id)
            .await
            .expect("failed to insert child session");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
            .await
            .expect("failed to insert task");
        let claimed = database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("failed to claim task");

        // Act
        let linked = database
            .orchestrations()
            .link_orchestration_task_child(task_id, "child-a")
            .await
            .expect("failed to link child");
        database
            .orchestrations()
            .update_orchestration_task_result_summary(task_id, "Added the parser")
            .await
            .expect("failed to record summary");
        let task = database
            .orchestrations()
            .load_orchestration_tasks(orchestration_id)
            .await
            .expect("failed to load task")
            .remove(0);

        // Assert
        assert!(claimed);
        assert!(linked);
        assert_eq!(task.id, task_id);
        assert_eq!(task.attempt_count, 1);
        assert_eq!(task.child_session_id.as_deref(), Some("child-a"));
        assert_eq!(task.result_summary.as_deref(), Some("Added the parser"));
        assert_eq!(task.status, OrchestrationTaskStatus::Running.to_string());
        assert_eq!(task.child_status.as_deref(), Some("Draft"));
        assert_eq!(task.child_summary, None);
        assert_eq!(task.child_input_tokens, 0);
        assert_eq!(task.child_output_tokens, 0);
    }

    #[tokio::test]
    /// Loads running, submitting, and canceling orchestrations while excluding
    /// terminal rows.
    async fn test_active_orchestration_load_includes_recoverable_states() {
        // Arrange
        let database = controller_fixture().await;
        let running_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert running orchestration");
        let submitting_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert submitting orchestration");
        let canceling_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Canceling.to_string(), 2)
            .await
            .expect("failed to insert canceling orchestration");
        let settled_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert settled orchestration");

        // Act
        database
            .orchestrations()
            .update_orchestration_status(settled_id, &OrchestrationStatus::Done.to_string())
            .await
            .expect("failed to settle orchestration");
        let first_claim = database
            .orchestrations()
            .claim_orchestration_rollup(submitting_id)
            .await
            .expect("failed to claim roll-up");
        let duplicate_claim = database
            .orchestrations()
            .claim_orchestration_rollup(submitting_id)
            .await
            .expect("failed to repeat roll-up claim");
        let active = database
            .orchestrations()
            .load_active_orchestrations()
            .await
            .expect("failed to load active orchestrations");

        // Assert
        assert!(first_claim);
        assert!(!duplicate_claim);
        assert_eq!(
            active.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![running_id, submitting_id, canceling_id]
        );
        assert_eq!(active[1].status, OrchestrationStatus::Verifying.to_string());
        assert_eq!(active[1].verification_generation, 1);
        assert_eq!(active[2].status, OrchestrationStatus::Canceling.to_string());
    }

    #[tokio::test]
    /// Uses the cancellation status as a durable barrier against both a late
    /// task claim and a late child link.
    async fn test_cancellation_barrier_blocks_fan_out_claims_and_links() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to reload project");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let late_task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "late"))
            .await
            .expect("failed to insert late task");
        let claimed_task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "claimed"))
            .await
            .expect("failed to insert claimed task");
        let initial_claim = database
            .orchestrations()
            .claim_orchestration_task(claimed_task_id)
            .await
            .expect("failed to claim task before cancellation");
        insert_orchestration_child(&database, project_id, "child-claimed", claimed_task_id).await;

        // Act
        let cancellation_started = database
            .orchestrations()
            .begin_orchestration_cancellation(orchestration_id)
            .await
            .expect("failed to begin cancellation");
        let cancellation_retried = database
            .orchestrations()
            .begin_orchestration_cancellation(orchestration_id)
            .await
            .expect("failed to retry cancellation");
        let late_claim = database
            .orchestrations()
            .claim_orchestration_task(late_task_id)
            .await
            .expect("failed to inspect late claim");
        let late_link = database
            .orchestrations()
            .link_orchestration_task_child(claimed_task_id, "child-claimed")
            .await
            .expect("failed to inspect late link");
        let rollup_completion = database
            .orchestrations()
            .complete_orchestration_rollup(orchestration_id)
            .await
            .expect("failed to inspect late roll-up completion");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load orchestration")
            .expect("orchestration should exist");
        // Assert
        assert!(initial_claim);
        assert!(cancellation_started);
        assert!(cancellation_retried);
        assert!(!late_claim);
        assert!(!late_link);
        assert!(!rollup_completion);
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::Canceling.to_string()
        );
    }

    #[tokio::test]
    /// Observes the durable worker operation until successful completion and
    /// only then settles its submitting orchestration.
    async fn test_rollup_operation_status_controls_orchestration_completion() {
        // Arrange
        let database = controller_fixture().await;
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let claimed = database
            .orchestrations()
            .claim_orchestration_rollup(orchestration_id)
            .await
            .expect("failed to claim roll-up");
        assert!(claimed);
        let operation_id = format!("orchestration-rollup-{orchestration_id}-1");

        // Act
        let missing_status = database
            .orchestrations()
            .load_rollup_operation_status(&operation_id)
            .await
            .expect("failed to load missing operation");
        database
            .operations()
            .claim_session_operation(&operation_id, "controller", "reply")
            .await
            .expect("failed to claim operation");
        let queued_status = database
            .orchestrations()
            .load_rollup_operation_status(&operation_id)
            .await
            .expect("failed to load queued operation");
        database
            .operations()
            .mark_session_operation_running(&operation_id)
            .await
            .expect("failed to run operation");
        let running_status = database
            .orchestrations()
            .load_rollup_operation_status(&operation_id)
            .await
            .expect("failed to load running operation");
        database
            .operations()
            .mark_session_operation_failed(&operation_id, "turn failed")
            .await
            .expect("failed to fail operation");
        let failed_status = database
            .orchestrations()
            .load_rollup_operation_status(&operation_id)
            .await
            .expect("failed to load failed operation");
        database
            .operations()
            .claim_session_operation(&operation_id, "controller", "reply")
            .await
            .expect("failed to reclaim operation");
        database
            .operations()
            .mark_session_operation_done(&operation_id)
            .await
            .expect("failed to complete operation");
        let done_status = database
            .orchestrations()
            .load_rollup_operation_status(&operation_id)
            .await
            .expect("failed to load completed operation");
        let completed = database
            .orchestrations()
            .complete_orchestration_rollup(orchestration_id)
            .await
            .expect("failed to complete orchestration");
        let duplicate_completion = database
            .orchestrations()
            .complete_orchestration_rollup(orchestration_id)
            .await
            .expect("failed to inspect duplicate completion");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load orchestration")
            .expect("orchestration should exist");

        // Assert
        assert_eq!(missing_status, None);
        assert_eq!(queued_status.as_deref(), Some("queued"));
        assert_eq!(running_status.as_deref(), Some("running"));
        assert_eq!(failed_status.as_deref(), Some("failed"));
        assert_eq!(done_status.as_deref(), Some("done"));
        assert!(completed);
        assert!(!duplicate_completion);
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::AwaitingIntegration.to_string()
        );
    }

    #[tokio::test]
    /// Bulk-loads controller progress and child adjacency without per-session
    /// orchestration queries.
    async fn test_session_metadata_for_project_loads_controller_and_children_together() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to reload project");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let running_task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "running"))
            .await
            .expect("failed to insert running task");
        let waiting_task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "waiting"))
            .await
            .expect("failed to insert waiting task");
        insert_orchestration_child(&database, project_id, "child-running", running_task_id).await;
        insert_orchestration_child(&database, project_id, "child-waiting", waiting_task_id).await;
        let running_claim = database
            .orchestrations()
            .claim_orchestration_task(running_task_id)
            .await
            .expect("failed to claim running task");
        let waiting_claim = database
            .orchestrations()
            .claim_orchestration_task(waiting_task_id)
            .await
            .expect("failed to claim waiting task");
        let running_link = database
            .orchestrations()
            .link_orchestration_task_child(running_task_id, "child-running")
            .await
            .expect("failed to link running child");
        let waiting_link = database
            .orchestrations()
            .link_orchestration_task_child(waiting_task_id, "child-waiting")
            .await
            .expect("failed to link waiting child");
        database
            .orchestrations()
            .update_orchestration_task_status(
                waiting_task_id,
                &OrchestrationTaskStatus::WaitingForInput.to_string(),
                None,
            )
            .await
            .expect("failed to mark waiting child");

        // Act
        let metadata = database
            .orchestrations()
            .load_session_metadata_for_project(project_id)
            .await
            .expect("failed to load bulk orchestration metadata");

        // Assert
        assert!(running_claim);
        assert!(waiting_claim);
        assert!(running_link);
        assert!(waiting_link);
        assert_eq!(
            metadata,
            vec![
                SessionOrchestrationMetadataRow {
                    controller_session_id: Some("controller".to_string()),
                    orchestration_status: None,
                    running_task_count: 0,
                    session_id: "child-running".to_string(),
                    waiting_task_count: 0,
                },
                SessionOrchestrationMetadataRow {
                    controller_session_id: Some("controller".to_string()),
                    orchestration_status: None,
                    running_task_count: 0,
                    session_id: "child-waiting".to_string(),
                    waiting_task_count: 0,
                },
                SessionOrchestrationMetadataRow {
                    controller_session_id: None,
                    orchestration_status: Some(OrchestrationStatus::Running.to_string()),
                    running_task_count: 1,
                    session_id: "controller".to_string(),
                    waiting_task_count: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn approval_persists_plan_and_releases_proposed_tasks() {
        // Arrange
        let database = controller_fixture().await;
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration(
                "controller",
                &OrchestrationStatus::AwaitingApproval.to_string(),
                2,
            )
            .await
            .expect("failed to insert orchestration");
        let task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "follow-up"))
            .await
            .expect("failed to insert proposed task");
        database
            .orchestrations()
            .update_orchestration_task_status(
                task_id,
                &OrchestrationTaskStatus::Proposed.to_string(),
                None,
            )
            .await
            .expect("failed to propose task");

        // Act
        database
            .orchestrations()
            .update_orchestration_plan(orchestration_id, "Ship the campaign", 4)
            .await
            .expect("failed to update campaign plan");
        let approved = database
            .orchestrations()
            .approve_orchestration_plan(orchestration_id)
            .await
            .expect("failed to approve campaign");
        let duplicate_approval = database
            .orchestrations()
            .approve_orchestration_plan(orchestration_id)
            .await
            .expect("failed to inspect duplicate approval");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load campaign")
            .expect("campaign should exist");
        let task = database
            .orchestrations()
            .load_orchestration_tasks(orchestration_id)
            .await
            .expect("failed to load campaign tasks")
            .remove(0);

        // Assert
        assert!(approved);
        assert!(!duplicate_approval);
        assert_eq!(orchestration.goal_statement, "Ship the campaign");
        assert_eq!(orchestration.max_parallelism, 4);
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::Running.to_string()
        );
        assert_eq!(task.status, OrchestrationTaskStatus::Planned.to_string());
    }

    #[tokio::test]
    async fn integration_approval_persists_selected_approach_atomically() {
        // Arrange
        let database = controller_fixture().await;
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration(
                "controller",
                &OrchestrationStatus::AwaitingIntegration.to_string(),
                2,
            )
            .await
            .expect("failed to insert orchestration");

        // Act
        let approved = database
            .orchestrations()
            .approve_orchestration_integration(orchestration_id, IntegrationApproach::ReviewRequest)
            .await
            .expect("failed to approve integration");
        let duplicate_approval = database
            .orchestrations()
            .approve_orchestration_integration(orchestration_id, IntegrationApproach::LocalMerge)
            .await
            .expect("failed to inspect duplicate approval");
        let approach = database
            .orchestrations()
            .load_orchestration_integration_approach(orchestration_id)
            .await
            .expect("failed to load integration approach");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load orchestration")
            .expect("orchestration should exist");

        // Assert
        assert!(approved);
        assert!(!duplicate_approval);
        assert_eq!(approach, IntegrationApproach::ReviewRequest.to_string());
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::Integrating.to_string()
        );
    }

    #[tokio::test]
    async fn managed_child_continuation_questions_and_detach_are_durable() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to reload project");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
            .await
            .expect("failed to insert task");
        assert!(
            database
                .orchestrations()
                .claim_orchestration_task(task_id)
                .await
                .expect("failed to claim task")
        );
        insert_orchestration_child(&database, project_id, "child-alpha", task_id).await;
        assert!(
            database
                .orchestrations()
                .link_orchestration_task_child(task_id, "child-alpha")
                .await
                .expect("failed to link child")
        );
        database
            .orchestrations()
            .update_orchestration_task_status(
                task_id,
                &OrchestrationTaskStatus::WaitingForInput.to_string(),
                None,
            )
            .await
            .expect("failed to wait for child question");

        // Act
        let surfaced = surface_and_clear_orchestration_questions(
            &database,
            orchestration_id,
            task_id,
            r#"[{"text":"Choose one"}]"#,
        )
        .await;
        database
            .orchestrations()
            .update_orchestration_task_status(
                task_id,
                &OrchestrationTaskStatus::Ready.to_string(),
                None,
            )
            .await
            .expect("failed to settle child after its question");
        let queued = database
            .orchestrations()
            .queue_orchestration_continuation(
                task_id,
                "Add the missing edge case",
                r#"["The edge case is tested"]"#,
            )
            .await
            .expect("failed to queue continuation");
        let duplicate_queue = database
            .orchestrations()
            .queue_orchestration_continuation(task_id, "Duplicate", r#"["Duplicate"]"#)
            .await
            .expect("failed to inspect duplicate continuation");
        let detached = database
            .orchestrations()
            .detach_orchestration_child("child-alpha")
            .await
            .expect("failed to detach child");
        let duplicate_detach = database
            .orchestrations()
            .detach_orchestration_child("child-alpha")
            .await
            .expect("failed to inspect duplicate detach");
        let (task, child, controller) =
            load_detached_campaign_state(&database, orchestration_id).await;

        // Assert
        assert!(queued);
        assert!(!duplicate_queue);
        assert!(surfaced);
        assert!(detached);
        assert!(!duplicate_detach);
        assert_eq!(task.status, OrchestrationTaskStatus::Detached.to_string());
        assert_eq!(task.child_session_id, None);
        assert_eq!(task.continuation_generation, 1);
        assert_eq!(
            task.continuation_prompt.as_deref(),
            Some("Add the missing edge case")
        );
        assert_eq!(child.role.as_deref(), Some("Worker"));
        assert_eq!(controller.status, "Review");
        assert_eq!(controller.questions.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn question_relay_preserves_controller_questions() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to reload project");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let task_id = insert_waiting_orchestration_task(
            &database,
            project_id,
            orchestration_id,
            "worker",
            "child-worker",
        )
        .await;
        let controller_question = r#"[{"text":"Controller question"}]"#;
        database
            .sessions()
            .update_session_questions("controller", controller_question)
            .await
            .expect("failed to seed controller question");

        // Act
        let blocked_by_controller = database
            .orchestrations()
            .surface_orchestration_questions(
                orchestration_id,
                task_id,
                r#"[{"text":"Child question"}]"#,
            )
            .await
            .expect("failed to inspect controller question");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load orchestration")
            .expect("orchestration should exist");
        let controller = database
            .sessions()
            .load_session("controller")
            .await
            .expect("failed to load controller")
            .expect("controller should exist");

        // Assert
        assert!(!blocked_by_controller);
        assert_eq!(orchestration.relayed_question_task_id, None);
        assert_eq!(controller.questions.as_deref(), Some(controller_question));
    }

    #[tokio::test]
    async fn question_relay_claims_one_exact_task_at_a_time() {
        // Arrange
        let database = controller_fixture().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to reload project");
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let first_task_id = insert_waiting_orchestration_task(
            &database,
            project_id,
            orchestration_id,
            "first",
            "child-first",
        )
        .await;
        let second_task_id = insert_waiting_orchestration_task(
            &database,
            project_id,
            orchestration_id,
            "second",
            "child-second",
        )
        .await;

        // Act
        let first_surfaced = database
            .orchestrations()
            .surface_orchestration_questions(
                orchestration_id,
                first_task_id,
                r#"[{"text":"First child question"}]"#,
            )
            .await
            .expect("failed to surface first child question");
        let second_blocked = database
            .orchestrations()
            .surface_orchestration_questions(
                orchestration_id,
                second_task_id,
                r#"[{"text":"Second child question"}]"#,
            )
            .await
            .expect("failed to inspect occupied relay");
        let claimed_orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load claimed orchestration")
            .expect("orchestration should exist");
        database
            .orchestrations()
            .clear_orchestration_questions(orchestration_id)
            .await
            .expect("failed to release first relay");
        let second_surfaced = database
            .orchestrations()
            .surface_orchestration_questions(
                orchestration_id,
                second_task_id,
                r#"[{"text":"Second child question"}]"#,
            )
            .await
            .expect("failed to surface second child question");
        let second_claimed_orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load second claimed orchestration")
            .expect("orchestration should exist");

        // Assert
        assert!(first_surfaced);
        assert!(!second_blocked);
        assert_eq!(
            claimed_orchestration.relayed_question_task_id,
            Some(first_task_id)
        );
        assert!(second_surfaced);
        assert_eq!(
            second_claimed_orchestration.relayed_question_task_id,
            Some(second_task_id)
        );
    }

    #[tokio::test]
    async fn infrastructure_retries_are_bounded_and_completion_is_idempotent() {
        // Arrange
        let database = controller_fixture().await;
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("failed to insert orchestration");
        let task_id = database
            .orchestrations()
            .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
            .await
            .expect("failed to insert task");

        // Act
        let first_status = database
            .orchestrations()
            .record_orchestration_spawn_failure(task_id, "provider unavailable", 2)
            .await
            .expect("failed to record first retry");
        let second_status = database
            .orchestrations()
            .record_orchestration_spawn_failure(task_id, "provider unavailable", 2)
            .await
            .expect("failed to record second retry");
        let final_status = database
            .orchestrations()
            .record_orchestration_spawn_failure(task_id, "provider unavailable", 2)
            .await
            .expect("failed to exhaust retries");
        database
            .orchestrations()
            .update_orchestration_status(
                orchestration_id,
                &OrchestrationStatus::Integrating.to_string(),
            )
            .await
            .expect("failed to begin integration completion");
        let completed = database
            .orchestrations()
            .complete_orchestration_campaign(orchestration_id)
            .await
            .expect("failed to complete campaign");
        let duplicate_completion = database
            .orchestrations()
            .complete_orchestration_campaign(orchestration_id)
            .await
            .expect("failed to inspect duplicate completion");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load completed campaign")
            .expect("completed campaign should exist");
        let task = database
            .orchestrations()
            .load_orchestration_tasks(orchestration_id)
            .await
            .expect("failed to load exhausted task")
            .remove(0);
        let controller = database
            .sessions()
            .load_session("controller")
            .await
            .expect("failed to load completed controller")
            .expect("controller should exist");

        // Assert
        assert_eq!(first_status, OrchestrationTaskStatus::Planned.to_string());
        assert_eq!(second_status, OrchestrationTaskStatus::Planned.to_string());
        assert_eq!(final_status, OrchestrationTaskStatus::Failed.to_string());
        assert!(completed);
        assert!(!duplicate_completion);
        assert_eq!(task.infrastructure_retry_count, 3);
        assert_eq!(task.status, OrchestrationTaskStatus::Failed.to_string());
        assert_eq!(orchestration.status, OrchestrationStatus::Done.to_string());
        assert_eq!(controller.status, "Done");
    }

    #[tokio::test]
    /// Returns no orchestration for a controller session that never planned.
    async fn test_missing_orchestration_returns_none() {
        // Arrange
        let database = controller_fixture().await;

        // Act
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load orchestration");
        // Assert
        assert_eq!(orchestration, None);
    }
}
