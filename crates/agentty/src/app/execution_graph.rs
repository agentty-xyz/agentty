//! Explicit graph launch and wakeups; execution remains owned by workers.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};
use ag_session::{
    ContinueGraphNodeRequest, ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus,
    GraphBlockedReason, GraphContext, GraphControlRequest, GraphExecutionRef, GraphNode,
    GraphNodeStatus, GraphResult, GraphResultRef, ResumeExecutionGraphRequest,
    StartExecutionGraphRequest,
};
use ag_worker::{
    GraphExecutionRecord, GraphExecutor, GraphNodeRunner, GraphRunRequest, GraphUtilityRunner,
};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::app::graph_activity::GraphActivity;
use crate::app::graph_budget::GraphBudgetLedger;
use crate::app::review_graph::ReviewGraphHost;
use crate::app::session::SessionTaskService;
use crate::app::{App, AppServices, SessionRuntimeHandle, spawn};
use crate::domain::session::{SessionId, Status};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::GraphEvidence;
use crate::presentation::execution_graph::{
    GraphViewOutput, outputs_awaiting_acceptance, takes_revision_guidance,
};
use crate::presentation::graph_chat::output_approval_notice;

/// Launches retain their executor and budgets even after dispatch stops.
pub(super) struct GraphDriver {
    cancellation: CancellationToken,
    executor: Arc<GraphExecutor>,
    /// Whether the graph does the session's own work, which automatic review
    /// can wait for. Host-driven graphs, such as the review itself, do not.
    is_session_work: bool,
    /// Status leases the executor's runner takes, kept so a relaunched
    /// driver settles them.
    leases: GraphNodeLeases,
    request: StartExecutionGraphRequest,
    running: Arc<AtomicBool>,
}

impl GraphDriver {
    /// Whether this driver still dispatches newly admitted work.
    fn is_live(&self) -> bool {
        self.running.load(Ordering::Acquire) && !self.cancellation.is_cancelled()
    }
}

/// Session state a graph driver works in and reports to.
pub(super) struct GraphDriverSession {
    /// Session worktree the graph's nodes run in.
    pub(super) folder: PathBuf,
    /// Status leases the driver settles after each dispatch pass.
    pub(super) leases: GraphNodeLeases,
    /// Live session chat that receives the driver's notices.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

/// Status leases taken by one driver's running nodes.
///
/// The driver finishes them only after a dispatch pass settles every node it
/// admitted, so a session whose graph still runs nodes stays `InProgress`,
/// including between a node and the dependent node it unblocks.
#[derive(Clone, Default)]
pub(super) struct GraphNodeLeases(Arc<Mutex<Vec<GraphActivity>>>);

impl GraphNodeLeases {
    fn hold(&self, activity: GraphActivity) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(activity);
    }

    async fn finish(&self, services: &AppServices) {
        let leases = std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for lease in leases {
            lease.finish(services).await;
        }
    }
}

/// Holds the session `InProgress` from the moment one of its graph's nodes
/// starts until the driver settles the dispatch pass that ran it.
///
/// Utility nodes never pass through the session worker, so nothing else
/// reports their work in the session status.
struct SessionStatusGraphRunner {
    leases: GraphNodeLeases,
    runner: Arc<dyn GraphNodeRunner>,
    services: AppServices,
    session_id: SessionId,
    status: Arc<Mutex<Status>>,
}

#[async_trait]
impl GraphNodeRunner for SessionStatusGraphRunner {
    fn supports(&self, node: &GraphNode) -> bool {
        self.runner.supports(node)
    }

    fn manages_environment(&self, node: &GraphNode) -> bool {
        self.runner.manages_environment(node)
    }

    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError> {
        let activity = self
            .services
            .begin_graph_node_activity(&self.session_id, Some(Arc::clone(&self.status)));
        activity.record(&self.services).await;
        self.leases.hold(activity);

        self.runner.run(request).await
    }
}

/// Active graph drivers keyed by graph identity, shared with lifecycle
/// controls and session cleanup.
pub(super) type GraphDrivers = Arc<Mutex<HashMap<String, GraphDriver>>>;

