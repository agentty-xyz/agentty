//! Repository bundle wiring for the database layer.

use std::sync::Arc;

use sqlx::SqlitePool;

#[cfg(test)]
use super::connection::open_in_memory_pool;
use super::{
    ActivityRepository, OperationRepository, ProjectRepository, ReviewRepository,
    SessionRepository, SettingRepository, SqliteActivityRepository, SqliteOperationRepository,
    SqliteProjectRepository, SqliteReviewRepository, SqliteSessionRepository,
    SqliteSettingRepository, SqliteUsageRepository, UsageRepository,
};

/// App-layer repository bundle used for selective mock injection.
#[derive(Clone)]
pub struct AppRepositories {
    activity: Arc<dyn ActivityRepository>,
    operation: Arc<dyn OperationRepository>,
    project: Arc<dyn ProjectRepository>,
    review: Arc<dyn ReviewRepository>,
    session: Arc<dyn SessionRepository>,
    setting: Arc<dyn SettingRepository>,
    usage: Arc<dyn UsageRepository>,
}

impl AppRepositories {
    /// Creates a repository bundle from explicit repository implementations.
    pub(crate) fn new(
        activity: Arc<dyn ActivityRepository>,
        operation: Arc<dyn OperationRepository>,
        project: Arc<dyn ProjectRepository>,
        review: Arc<dyn ReviewRepository>,
        session: Arc<dyn SessionRepository>,
        setting: Arc<dyn SettingRepository>,
        usage: Arc<dyn UsageRepository>,
    ) -> Self {
        Self {
            activity,
            operation,
            project,
            review,
            session,
            setting,
            usage,
        }
    }

    /// Creates a repository bundle backed by one shared `SQLite` pool.
    pub(crate) fn from_pool(pool: SqlitePool) -> Self {
        Self::new(
            Arc::new(SqliteActivityRepository::new(pool.clone())),
            Arc::new(SqliteOperationRepository::new(pool.clone())),
            Arc::new(SqliteProjectRepository::new(pool.clone())),
            Arc::new(SqliteReviewRepository::new(pool.clone())),
            Arc::new(SqliteSessionRepository::new(pool.clone())),
            Arc::new(SqliteSettingRepository::new(pool.clone())),
            Arc::new(SqliteUsageRepository::new(pool)),
        )
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
