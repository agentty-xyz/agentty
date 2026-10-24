//! Explicit graph launch and wakeups; execution remains owned by workers.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};
use ag_session::{
    ExecutionGraph, GraphContext, GraphNode, GraphResultRef, StartExecutionGraphRequest,
};
use ag_worker::{GraphExecutor, GraphNodeRunner, GraphRunRequest, GraphUtilityRunner, RunClient};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::app::{App, AppServices, SessionRuntimeHandle};
use crate::domain::session::Status;
use crate::infra::db::GraphEvidence;

/// Active launches are retained so redelivery cannot replace their budgets.
pub(super) struct GraphDriver {
    cancellation: CancellationToken,
    request: StartExecutionGraphRequest,
}

impl App {
    pub(super) async fn start_api_execution_graph(
        &mut self,
        request: StartExecutionGraphRequest,
    ) -> Result<(), ag_session::SessionError> {
        let invalid = ag_session::SessionError::Operation;
        request
            .graph
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        self.sessions
            .sync_session_from_handle(&request.graph.session_id);
        let session = self
            .sessions
            .session_or_err(&request.graph.session_id)
            .map_err(|error| invalid(error.to_string()))?;
        if !matches!(
            session.status,
            Status::Draft | Status::Review | Status::AgentReview | Status::InProgress
        ) || session.is_draft_session()
        {
            return Err(invalid(
                "Graph execution requires a prepared, active session".into(),
            ));
        }
        let folder = session.folder.clone();
        let _: ag_contracts::PermissionMode = self
            .services
            .db()
            .sessions()
            .load_session(&request.graph.session_id)
            .await
            .map_err(|error| invalid(error.to_string()))?
            .ok_or_else(|| invalid("Graph session is missing".into()))?
            .permission_mode
            .parse()
            .map_err(|error| invalid(format!("Invalid session permissions: {error}")))?;
        let cancellation = CancellationToken::new();
        let runner = Arc::new(SessionGraphRunner {
            cancellation: cancellation.clone(),
            runtime: self.sessions.handle(),
            utility: GraphUtilityRunner::new(self.services.graph_run_worker()),
        });
        for node in &request.graph.nodes {
            if !runner.supports(node) {
                return Err(invalid(format!("Unsupported graph node: {}", node.id)));
            }
            if let Some(id) = &node.execution.shared_budget_id
                && !request.provider_call_limits.contains_key(id)
            {
                return Err(invalid(format!("Unknown graph provider budget: {id}")));
            }
        }
        {
            let drivers = self
                .services
                .graph_drivers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(driver) = drivers.get(&request.graph.id) {
                return if driver.request == request {
                    Ok(())
                } else {
                    Err(invalid(
                        "Active graph launch has different inputs or budgets".into(),
                    ))
                };
            }
        }
        self.services
            .db()
            .graphs()
            .save_graph(&request.graph, None)
            .await
            .map_err(|error| invalid(error.to_string()))?;
        let budgets: HashMap<_, _> = request
            .provider_call_limits
            .iter()
            .map(|(id, limit)| (id.clone(), ProviderCallBudget::new(*limit)))
            .collect();
        let executor = GraphExecutor::new(
            self.services.db().graph_dispatch(),
            Arc::new(self.services.graph_resources.with_budgets(budgets)),
            runner,
        );
        self.services
            .spawn_graph_driver(request, cancellation, executor, folder);

        Ok(())
    }

    /// Acknowledges handoff only after the serial mailbox owns its completion.
    pub(super) async fn enqueue_graph_node(
        &mut self,
        request: GraphRunRequest,
        cancellation: CancellationToken,
        admitted_tx: tokio::sync::oneshot::Sender<()>,
        response_tx: tokio::sync::oneshot::Sender<Result<String, OneShotError>>,
    ) {
        if let ag_session::GraphExecutionRef::TaskAttempt { attempt_id, .. } =
            &request.dispatch.record.execution
        {
            let ag_session::EnvId::SessionWorktree(session_id) = &request.dispatch.node.env_id;
            if let Err(error) = self
                .suppress_agent_task_automation(session_id, attempt_id)
                .await
            {
                let _ = response_tx.send(Err(OneShotError::new(error.to_string())));
                let _ = admitted_tx.send(());
                return;
            }
        }
        self.sessions
            .enqueue_claimed_graph_task(&self.services, request, cancellation, response_tx)
            .await;
        let _ = admitted_tx.send(());
    }