/// Publishes a host-driven graph to lifecycle cancellation and session
/// cleanup while it dispatches. Dropping it unregisters the driver.
pub(super) struct GraphDriverRegistration {
    /// Canceled by `cancel_execution_graph` or by stopping the session.
    pub(super) cancellation: CancellationToken,
    drivers: GraphDrivers,
    graph_id: String,
}

impl GraphDriverRegistration {
    /// Registers a running driver without provider limits; host phases
    /// enforce their own budgets.
    pub(super) fn register(
        drivers: &GraphDrivers,
        graph: &ExecutionGraph,
        executor: Arc<GraphExecutor>,
    ) -> Self {
        let cancellation = CancellationToken::new();
        drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                graph.id.clone(),
                GraphDriver {
                    cancellation: cancellation.clone(),
                    executor,
                    is_session_work: false,
                    leases: GraphNodeLeases::default(),
                    request: StartExecutionGraphRequest {
                        graph: graph.clone(),
                        provider_call_limits: std::collections::BTreeMap::new(),
                    },
                    running: Arc::new(AtomicBool::new(true)),
                },
            );

        Self {
            cancellation,
            drivers: drivers.clone(),
            graph_id: graph.id.clone(),
        }
    }
}

impl Drop for GraphDriverRegistration {
    fn drop(&mut self) {
        self.drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.graph_id);
    }
}

impl App {
    /// Whether a live driver dispatches the graph's admitted continuations;
    /// otherwise they wait for an explicit resume.
    pub(super) fn has_live_graph_driver(&self, graph_id: &str) -> bool {
        self.services
            .graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(graph_id)
            .is_some_and(GraphDriver::is_live)
    }

    /// Whether automatic review of `session_id` waits for a graph: a live
    /// driver dispatches a graph that does the session's own work, and that
    /// graph can still finish. The review then covers the finished graph
    /// instead of a diff between two nodes.
    ///
    /// A paused graph has no live driver, and a graph that failed or whose
    /// pending node spent its budget cannot finish until the user retries or
    /// cancels it. Neither holds a review, so work the user continues outside
    /// them is still reviewed.
    pub(super) async fn graph_holds_automatic_review(&self, session_id: &str) -> bool {
        for graph_id in self.services.live_session_work_graphs(session_id) {
            let can_finish = self
                .execution_graph_progress(&graph_id)
                .await
                .is_ok_and(|progress| progress.as_ref().is_some_and(can_still_finish));
            if can_finish {
                return true;
            }
        }

        false
    }

    /// Projects persisted graph progress with the live driver's state: nodes
    /// it is still settling stay running, pending nodes report spent budgets,
    /// and a graph without a live driver is paused.
    ///
    /// A run turns terminal before its settlement stores the graph response,
    /// so the persisted projection alone reads a settling node as failed. Only
    /// a settlement no live executor owns, such as one interrupted by a
    /// restart or one that failed to store, remains failed.
    pub(super) async fn execution_graph_progress(
        &self,
        graph_id: &str,
    ) -> Result<Option<ExecutionGraphProgress>, ag_session::SessionError> {
        // Sampled before the read, so the read reflects the settlement of any
        // execution missing from the sample.
        let unsettled = self
            .services
            .graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(graph_id)
            .filter(|driver| driver.is_live())
            .map(|driver| driver.executor.unsettled_executions());
        let Some(mut progress) = self
            .services
            .db()
            .graphs()
            .progress(graph_id)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let drivers = self
            .services
            .graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let driver = drivers.get(graph_id).filter(|driver| driver.is_live());
        if let Some(unsettled) = &unsettled {
            project_unsettled_nodes_running(&mut progress, unsettled);
        }
        for node in &mut progress.nodes {
            if node.status != GraphNodeStatus::Pending {
                continue;
            }
            if let Some(driver) = driver {
                let budget = driver
                    .request
                    .graph
                    .nodes
                    .iter()
                    .find(|definition| definition.id == node.node_id)
                    .and_then(|definition| definition.execution.shared_budget_id.as_ref());
                if let Some(id) = budget
                    && driver.executor.remaining_provider_calls(id) == Some(0)
                {
                    node.blocked
                        .push(GraphBlockedReason::BudgetExhausted(id.clone()));
                }
            } else {
                node.blocked.push(GraphBlockedReason::ResumeRequired);
            }
        }
        if matches!(
            progress.status,
            ExecutionGraphStatus::Running | ExecutionGraphStatus::Blocked
        ) {
            if driver.is_none() {
                progress.status = ExecutionGraphStatus::Paused;
            } else if progress.nodes.iter().all(|node| {
                node.status != GraphNodeStatus::Pending
                    || node
                        .blocked
                        .iter()
                        .any(|reason| !matches!(reason, GraphBlockedReason::AwaitingDispatch))
            }) && !progress.nodes.iter().any(|node| {
                matches!(
                    node.status,
                    GraphNodeStatus::Queued | GraphNodeStatus::Running
                )
            }) {
                progress.status = ExecutionGraphStatus::Blocked;
            }
        }

        Ok(Some(progress))
    }

