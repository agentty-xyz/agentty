//! Repository bundle wiring for the database layer.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::timestamp::TimestampSource;
use crate::{activity, operation, project, review, session, setting, usage};

/// App-layer repository bundle used for selective mock injection.
#[derive(Clone)]
pub struct AppRepositories {
    repositories: Arc<RepositorySet>,
}

impl AppRepositories {
    /// Returns immutable graph revisions, execution links, and exact results.
    pub fn graphs(&self) -> &dyn crate::GraphRepository {
        self.repositories.graph.as_ref()
    }

    /// Returns the activity-event repository.
    pub fn activity(&self) -> &dyn activity::ActivityRepository {
        self.repositories.activity.as_ref()
    }

    /// Returns the session-operation repository.
    pub fn operations(&self) -> &dyn operation::OperationRepository<crate::DbError> {
        self.repositories.operation.as_ref()
    }

    /// Returns the project repository.
    pub fn projects(&self) -> &dyn project::ProjectRepository {
        self.repositories.project.as_ref()
    }

    /// Returns the session review-request repository.
    pub fn reviews(&self) -> &dyn review::ReviewRepository {
        self.repositories.review.as_ref()
    }

    /// Returns worker-owned utility run persistence.
    pub fn runs(&self) -> Arc<dyn ag_worker::RunRepository> {
        Arc::clone(&self.repositories.run)
    }

    /// Returns the session repository.
    pub fn sessions(&self) -> &dyn session::SessionRepository {
        self.repositories.session.as_ref()
    }

    /// Returns the settings repository.
    pub fn settings(&self) -> &dyn setting::SettingRepository {
        self.repositories.setting.as_ref()
    }

    /// Returns desired outcomes and their operation-backed attempt history.
    pub fn tasks(&self) -> &dyn crate::task::TaskRepository {
        self.repositories.task.as_ref()
    }

    /// Returns the per-session usage repository.
    pub fn usage(&self) -> &dyn usage::UsageRepository {
        self.repositories.usage.as_ref()
    }

    /// Creates a repository bundle backed by one pool and timestamp source.
    pub(crate) fn from_pool_and_timestamp_source(
        pool: SqlitePool,
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Self {
        Self {
            repositories: Arc::new(RepositorySet {
                activity: Arc::new(activity::SqliteActivityRepository::new(pool.clone())),
                graph: Arc::new(crate::execution_graph::SqliteGraphRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                operation: Arc::new(operation::SqliteOperationRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                project: Arc::new(project::SqliteProjectRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                review: Arc::new(review::SqliteReviewRepository::new(pool.clone())),
                run: Arc::new(crate::run::SqliteRunRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                session: Arc::new(session::SqliteSessionRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                setting: Arc::new(setting::SqliteSettingRepository::new(pool.clone())),
                task: Arc::new(crate::task::SqliteTaskRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                usage: Arc::new(usage::SqliteUsageRepository::new(pool, timestamp_source)),
            }),
        }
    }
}

/// Shared adapters keep the bundle's stack footprint independent of the
/// number of repositories carried through asynchronous workflows.
struct RepositorySet {
    activity: Arc<dyn activity::ActivityRepository>,
    graph: Arc<dyn crate::GraphRepository>,
    operation: Arc<dyn operation::OperationRepository<crate::DbError>>,
    project: Arc<dyn project::ProjectRepository>,
    review: Arc<dyn review::ReviewRepository>,
    run: Arc<dyn ag_worker::RunRepository>,
    session: Arc<dyn session::SessionRepository>,
    setting: Arc<dyn setting::SettingRepository>,
    task: Arc<dyn crate::task::TaskRepository>,
    usage: Arc<dyn usage::UsageRepository>,
}

#[cfg(test)]
#[path = "repository_test.rs"]
mod tests;

#[cfg(any(test, feature = "test-utils"))]
#[path = "repository_support_test.rs"]
mod test_support;