    pub(super) async fn accept_api_execution_graph_result(
        &mut self,
        reference: GraphResultRef,
    ) -> Result<(), ag_session::SessionError> {
        let stored = self
            .services
            .db()
            .graphs()
            .load_graph(&reference.graph_id, None)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?
            .ok_or_else(|| ag_session::SessionError::Operation("Graph is missing".into()))?;
        if stored.revision != reference.graph_revision {
            return Err(ag_session::SessionError::Operation(
                "Stale graph acceptance".into(),
            ));
        }
        self.services
            .db()
            .graphs()
            .accept_result(
                &reference,
                &GraphEvidence {
                    actor: "session-api".into(),
                    reason: "Explicit acceptance of exact graph output".into(),
                },
            )
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?;
        self.services.wake_graphs();

        Ok(())
    }
}

impl AppServices {
    /// Owns one launch's budgets and wake-driven dispatch until shutdown or
    /// failure.
    fn spawn_graph_driver(
        &self,
        request: StartExecutionGraphRequest,
        cancellation: CancellationToken,
        executor: GraphExecutor,
        folder: std::path::PathBuf,
    ) {
        let services = self.clone();
        let graph = request.graph.clone();
        self.graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                graph.id.clone(),
                GraphDriver {
                    cancellation: cancellation.clone(),
                    request,
                },
            );
        self.track_cleanup_task(tokio::spawn(async move {
            services
                .drive_graph(&graph, &cancellation, &executor, &folder, || {
                    services.graph_session_permission(graph.session_id.as_str())
                })
                .await;
        }));
    }

    /// Separates readiness reads from wake coordination while retaining one
    /// driver loop for production persistence and scripted host boundaries.
    async fn drive_graph<F, P>(
        &self,
        graph: &ExecutionGraph,
        cancellation: &CancellationToken,
        executor: &GraphExecutor,
        folder: &Path,
        mut load_permission: F,
    ) where
        F: FnMut() -> P,
        P: Future<Output = Option<PermissionMode>>,
    {
        let mut wake = self.graph_wake.subscribe();
        let mut releases = self.graph_resources.subscribe_releases();
        loop {
            wake.borrow_and_update();
            releases.borrow_and_update();
            if cancellation.is_cancelled() {
                break;
            }
            let current_permission = load_permission().await;
            let Some(permission) = current_permission else {
                tokio::select! {
                    () = cancellation.cancelled() => break,
                    changed = wake.changed() => if changed.is_err() { break; },
                }
                continue;
            };
            let result = ag_worker::in_scope(
                ag_worker::RunScope {
                    cancellation: Some(cancellation.clone()),
                    session_id: Some(graph.session_id.to_string()),
                    parent_id: Some(graph.id.clone()),
                    ..ag_worker::RunScope::default()
                },
                executor.run_ready(&graph.id, graph.revision, folder, permission),
            )
            .await;
            match result {
                Ok(receipts) if !receipts.is_empty() => self.wake_graphs(),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(graph_id = graph.id, %error, "Graph dispatch failed");
                    self.wake_graphs();
                    break;
                }
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                changed = wake.changed() => if changed.is_err() { break; },
                changed = releases.changed() => if changed.is_err() { break; },
                () = executor.capacity_released() => {},
            }
        }
    }

    async fn graph_session_permission(&self, session_id: &str) -> Option<PermissionMode> {
        match self.db().sessions().load_session(session_id).await {
            Ok(Some(session))
                if matches!(
                    session.status.as_str(),
                    "Draft" | "Review" | "AgentReview" | "InProgress"
                ) =>
            {
                session.permission_mode.parse().ok()
            }
            _ => None,
        }
    }

    /// Legacy background readers retain their overlap with ordinary session
    /// work while excluding graph writers. Nested children use their serial
    /// worker's retained lease instead.
    pub(super) fn background_run_client(
        &self,
        session_id: &str,
        client: Arc<dyn RunClient>,
    ) -> Arc<dyn RunClient> {
        Arc::new(BackgroundRunClient {
            background_readers: self
                .graph_resources
                .background_readers(&ag_session::EnvId::SessionWorktree(session_id.into())),
            client,
            environment: self
                .graph_resources
                .environment(&ag_session::EnvId::SessionWorktree(session_id.into())),
            wake: self.graph_wake.clone(),
        })
    }

    pub(super) fn try_environment_write(
        &self,
        session_id: &str,
    ) -> Result<EnvironmentLease, crate::app::SessionError> {
        let writer = self
            .graph_resources
            .environment(&ag_session::EnvId::SessionWorktree(session_id.into()))
            .try_write_owned()
            .map_err(|_| crate::app::SessionError::Workflow("Session Env is busy".into()))?;

        Ok(EnvironmentLease {
            reader: None,
            writer: Some(writer),
            wake: self.graph_wake.clone(),
        })
    }

    /// Signals gate changes after persistence, including acceptance and
    /// ordinary Env release. Watch versions retain wakeups racing an
    /// in-flight drain.
    pub(super) fn wake_graphs(&self) {
        self.graph_wake
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    pub(super) fn stop_graph_drivers(&self, session_id: Option<&str>) {
        self.graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, driver| {
                let stop =
                    session_id.is_none_or(|id| driver.request.graph.session_id.as_str() == id);
                if stop {
                    driver.cancellation.cancel();
                }

                !stop
            });
    }
}