    pub(super) async fn cancel_api_execution_graph(
        &mut self,
        request: GraphControlRequest,
    ) -> Result<(), ag_session::SessionError> {
        self.services
            .db()
            .graphs()
            .cancel(&request)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?;
        if let Some(driver) = self
            .services
            .graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&request.graph_id)
        {
            driver.cancellation.cancel();
        }
        self.services.wake_graphs();

        Ok(())
    }

    pub(super) async fn resume_api_execution_graph(
        &mut self,
        request: ResumeExecutionGraphRequest,
    ) -> Result<(), ag_session::SessionError> {
        let graph = self
            .services
            .db()
            .graphs()
            .load_graph(&request.graph.graph_id, None)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?
            .filter(|graph| graph.revision == request.graph.revision)
            .ok_or_else(|| ag_session::SessionError::Operation("Missing or stale graph".into()))?;
        // Limits were fixed at first launch; the executor resumes from the
        // credit left after recorded spending.
        let provider_call_limits = self
            .services
            .db()
            .graphs()
            .budget_limits(&graph.id)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?;
        if provider_call_limits.is_empty()
            && graph
                .nodes
                .iter()
                .any(|node| node.execution.shared_budget_id.is_some())
        {
            return Err(ag_session::SessionError::Operation(
                "This graph ran before provider-call spending was recorded; cancel it and start a \
                 new one"
                    .into(),
            ));
        }
        self.start_api_execution_graph(StartExecutionGraphRequest {
            graph,
            provider_call_limits,
        })
        .await
    }

    /// Records an explicit budget extension, then adds its calls to a
    /// retained driver's live budget and wakes dispatch. Recording comes
    /// first, so spending charged against the new calls stays within the
    /// stored limit; a graph without a driver picks the credit up when it is
    /// resumed. A redelivered extension adds nothing.
    pub(super) async fn extend_api_execution_graph_budget(
        &mut self,
        request: ag_session::ExtendGraphBudgetRequest,
    ) -> Result<(), ag_session::SessionError> {
        let invalid = ag_session::SessionError::Operation;
        let status = self
            .services
            .db()
            .graphs()
            .progress(&request.graph.graph_id)
            .await
            .map_err(|error| invalid(error.to_string()))?
            .ok_or_else(|| invalid("Graph is missing".into()))?
            .status;
        if status == ExecutionGraphStatus::Completed {
            return Err(invalid(
                "Completed graphs need no more provider calls".into(),
            ));
        }
        let is_recorded = self
            .services
            .db()
            .graphs()
            .extend_budget(&request)
            .await
            .map_err(|error| invalid(error.to_string()))?;
        if is_recorded
            && let Some(driver) = self
                .services
                .graph_drivers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&request.graph.graph_id)
        {
            driver
                .executor
                .extend_provider_calls(&request.budget_id, request.additional_calls);
        }
        self.services.wake_graphs();

        Ok(())
    }

    pub(super) async fn continue_api_execution_graph_node(
        &mut self,
        request: ContinueGraphNodeRequest,
    ) -> Result<(), ag_session::SessionError> {
        self.services
            .db()
            .graphs()
            .continue_node(&request)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?;
        // Durable intent survives restart. Dispatch only wakes an explicitly
        // launched driver; recording a retry cannot recreate budgets on its
        // own.
        self.services.wake_graphs();

        Ok(())
    }

    pub(super) async fn start_api_execution_graph(
        &mut self,
        request: StartExecutionGraphRequest,
    ) -> Result<(), ag_session::SessionError> {
        let invalid = ag_session::SessionError::Operation;
        self.validate_proposal_launch(&request).await?;
        request
            .graph
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        if self
            .services
            .db()
            .graphs()
            .progress(&request.graph.id)
            .await
            .map_err(|error| invalid(error.to_string()))?
            .is_some_and(|progress| progress.status == ExecutionGraphStatus::Cancelled)
        {
            return Err(invalid("Canceled graphs cannot be resumed".into()));
        }
        self.sessions
            .sync_session_from_handle(&request.graph.session_id);
        let (session, handles) = self
            .sessions
            .session_and_handles_or_err(&request.graph.session_id)
            .map_err(|error| invalid(error.to_string()))?;
        if !matches!(
            session.status,
            Status::Draft
                | Status::Review
                | Status::AgentReview
                | Status::Watching
                | Status::InProgress
        ) || session.is_draft_session()
        {
            return Err(invalid(
                "Graph execution requires a prepared, active session".into(),
            ));
        }
        let folder = session.folder.clone();
        let status = Arc::clone(&handles.status);
        let transcript = Arc::clone(&handles.transcript);
        self.services
            .validate_graph_session_permissions(&request.graph.session_id)
            .await?;
        let cancellation = CancellationToken::new();
        let runner = Arc::new(SessionGraphRunner {
            cancellation: cancellation.clone(),
            runtime: self.sessions.handle(),
            utility: GraphUtilityRunner::new(self.services.graph_run_worker()),
        });
        runner.validate_launch(&request)?;
        let previous = {
            let drivers = self
                .services
                .graph_drivers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(driver) = drivers.get(&request.graph.id) {
                if driver.request != request {
                    return Err(invalid(
                        "Active graph launch has different inputs or budgets".into(),
                    ));
                }

                Some((
                    driver.cancellation.clone(),
                    driver.executor.clone(),
                    driver.leases.clone(),
                    driver.running.clone(),
                ))
            } else {
                None
            }
        };
        if let Some((cancellation, executor, leases, running)) = previous {
            self.services.spawn_graph_driver_task(
                request.graph,
                cancellation,
                executor,
                running,
                GraphDriverSession {
                    folder,
                    leases,
                    transcript,
                },
            );

            return Ok(());
        }
        self.services
            .launch_graph_driver(
                request,
                cancellation,
                runner,
                status,
                GraphDriverSession {
                    folder,
                    leases: GraphNodeLeases::default(),
                    transcript,
                },
            )
            .await
    }

    /// Acknowledges handoff only after the serial mailbox owns its completion.
    pub(super) async fn enqueue_graph_node(
        &mut self,
        request: GraphRunRequest,
        cancellation: CancellationToken,
        admitted_tx: tokio::sync::oneshot::Sender<()>,
        response_tx: tokio::sync::oneshot::Sender<Result<String, OneShotError>>,
    ) {
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
    /// Wakes resource-blocked graphs after a detached utility settles.
    ///
    /// Active session utilities hold `agent_run` rows that block graph Env
    /// claims, so their completion must wake drivers like a released lease.
    pub(super) fn graph_waking_run_client(
        &self,
        client: Arc<dyn ag_worker::RunClient>,
    ) -> Arc<dyn ag_worker::RunClient> {
        Arc::new(GraphWakeClient {
            client,
            wake: self.graph_wake.clone(),
        })
    }

    async fn validate_graph_session_permissions(
        &self,
        session_id: &str,
    ) -> Result<(), ag_session::SessionError> {
        let invalid = ag_session::SessionError::Operation;
        let session = self
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| invalid(error.to_string()))?
            .ok_or_else(|| invalid("Graph session is missing".into()))?;
        let _: PermissionMode = session
            .permission_mode
            .parse()
            .map_err(|error| invalid(format!("Invalid session permissions: {error}")))?;

        Ok(())
    }

    /// Saves the launch and opens its ledgered budgets from the credit the
    /// store has left.
    ///
    /// # Errors
    /// Rejects the launch when it cannot be saved, and while a stopped driver
    /// of the same graph still holds spending the store has not recorded,
    /// because the stored credit would hand those calls back.
    async fn launch_graph_budgets(
        &self,
        request: &StartExecutionGraphRequest,
    ) -> Result<HashMap<String, ProviderCallBudget>, ag_session::SessionError> {
        let graph_id = &request.graph.id;
        if self.graph_budget_writers.has_unrecorded_spending(graph_id) {
            return Err(ag_session::SessionError::Operation(
                "Earlier provider-call spending for this graph is not recorded yet; resume it \
                 again shortly"
                    .into(),
            ));
        }
        let remaining = self
            .db()
            .graphs()
            .launch_graph(&request.graph, &request.provider_call_limits)
            .await
            .map_err(|error| ag_session::SessionError::Operation(error.to_string()))?;

        Ok(remaining
            .into_iter()
            .map(|(id, calls)| {
                let budget = GraphBudgetLedger::budget(self, graph_id, &id, calls);

                (id, budget)
            })
            .collect())
    }

    /// Opens a first launch's budgets and starts its driver, whose running
    /// nodes hold the session `status` in `InProgress`.
    async fn launch_graph_driver(
        &self,
        request: StartExecutionGraphRequest,
        cancellation: CancellationToken,
        runner: Arc<dyn GraphNodeRunner>,
        status: Arc<Mutex<Status>>,
        session: GraphDriverSession,
    ) -> Result<(), ag_session::SessionError> {
        let budgets = self.launch_graph_budgets(&request).await?;
        let executor = GraphExecutor::new(
            self.db().graph_dispatch(),
            Arc::new(self.graph_resources.with_budgets(budgets)),
            Arc::new(SessionStatusGraphRunner {
                leases: session.leases.clone(),
                runner,
                services: self.clone(),
                session_id: request.graph.session_id.clone(),
                status,
            }),
        );
        self.spawn_graph_driver(request, cancellation, executor, session);

        Ok(())
    }

    /// Retains a launch's executor and budgets across dispatch exits. Only an
    /// explicit relaunch restarts a stopped driver.
    fn spawn_graph_driver(
        &self,
        request: StartExecutionGraphRequest,
        cancellation: CancellationToken,
        executor: GraphExecutor,
        session: GraphDriverSession,
    ) {
        let services = self.clone();
        let graph = request.graph.clone();
        let executor = Arc::new(executor);
        let running = Arc::new(AtomicBool::new(false));
        self.graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                graph.id.clone(),
                GraphDriver {
                    cancellation: cancellation.clone(),
                    executor: executor.clone(),
                    is_session_work: true,
                    leases: session.leases.clone(),
                    request,
                    running: running.clone(),
                },
            );
        services.spawn_graph_driver_task(graph, cancellation, executor, running, session);
    }

    fn spawn_graph_driver_task(
        &self,
        graph: ExecutionGraph,
        cancellation: CancellationToken,
        executor: Arc<GraphExecutor>,
        running: Arc<AtomicBool>,
        session: GraphDriverSession,
    ) {
        if running.swap(true, Ordering::AcqRel) {
            return;
        }
        let activity = GraphDriverActivity(running);
        let services = self.clone();
        self.track_cleanup_task(spawn::spawn_detached(async move {
            services
                .drive_graph(&graph, &cancellation, &executor, &session, || {
                    services.graph_session_permission(graph.session_id.as_str())
                })
                .await;
            // The stopped driver no longer holds the session's automatic
            // review; the update lets the reducer start a deferred one.
            drop(activity);
            SessionTaskService::emit_session_updated(
                &services.event_sender(),
                &services.session_update_versions(),
                &graph.session_id,
            );
        }));
    }

    /// Separates readiness reads from wake coordination while retaining one
    /// driver loop for production persistence and scripted host boundaries.
    async fn drive_graph<F, P>(
        &self,
        graph: &ExecutionGraph,
        cancellation: &CancellationToken,
        executor: &GraphExecutor,
        session: &GraphDriverSession,
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
                executor.run_ready(&graph.id, graph.revision, &session.folder, permission),
            )
            .await;
            // The pass settled every node it admitted; a node that becomes
            // ready later takes a new lease when it starts.
            session.leases.finish(self).await;
            match result {
                Ok(receipts) if !receipts.is_empty() => {
                    self.announce_outputs_awaiting_acceptance(
                        graph,
                        &receipts,
                        &session.transcript,
                    )
                    .await;
                    // Settlement stores results after the worker's own status
                    // updates; refresh session views that project them.
                    SessionTaskService::emit_session_updated(
                        &self.event_sender(),
                        &self.session_update_versions(),
                        &graph.session_id,
                    );
                    self.wake_graphs();
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(graph_id = graph.id, %error, "Graph dispatch failed");
                    // Keep the driver subscribed for explicit retry intent.
                    // Existing receipts still prevent automatic node replay.
                    let has_execution = self
                        .db()
                        .graphs()
                        .progress(&graph.id)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|progress| {
                            progress
                                .nodes
                                .iter()
                                .any(|node| !node.executions.is_empty())
                        });
                    if !has_execution {
                        break;
                    }
                    // A failed graph no longer holds the session's automatic
                    // review; the update lets the reducer start a deferred one.
                    SessionTaskService::emit_session_updated(
                        &self.event_sender(),
                        &self.session_update_versions(),
                        &graph.session_id,
                    );
                }
            }
            // Dispatch can be idle at an acceptance or clarification gate;
            // only a persisted complete projection proves completion.
            if self
                .db()
                .graphs()
                .progress(&graph.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|progress| {
                    matches!(
                        progress.status,
                        ExecutionGraphStatus::Completed | ExecutionGraphStatus::Cancelled
                    )
                })
            {
                break;
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                changed = wake.changed() => if changed.is_err() { break; },
                changed = releases.changed() => if changed.is_err() { break; },
                () = executor.capacity_released() => {},
                () = executor.environment_released() => {},
            }
        }
    }

    /// Tells the user in chat when a settled execution leaves its output
    /// waiting for their approval, showing that output with the reply
    /// guidance the sidebar gives.
    ///
    /// Only outputs of executions in `receipts` are announced, so restarting
    /// a driver never repeats an earlier notice.
    async fn announce_outputs_awaiting_acceptance(
        &self,
        graph: &ExecutionGraph,
        receipts: &[GraphExecutionRecord],
        transcript: &Arc<Mutex<SessionTranscript>>,
    ) {
        let repository = self.db().graphs();
        let progress = repository.progress(&graph.id).await.ok().flatten();
        let awaiting: Vec<(String, GraphResult, bool)> = progress
            .iter()
            .flat_map(|progress| {
                outputs_awaiting_acceptance(progress)
                    .into_iter()
                    .filter(|(producer, _)| {
                        producer.executions.last().is_some_and(|execution| {
                            receipts
                                .iter()
                                .any(|receipt| receipt.execution == *execution)
                        })
                    })
                    .map(|(producer, result)| {
                        (
                            producer.node_id.clone(),
                            result.clone(),
                            takes_revision_guidance(&graph.nodes, progress, &producer.node_id),
                        )
                    })
            })
            .collect();
        for (node_id, result, takes_revision) in awaiting {
            let body = repository
                .load_result(&result.reference.id)
                .await
                .ok()
                .flatten()
                .map_or_default(|stored| stored.body);
            let output = GraphViewOutput::new(result, body);
            let notice = output_approval_notice(&node_id, output.display(), takes_revision);
            SessionTaskService::append_workflow_notice(
                transcript,
                self.db(),
                &self.event_sender(),
                &self.session_update_versions(),
                &graph.session_id,
                &notice,
            )
            .await;
        }
    }

    async fn graph_session_permission(&self, session_id: &str) -> Option<PermissionMode> {
        match self.db().sessions().load_session(session_id).await {
            Ok(Some(session))
                if matches!(
                    session.status.as_str(),
                    "Draft" | "Review" | "AgentReview" | "Watching" | "InProgress"
                ) =>
            {
                session.permission_mode.parse().ok()
            }
            _ => None,
        }
    }

    /// Focused reviews run as background-reader graphs that overlap ordinary
    /// session work while excluding graph writers.
    pub(super) fn review_graph_host(&self) -> ReviewGraphHost {
        ReviewGraphHost {
            drivers: self.graph_drivers.clone(),
            repositories: self.db().clone(),
            resources: self.graph_resources.clone(),
            runs: self.graph_run_worker(),
            wake: self.graph_wake.clone(),
        }
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

    /// Lists the graphs with a live driver that do `session_id`'s own work.
    /// Host-driven graphs, such as focused review, are never listed.
    pub(super) fn live_session_work_graphs(&self, session_id: &str) -> Vec<String> {
        self.graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, driver)| {
                driver.is_session_work
                    && driver.is_live()
                    && driver.request.graph.session_id.as_str() == session_id
            })
            .map(|(graph_id, _)| graph_id.clone())
            .collect()
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

