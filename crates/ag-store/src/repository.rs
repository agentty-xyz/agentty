//! Repository bundle wiring for the database layer.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::timestamp::TimestampSource;
use crate::{activity, operation, orchestration, project, review, session, setting, usage};

/// App-layer repository bundle used for selective mock injection.
#[derive(Clone)]
pub struct AppRepositories {
    activity: Arc<dyn activity::ActivityRepository>,
    operation: Arc<dyn operation::OperationRepository>,
    orchestration: Arc<dyn orchestration::OrchestrationRepository>,
    project: Arc<dyn project::ProjectRepository>,
    review: Arc<dyn review::ReviewRepository>,
    session: Arc<dyn session::SessionRepository>,
    setting: Arc<dyn setting::SettingRepository>,
    usage: Arc<dyn usage::UsageRepository>,
}

impl AppRepositories {
    /// Returns the activity-event repository.
    pub fn activity(&self) -> &dyn activity::ActivityRepository {
        self.activity.as_ref()
    }

    /// Returns the session-operation repository.
    pub fn operations(&self) -> &dyn operation::OperationRepository {
        self.operation.as_ref()
    }

    /// Returns the orchestration repository.
    pub fn orchestrations(&self) -> &dyn orchestration::OrchestrationRepository {
        self.orchestration.as_ref()
    }

    /// Returns a cloneable orchestration repository for background
    /// reconciliation.
    pub fn orchestration_repository(&self) -> Arc<dyn orchestration::OrchestrationRepository> {
        Arc::clone(&self.orchestration)
    }

    /// Returns the project repository.
    pub fn projects(&self) -> &dyn project::ProjectRepository {
        self.project.as_ref()
    }

    /// Returns the session review-request repository.
    pub fn reviews(&self) -> &dyn review::ReviewRepository {
        self.review.as_ref()
    }

    /// Returns the session repository.
    pub fn sessions(&self) -> &dyn session::SessionRepository {
        self.session.as_ref()
    }

    /// Returns the settings repository.
    pub fn settings(&self) -> &dyn setting::SettingRepository {
        self.setting.as_ref()
    }

    /// Returns the per-session usage repository.
    pub fn usage(&self) -> &dyn usage::UsageRepository {
        self.usage.as_ref()
    }

    /// Creates a repository bundle backed by one pool and timestamp source.
    pub(crate) fn from_pool_and_timestamp_source(
        pool: SqlitePool,
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Self {
        Self {
            activity: Arc::new(activity::SqliteActivityRepository::new(pool.clone())),
            operation: Arc::new(operation::SqliteOperationRepository::new(
                pool.clone(),
                Arc::clone(&timestamp_source),
            )),
            orchestration: Arc::new(orchestration::SqliteOrchestrationRepository::new(
                pool.clone(),
                Arc::clone(&timestamp_source),
            )),
            project: Arc::new(project::SqliteProjectRepository::new(
                pool.clone(),
                Arc::clone(&timestamp_source),
            )),
            review: Arc::new(review::SqliteReviewRepository::new(pool.clone())),
            session: Arc::new(session::SqliteSessionRepository::new(
                pool.clone(),
                Arc::clone(&timestamp_source),
            )),
            setting: Arc::new(setting::SqliteSettingRepository::new(pool.clone())),
            usage: Arc::new(usage::SqliteUsageRepository::new(pool, timestamp_source)),
        }
    }
}

#[cfg(test)]
#[path = "repository_test.rs"]
mod tests;

#[cfg(any(test, feature = "test-utils"))]
#[path = "repository_support_test.rs"]
mod test_support;
