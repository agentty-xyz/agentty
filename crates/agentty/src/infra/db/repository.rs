//! Repository bundle wiring for the database layer.

use std::sync::Arc;

use sqlx::SqlitePool;

#[cfg(test)]
use super::connection::open_in_memory_pool;
use super::{
    ActivityRepository, OperationRepository, OrchestrationRepository, ProjectRepository,
    ReviewRepository, SessionRepository, SettingRepository, SqliteActivityRepository,
    SqliteOperationRepository, SqliteOrchestrationRepository, SqliteProjectRepository,
    SqliteReviewRepository, SqliteSessionRepository, SqliteSettingRepository,
    SqliteUsageRepository, UsageRepository,
};
use crate::infra::clock::{self, Clock};

/// App-layer repository bundle used for selective mock injection.
#[derive(Clone)]
pub struct AppRepositories {
    activity: Arc<dyn ActivityRepository>,
    operation: Arc<dyn OperationRepository>,
    orchestration: Arc<dyn OrchestrationRepository>,
    project: Arc<dyn ProjectRepository>,
    review: Arc<dyn ReviewRepository>,
    session: Arc<dyn SessionRepository>,
    setting: Arc<dyn SettingRepository>,
    usage: Arc<dyn UsageRepository>,
}

impl AppRepositories {
    /// Creates a repository bundle backed by one shared `SQLite` pool.
    pub(crate) fn from_pool(pool: SqlitePool) -> Self {
        Self::from_pool_and_clock(pool, clock::from_environment())
    }

    /// Creates a repository bundle backed by one pool and timestamp source.
    fn from_pool_and_clock(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        Self::from_parts(AppRepositoryParts {
            activity: Arc::new(SqliteActivityRepository::new(pool.clone())),
            operation: Arc::new(SqliteOperationRepository::new(
                pool.clone(),
                Arc::clone(&clock),
            )),
            orchestration: Arc::new(SqliteOrchestrationRepository::new(
                pool.clone(),
                Arc::clone(&clock),
            )),
            project: Arc::new(SqliteProjectRepository::new(
                pool.clone(),
                Arc::clone(&clock),
            )),
            review: Arc::new(SqliteReviewRepository::new(pool.clone())),
            session: Arc::new(SqliteSessionRepository::new(
                pool.clone(),
                Arc::clone(&clock),
            )),
            setting: Arc::new(SqliteSettingRepository::new(pool.clone())),
            usage: Arc::new(SqliteUsageRepository::new(pool, clock)),
        })
    }

    /// Creates a repository bundle from a complete set of required adapters.
    pub(crate) fn from_parts(parts: AppRepositoryParts) -> Self {
        let AppRepositoryParts {
            activity,
            operation,
            orchestration,
            project,
            review,
            session,
            setting,
            usage,
        } = parts;

        Self {
            activity,
            operation,
            orchestration,
            project,
            review,
            session,
            setting,
            usage,
        }
    }

    /// Opens an isolated in-memory repository bundle for tests.
    #[cfg(test)]
    pub(crate) async fn in_memory() -> Self {
        let (repositories, _pool) = Self::in_memory_with_pool().await;

        repositories
    }

    /// Opens an isolated in-memory repository bundle plus its shared
    /// `SQLite` pool for tests that need raw SQL setup.
    #[cfg(test)]
    pub(crate) async fn in_memory_with_pool() -> (Self, SqlitePool) {
        Self::from_new_in_memory_pool().await
    }

    /// Opens an isolated in-memory repository bundle plus its shared
    /// `SQLite` pool without depending on `Database`.
    #[cfg(test)]
    pub(crate) async fn from_new_in_memory_pool() -> (Self, SqlitePool) {
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");

        (Self::from_pool(pool.clone()), pool)
    }

    /// Returns the activity-event repository.
    pub fn activity(&self) -> &dyn ActivityRepository {
        self.activity.as_ref()
    }

    /// Returns the session-operation repository.
    pub fn operations(&self) -> &dyn OperationRepository {
        self.operation.as_ref()
    }