/// Reports nodes whose latest execution is still `unsettled` as running
/// instead of failed, and recomputes a graph status that only those nodes made
/// failed.
fn project_unsettled_nodes_running(
    progress: &mut ExecutionGraphProgress,
    unsettled: &[GraphExecutionRef],
) {
    for node in &mut progress.nodes {
        if node.status == GraphNodeStatus::Failed
            && node
                .executions
                .last()
                .is_some_and(|execution| unsettled.contains(execution))
        {
            node.status = GraphNodeStatus::Running;
            node.last_error = None;
        }
    }
    if progress.status == ExecutionGraphStatus::Failed
        && !progress.nodes.iter().any(|node| {
            matches!(
                node.status,
                GraphNodeStatus::Failed | GraphNodeStatus::Cancelled
            )
        })
    {
        progress.status = ExecutionGraphStatus::Running;
    }
}

/// Whether dispatch can still finish the graph without the user retrying or
/// cancelling it: the graph runs or waits at a gate, and no pending node is
/// out of provider calls.
fn can_still_finish(progress: &ExecutionGraphProgress) -> bool {
    matches!(
        progress.status,
        ExecutionGraphStatus::Running | ExecutionGraphStatus::Blocked
    ) && !progress.nodes.iter().any(|node| {
        node.blocked
            .iter()
            .any(|reason| matches!(reason, GraphBlockedReason::BudgetExhausted(_)))
    })
}

