//! Repository bundle wiring for the database layer.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::timestamp::TimestampSource;
use crate::{
    activity, operation, platform_task, project, remote_device, review, session, setting, usage,
};

/// App-layer repository bundle used for selective mock injection.
#[derive(Clone)]
pub struct AppRepositories {
    repositories: Arc<RepositorySet>,
}

impl AppRepositories {
    /// Returns proposal versions and atomic publication after explicit
    /// approval.
    pub fn graph_proposals(&self) -> &dyn crate::GraphProposalRepository {
        self.repositories.graph_proposal.as_ref()
    }

    /// Returns atomic static-graph admission and result settlement.
    pub fn graph_dispatch(&self) -> Arc<dyn ag_worker::GraphDispatchRepository> {
        self.repositories.graph_dispatch.clone()
    }

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

    /// Returns the tasks a platform's Sync holds for sessions.
    pub fn platform_tasks(&self) -> &dyn platform_task::PlatformTaskRepository {
        self.repositories.platform_task.as_ref()
    }

    /// Returns the project repository.
    pub fn projects(&self) -> &dyn project::ProjectRepository {
        self.repositories.project.as_ref()
    }

    /// Returns the paired remote-control device repository.
    pub fn remote_devices(&self) -> Arc<dyn remote_device::RemoteDeviceRepository> {
        Arc::clone(&self.repositories.remote_device)
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

    /// Returns each session's shared notes document.
    pub fn session_documents(&self) -> &dyn crate::SessionDocumentRepository {
        self.repositories.session_document.as_ref()
    }

    /// Returns the settings repository.
    pub fn settings(&self) -> &dyn setting::SettingRepository {
        self.repositories.setting.as_ref()
    }

    /// Returns desired outcomes and their operation-backed attempt history.
    pub fn tasks(&self) -> &dyn crate::task::TaskRepository {
        self.repositories.task.as_ref()
    }

    /// Returns task worktree Env records and their per-session limit.
    pub fn task_worktrees(&self) -> &dyn crate::TaskWorktreeRepository {
        self.repositories.task_worktree.as_ref()
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
        let graph = Arc::new(crate::execution_graph::SqliteGraphRepository::new(
            pool.clone(),
            Arc::clone(&timestamp_source),
        ));

        Self {
            repositories: Arc::new(RepositorySet {
                activity: Arc::new(activity::SqliteActivityRepository::new(pool.clone())),
                graph: graph.clone(),
                graph_dispatch: graph.clone(),
                graph_proposal: graph,
                operation: Arc::new(operation::SqliteOperationRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                platform_task: Arc::new(platform_task::SqlitePlatformTaskRepository::new(
                    pool.clone(),
                )),
                project: Arc::new(project::SqliteProjectRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                remote_device: Arc::new(remote_device::SqliteRemoteDeviceRepository::new(
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
                session_document: Arc::new(
                    crate::session_document::SqliteSessionDocumentRepository::new(
                        pool.clone(),
                        Arc::clone(&timestamp_source),
                    ),
                ),
                setting: Arc::new(setting::SqliteSettingRepository::new(pool.clone())),
                task: Arc::new(crate::task::SqliteTaskRepository::new(
                    pool.clone(),
                    Arc::clone(&timestamp_source),
                )),
                task_worktree: Arc::new(crate::task_worktree::SqliteTaskWorktreeRepository::new(
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
    graph_dispatch: Arc<dyn ag_worker::GraphDispatchRepository>,
    graph_proposal: Arc<dyn crate::GraphProposalRepository>,
    operation: Arc<dyn operation::OperationRepository<crate::DbError>>,
    platform_task: Arc<dyn platform_task::PlatformTaskRepository>,
    project: Arc<dyn project::ProjectRepository>,
    remote_device: Arc<dyn remote_device::RemoteDeviceRepository>,
    review: Arc<dyn review::ReviewRepository>,
    run: Arc<dyn ag_worker::RunRepository>,
    session: Arc<dyn session::SessionRepository>,
    session_document: Arc<dyn crate::SessionDocumentRepository>,
    setting: Arc<dyn setting::SettingRepository>,
    task: Arc<dyn crate::task::TaskRepository>,
    task_worktree: Arc<dyn crate::TaskWorktreeRepository>,
    usage: Arc<dyn usage::UsageRepository>,
}

#[cfg(test)]
#[path = "repository_test.rs"]
mod tests;

#[cfg(any(test, feature = "test-utils"))]
#[path = "repository_support_test.rs"]
mod test_support;
