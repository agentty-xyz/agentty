//! Orchestration and orchestration-task persistence adapters.
//!
//! Task rows are written when the controller proposes a plan, before any child
//! session exists. That ordering is what makes fan-out idempotent: a restart or
//! a retry reuses the `(session_orchestration_id, task_key)` unique key instead
//! of creating a second child for the same subtask.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::infra::db::{DbError, unix_timestamp_now};

/// Row returned when loading one `session_orchestration`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOrchestrationRow {
    /// Project containing the controller and all child sessions.
    pub controller_project_id: i64,
    /// Controller session that owns this orchestration.
    pub controller_session_id: String,
    /// Stable database identifier.
    pub id: i64,
    /// Maximum number of children allowed to run at once.
    pub max_parallelism: i64,
    /// Persisted orchestration status string.
    pub status: String,
}

/// Row returned when loading one `session_orchestration_task`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOrchestrationTaskRow {
    /// Number of child sessions created for this task so far.
    pub attempt_count: i64,
    /// Total input tokens observed on the linked child session.
    pub child_input_tokens: i64,
    /// Total output tokens observed on the linked child session.
    pub child_output_tokens: i64,
    /// Child session created for this task, when one exists.
    pub child_session_id: Option<String>,
    /// Persisted lifecycle status observed on the linked child session.
    pub child_status: Option<String>,
    /// Cumulative summary observed on the linked child session.
    pub child_summary: Option<String>,
    /// Stable database identifier.
    pub id: i64,
    /// Most recent failure detail, when the task failed.
    pub last_error: Option<String>,
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

    /// Loads the durable worker-operation status for one roll-up delivery.
    async fn load_rollup_operation_status(
        &self,
        operation_id: &str,
    ) -> Result<Option<String>, DbError>;

    /// Updates one orchestration's persisted status.
    async fn update_orchestration_status(&self, id: i64, status: &str) -> Result<(), DbError>;

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
}

/// `SQLite` implementation of [`OrchestrationRepository`].
#[derive(Clone)]
pub(crate) struct SqliteOrchestrationRepository(SqlitePool);

impl SqliteOrchestrationRepository {
    /// Creates an orchestration repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
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
        let now = unix_timestamp_now();

        let row = sqlx::query!(
            r#"
INSERT INTO session_orchestration (
    controller_session_id,
    status,
    max_parallelism,
    created_at,
    updated_at
)
VALUES (?, ?, ?, ?, ?)
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
            prompt,
            session_orchestration_id,
            task_key,
            title,
            touched_areas,
        } = task;
        let now = unix_timestamp_now();
        let mut transaction = self.0.begin().await?;

        let row = sqlx::query!(
            r#"
INSERT INTO session_orchestration_task (
    session_orchestration_id,
    task_key,
    title,
    prompt,
    touched_areas,
    status,
    created_at,
    updated_at
)
VALUES (?, ?, ?, ?, ?, 'Planned', ?, ?)
ON CONFLICT(session_orchestration_id, task_key) DO UPDATE
SET title = excluded.title,
    prompt = excluded.prompt,
    touched_areas = excluded.touched_areas,
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
            now,
            now
        )
        .fetch_one(&mut *transaction)
        .await?;

        sqlx::query!(
            r"
UPDATE session
SET orchestration_task_id = NULL
WHERE orchestration_task_id = ?
",
            row.id
        )
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;

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
       orchestration.status,
       orchestration.max_parallelism
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

        Ok(row)
    }

    async fn load_active_orchestrations(&self) -> Result<Vec<SessionOrchestrationRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionOrchestrationRow,
            r#"
SELECT orchestration.id AS "id!: i64",
       session.project_id AS "controller_project_id!: i64",
       orchestration.controller_session_id,
       orchestration.status,
       orchestration.max_parallelism
FROM session_orchestration AS orchestration
INNER JOIN session
ON session.id = orchestration.controller_session_id
WHERE orchestration.status IN ('Running', 'Submitting', 'Canceling')
ORDER BY orchestration.id
"#
        )
        .fetch_all(&self.0)
        .await?;

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
               CASE WHEN task.status IN ('Creating', 'Running') THEN 1 ELSE 0 END
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
       task.attempt_count,
       COALESCE(child.input_tokens, 0) AS "child_input_tokens!: i64",
       COALESCE(child.output_tokens, 0) AS "child_output_tokens!: i64",
       task.child_session_id,
       child.status AS child_status,
       child.summary AS child_summary,
       task.last_error,
       task.prompt,
       task.result_summary,
       task.status,
       task.task_key,
       task.touched_areas,
       task.title
FROM session_orchestration_task AS task
LEFT JOIN session AS child
ON child.id = task.child_session_id
WHERE task.session_orchestration_id = ?
ORDER BY task.id
"#,
            session_orchestration_id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
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
        let now = unix_timestamp_now();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration
SET status = 'Canceling',
    updated_at = ?
WHERE id = ?
  AND status IN ('AwaitingApproval', 'Running', 'Submitting', 'Canceling')
",
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn claim_orchestration_task(&self, id: i64) -> Result<bool, DbError> {
        let now = unix_timestamp_now();

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
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn claim_orchestration_rollup(&self, id: i64) -> Result<bool, DbError> {
        let now = unix_timestamp_now();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration
SET status = 'Submitting',
    updated_at = ?
WHERE id = ?
  AND status = 'Running'
",
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    async fn complete_orchestration_rollup(&self, id: i64) -> Result<bool, DbError> {
        let now = unix_timestamp_now();

        let result = sqlx::query!(
            r"
UPDATE session_orchestration
SET status = 'Done',
    updated_at = ?
WHERE id = ?
  AND status = 'Submitting'
",
            now,
            id
        )
        .execute(&self.0)
        .await?;

        Ok(result.rows_affected() == 1)
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

        Ok(row.map(|row| row.status))
    }

    async fn update_orchestration_status(&self, id: i64, status: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

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

    async fn link_orchestration_task_child(
        &self,
        id: i64,
        child_session_id: &str,
    ) -> Result<bool, DbError> {
        let now = unix_timestamp_now();

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
        let now = unix_timestamp_now();

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
        let now = unix_timestamp_now();

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::{AgentKind, ReasoningLevel, SpeedMode};
    use crate::domain::orchestration::{OrchestrationStatus, OrchestrationTaskStatus};
    use crate::infra::db::{AppRepositories, PersistedSessionCreation};

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
            .insert_session("controller", "gpt-5.6-sol", "main", "Draft", project_id)
            .await
            .expect("failed to insert controller session");

        database
    }

    /// Builds one planned task payload for `orchestration_id`.
    fn planned_task(session_orchestration_id: i64, task_key: &str) -> PersistedOrchestrationTask {
        PersistedOrchestrationTask {
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
                role: Some("Worker"),
                speed_mode: SpeedMode::Normal,
                status: "Review",
            })
            .await
            .expect("failed to insert orchestration child");
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
        assert_eq!(
            active[1].status,
            OrchestrationStatus::Submitting.to_string()
        );
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
        let operation_id = format!("orchestration-rollup-{orchestration_id}");
        database
            .orchestrations()
            .claim_orchestration_rollup(orchestration_id)
            .await
            .expect("failed to claim roll-up");

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
        assert_eq!(orchestration.status, OrchestrationStatus::Done.to_string());
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