struct GraphWakeClient {
    client: Arc<dyn ag_worker::RunClient>,
    wake: tokio::sync::watch::Sender<u64>,
}

#[async_trait]
impl ag_worker::RunClient for GraphWakeClient {
    async fn submit(
        &self,
        request: ag_contracts::OneShotRequest,
    ) -> Result<ag_contracts::OneShotSubmission, OneShotError> {
        let result = self.client.submit(request).await;
        self.wake
            .send_modify(|version| *version = version.wrapping_add(1));

        result
    }
}

/// Construct before spawning so cancellation before the first poll also
/// leaves the retained launch available for explicit restart.
struct GraphDriverActivity(Arc<AtomicBool>);

impl Drop for GraphDriverActivity {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) struct EnvironmentLease {
    reader: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
    wake: tokio::sync::watch::Sender<u64>,
    writer: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

impl EnvironmentLease {
    /// Holds a background-reader lease, which overlaps ordinary session work
    /// but excludes graph writers, and wakes graph drivers on release.
    pub(super) fn background_reader(
        reader: tokio::sync::OwnedRwLockReadGuard<()>,
        wake: tokio::sync::watch::Sender<u64>,
    ) -> Self {
        Self {
            reader: Some(reader),
            wake,
            writer: None,
        }
    }

    /// Releases the writer before notifying graph drivers, including when a
    /// selected command returns early or is dropped during a gate wait.
    pub(super) fn from_writer(
        writer: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
        wake: tokio::sync::watch::Sender<u64>,
    ) -> Self {
        Self {
            reader: None,
            wake,
            writer,
        }
    }
}

impl Drop for EnvironmentLease {
    fn drop(&mut self) {
        drop(self.reader.take());
        drop(self.writer.take());
        self.wake
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

struct SessionGraphRunner {
    cancellation: CancellationToken,
    runtime: SessionRuntimeHandle,
    utility: GraphUtilityRunner,
}

impl SessionGraphRunner {
    fn validate_launch(
        &self,
        request: &StartExecutionGraphRequest,
    ) -> Result<(), ag_session::SessionError> {
        for node in &request.graph.nodes {
            if !self.supports(node) {
                return Err(ag_session::SessionError::Operation(format!(
                    "Unsupported graph node: {}",
                    node.id
                )));
            }
            if let Some(id) = &node.execution.shared_budget_id
                && !request.provider_call_limits.contains_key(id)
            {
                return Err(ag_session::SessionError::Operation(format!(
                    "Unknown graph provider budget: {id}"
                )));
            }
        }

        Ok(())
    }
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