    /// Returns the orchestration repository.
    pub fn orchestrations(&self) -> &dyn OrchestrationRepository {
        self.orchestration.as_ref()
    }

    /// Returns a cloneable orchestration repository for background
    /// reconciliation.
    pub(crate) fn orchestration_repository(&self) -> Arc<dyn OrchestrationRepository> {
        Arc::clone(&self.orchestration)
    }

    /// Returns the project repository.
    pub fn projects(&self) -> &dyn ProjectRepository {
        self.project.as_ref()
    }

    /// Returns the session review-request repository.
    pub fn reviews(&self) -> &dyn ReviewRepository {
        self.review.as_ref()
    }

    /// Returns the session repository.
    pub fn sessions(&self) -> &dyn SessionRepository {
        self.session.as_ref()
    }

    /// Returns the settings repository.
    pub(crate) fn settings(&self) -> &dyn SettingRepository {
        self.setting.as_ref()
    }

    /// Returns the per-session usage repository.
    pub fn usage(&self) -> &dyn UsageRepository {
        self.usage.as_ref()
    }
}

/// Complete repository adapter set accepted by [`AppRepositories`].
///
/// All fields are required so alternate composition cannot silently fall back
/// to a production adapter when a focused fake was intended.
pub(crate) struct AppRepositoryParts {
    pub(crate) activity: Arc<dyn ActivityRepository>,
    pub(crate) operation: Arc<dyn OperationRepository>,
    pub(crate) orchestration: Arc<dyn OrchestrationRepository>,
    pub(crate) project: Arc<dyn ProjectRepository>,
    pub(crate) review: Arc<dyn ReviewRepository>,
    pub(crate) session: Arc<dyn SessionRepository>,
    pub(crate) setting: Arc<dyn SettingRepository>,
    pub(crate) usage: Arc<dyn UsageRepository>,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    use super::super::operation::MockOperationRepository;
    use super::*;
    use crate::domain::session::{SessionDiffState, SessionStats};
    use crate::domain::session_message::SessionMessageKind;

    /// Clock fixture used to verify persistence composition deterministically.
    struct AdvancingPersistenceClock {
        next_timestamp: AtomicU64,
    }

    impl Clock for AdvancingPersistenceClock {
        fn now_instant(&self) -> Instant {
            Instant::now()
        }