pub(super) struct EnvironmentLease {
    reader: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
    wake: tokio::sync::watch::Sender<u64>,
    writer: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

impl Drop for EnvironmentLease {
    fn drop(&mut self) {
        drop(self.reader.take());
        drop(self.writer.take());
        self.wake
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

struct BackgroundRunClient {
    background_readers: Arc<tokio::sync::RwLock<()>>,
    client: Arc<dyn RunClient>,
    environment: Arc<tokio::sync::RwLock<()>>,
    wake: tokio::sync::watch::Sender<u64>,
}

#[async_trait]
impl RunClient for BackgroundRunClient {
    async fn submit(
        &self,
        request: ag_contracts::OneShotRequest,
    ) -> Result<ag_contracts::OneShotSubmission, OneShotError> {
        let (reader, writer) = if request.permission_mode.is_read_only() {
            (
                Some(self.background_readers.clone().read_owned().await),
                None,
            )
        } else {
            (None, Some(self.environment.clone().write_owned().await))
        };
        let _lease = EnvironmentLease {
            reader,
            writer,
            wake: self.wake.clone(),
        };

        self.client.submit(request).await
    }
}

struct SessionGraphRunner {
    cancellation: CancellationToken,
    runtime: SessionRuntimeHandle,
    utility: GraphUtilityRunner,
}

#[async_trait]
impl GraphNodeRunner for SessionGraphRunner {
    fn supports(&self, node: &GraphNode) -> bool {
        match node.execution.context {
            GraphContext::Fresh => {
                self.utility.supports(node)
                    && node.execution.workspace_access
                        == ag_session::GraphWorkspaceAccess::SharedRead
                    && matches!(
                        node.execution.response_profile,
                        ag_protocol::ProtocolRequestProfile::UtilityPrompt
                            | ag_protocol::ProtocolRequestProfile::FocusedReview
                            | ag_protocol::ProtocolRequestProfile::ReviewMetadata
                    )
            }
            GraphContext::TaskConversation { .. } => node.execution.validate().is_ok(),
        }
    }

    fn manages_environment(&self, node: &GraphNode) -> bool {
        matches!(
            node.execution.context,
            GraphContext::TaskConversation { .. }
        )
    }

    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError> {
        match request.dispatch.node.execution.context {
            GraphContext::Fresh => self.utility.run(request).await,
            GraphContext::TaskConversation { .. } => {
                self.runtime
                    .run_graph_node(request, self.cancellation.child_token())
                    .await
            }
        }
    }
}

#[cfg(test)]
#[path = "execution_graph_test.rs"]
pub(crate) mod tests;
