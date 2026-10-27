//! Review graph host backed by in-memory persistence and an idle utility
//! runtime. Tests inject every child call through `RunClient`.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{OneShotError, OneShotRequest};
use ag_session::{AgentKind, AgentModel, AgentSelection};
use ag_store::Database;
use ag_worker::{Clock, GraphResources, RunClient, RunWorker, RuntimeConfig};
use async_trait::async_trait;
use tokio::time::Instant;

use super::{ReviewGraphHost, ReviewGraphRequest};
use crate::app::review::ReviewProgress;

/// Session owning every fixture review graph.
pub(in crate::app) const SESSION_ID: &str = "session";

/// Graph host whose recorded runs share the fixture database.
pub(in crate::app) struct ReviewGraphFixture {
    pub(in crate::app) database: Database,
    pub(in crate::app) host: ReviewGraphHost,
    deadline: Duration,
}

impl ReviewGraphFixture {
    /// Seeds the owning project and session.
    pub(in crate::app) async fn new() -> Self {
        let database = Database::in_memory().await.expect("database");
        let project = database
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        database
            .sessions()
            .insert_session(SESSION_ID, "model", "main", "Review", project)
            .await
            .expect("session");

        Self::with_database(database)
    }

    /// Reuses a database that already owns `SESSION_ID`.
    pub(in crate::app) fn with_database(database: Database) -> Self {
        let host = ReviewGraphHost {
            drivers: Arc::default(),
            repositories: database.clone().into(),
            resources: Arc::new(GraphResources::new(
                NonZeroUsize::new(8).expect("capacity"),
                HashMap::new(),
            )),
            runs: Arc::new(RunWorker::new(
                &RuntimeConfig::default(),
                database.runs(),
                Arc::new(IdleClock),
                NonZeroUsize::MIN,
            )),
            wake: tokio::sync::watch::channel(0).0,
        };

        Self {
            database,
            deadline: Duration::from_hours(1),
            host,
        }
    }

    /// Shortens the review deadline measured from each review's start.
    pub(in crate::app) fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;

        self
    }

    /// Runs one complete review generation through the graph executor.
    pub(in crate::app) async fn review(
        &self,
        client: impl RunClient + 'static,
        request: OneShotRequest,
        diff: &str,
        context: &str,
        render: impl Fn(&str, &str, &str) -> Result<String, OneShotError> + Send + Sync + 'static,
        progress: impl Fn(ReviewProgress) + Send + Sync + 'static,
    ) -> Result<String, OneShotError> {
        self.host
            .submit(
                Arc::new(client),
                ReviewGraphRequest {
                    agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet55),
                    context: context.to_string(),
                    deadline: Instant::now() + self.deadline,
                    diff: diff.to_string(),
                    graph_id: format!("focused-review:{}", uuid::Uuid::new_v4()),
                    request,
                    session_id: SESSION_ID.to_string(),
                },
                Arc::new(render),
                Arc::new(progress),
            )
            .await
    }
}

/// Never schedules heartbeats, so paused-time tests advance only through
/// provider fixtures.
struct IdleClock;

#[async_trait]
impl Clock for IdleClock {
    async fn wait(&self) {
        std::future::pending::<()>().await;
    }
}