        fn now_system_time(&self) -> SystemTime {
            let timestamp = self.next_timestamp.fetch_add(1, Ordering::Relaxed);

            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp)
        }
    }

    /// Clock fixture used to expose accidental fallbacks to `SQLite` wall time.
    struct FixedPersistenceClock;

    impl Clock for FixedPersistenceClock {
        fn now_instant(&self) -> Instant {
            Instant::now()
        }

        fn now_system_time(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(456)
        }
    }

    #[tokio::test]
    async fn injected_clock_drives_all_repository_timestamps() {
        // Arrange
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");
        let clock = Arc::new(AdvancingPersistenceClock {
            next_timestamp: AtomicU64::new(120),
        });
        let _clock_instant = clock.now_instant();
        let repositories = AppRepositories::from_pool_and_clock(pool.clone(), clock);

        // Act
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/injected-clock", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        repositories
            .sessions()
            .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        repositories
            .sessions()
            .append_session_message("session-a", SessionMessageKind::UserPrompt, "Persist this")
            .await
            .expect("failed to append message");
        repositories
            .operations()
            .insert_session_operation("operation-a", "session-a", "reply")
            .await
            .expect("failed to insert operation");
        let orchestration_id = repositories
            .orchestrations()
            .insert_orchestration("session-a", "Running", 2)
            .await
            .expect("failed to insert orchestration");
        repositories
            .usage()
            .upsert_session_usage(
                "session-a",
                "gpt-5.6-sol",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 1,
                    output_tokens: 2,
                },
            )
            .await
            .expect("failed to insert usage");
        let timestamps = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64, i64)>(
            r"
SELECT project.created_at,
       project.updated_at,
       session.created_at,
       session.updated_at,
       session_message.created_at,
       session_operation.queued_at,
       session_orchestration.created_at,
       session_usage.created_at
FROM project
INNER JOIN session ON session.project_id = project.id
INNER JOIN session_message ON session_message.session_id = session.id
INNER JOIN session_operation ON session_operation.session_id = session.id
INNER JOIN session_orchestration
ON session_orchestration.controller_session_id = session.id
INNER JOIN session_usage ON session_usage.session_id = session.id
WHERE session_orchestration.id = ?
",
        )
        .bind(orchestration_id)
        .fetch_one(&pool)
        .await
        .expect("failed to load timestamps");

        // Assert
        assert_eq!(timestamps, (120, 120, 121, 122, 122, 123, 124, 125));
    }

    #[tokio::test]
    async fn fixed_clock_drives_session_metadata_status_and_usage_writes() {
        // Arrange
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");
        let clock = Arc::new(FixedPersistenceClock);
        let _clock_instant = clock.now_instant();
        let repositories = AppRepositories::from_pool_and_clock(pool.clone(), clock);
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/fixed-clock", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        repositories
            .sessions()
            .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let turn_metadata = super::super::SessionTurnMetadata {
            applied_personality_id: None,
            applied_personality_prompt_hash: None,
            instruction_conversation_id: None,
            model: "gpt-5.6-sol".to_string(),
            provider_conversation_id: Some("conversation-a".to_string()),
            questions_json: "[]".to_string(),
            summary: "Persisted summary".to_string(),
            token_usage_delta: SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 3,
                output_tokens: 5,
            },
        };

        // Act
        repositories
            .sessions()
            .persist_session_turn_metadata("session-a", &turn_metadata)
            .await
            .expect("failed to persist turn metadata");
        repositories
            .sessions()
            .update_session_status_with_timing_at("session-a", "InProgress", 789)
            .await
            .expect("failed to update status");
        repositories
            .usage()
            .upsert_session_usage(
                "session-a",
                "review-model",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 7,
                    output_tokens: 11,
                },
            )
            .await
            .expect("failed to persist usage");
        let session_timestamps = sqlx::query_as::<_, (i64, i64)>(
            "SELECT updated_at, in_progress_started_at FROM session WHERE id = ?",
        )
        .bind("session-a")
        .fetch_one(&pool)
        .await
        .expect("failed to load session timestamps");
        let usage_timestamps = sqlx::query_scalar::<_, i64>(
            "SELECT created_at FROM session_usage WHERE session_id = ? ORDER BY model",
        )
        .bind("session-a")
        .fetch_all(&pool)
        .await
        .expect("failed to load usage timestamps");

        // Assert
        assert_eq!(session_timestamps, (456, 789));
        assert_eq!(usage_timestamps, vec![456, 456]);
    }

    #[tokio::test]
    async fn repository_parts_support_focused_adapter_injection() {
        // Arrange
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");
        let baseline = AppRepositories::from_pool(pool);
        let mut operation = MockOperationRepository::new();
        operation
            .expect_is_cancel_requested_for_operation()
            .withf(|operation_id| operation_id == "operation-a")
            .times(1)
            .returning(|_| Ok(true));
        let repositories = AppRepositories::from_parts(AppRepositoryParts {
            activity: Arc::clone(&baseline.activity),
            operation: Arc::new(operation),
            orchestration: Arc::clone(&baseline.orchestration),
            project: Arc::clone(&baseline.project),
            review: Arc::clone(&baseline.review),
            session: Arc::clone(&baseline.session),
            setting: Arc::clone(&baseline.setting),
            usage: Arc::clone(&baseline.usage),
        });

        // Act
        let is_cancel_requested = repositories
            .operations()
            .is_cancel_requested_for_operation("operation-a")
            .await
            .expect("mock operation query should succeed");

        // Assert
        assert!(is_cancel_requested);
    }
}
