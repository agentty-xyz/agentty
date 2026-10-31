//! Focused review as a static execution graph.
//!
//! Original-diff fragments run as parallel background readers. A cross-file
//! pass joins every fragment's validated review, and consolidation joins both
//! phases. Each node is a recorded utility run coordinating its phase's child
//! calls through the worker; phase semantics remain in `review_prompt`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use ag_contracts::{
    OneShotError, OneShotRequest, OneShotSubmission, PermissionMode, ProviderCallBudget,
};
use ag_protocol::{FocusedReview, ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentSelection, EnvId, ExecutionGraph, ExecutionGraphStatus,
    GraphCompletionGate, GraphContext, GraphControlRequest, GraphDependency, GraphExecutionProfile,
    GraphExecutionRef, GraphInputBinding, GraphNode, GraphOutput, GraphPostProcessing,
    GraphWorkspaceAccess,
};
use ag_store::AppRepositories;
use ag_worker::{
    GraphDispatch, GraphExecutor, GraphNodeRunner, GraphResources, GraphRunRequest, RunClient,
    RunInfo, RunScope, RunWorker,
};
use async_trait::async_trait;
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep_until, timeout_at};
use tokio_util::sync::CancellationToken;

use super::execution_graph::{EnvironmentLease, GraphDriverRegistration, GraphDrivers};
use super::review::ReviewProgress;
use super::review_prompt::{
    self, BatchOutcome, BatchProgress, Fragment, MAX_REVIEW_PROVIDER_CALLS, REVIEW_CONCURRENCY,
    ReviewOutcome, ReviewedBatches,
};
use crate::infra::review_deadline::ReviewDeadlineClient;

/// Joins discovery and the cross-file pass into the final review.
const CONSOLIDATION_NODE: &str = "consolidation";

/// Joins every discovery fragment for an independent cross-file pass.
const CROSS_FILE_NODE: &str = "cross-file";

/// Each node publishes its complete focused review under this name.
const OUTPUT: &str = "review";

/// Renders review content, history, and the diff defining criteria scope.
pub(super) type ReviewRender =
    Arc<dyn Fn(&str, &str, &str) -> Result<String, OneShotError> + Send + Sync>;

/// Receives live phase progress for one review generation.
pub(super) type ReviewProgressSink = Arc<dyn Fn(ReviewProgress) + Send + Sync>;

/// Host boundaries shared with other graphs and ordinary session work.
pub(super) struct ReviewGraphHost {
    /// Lifecycle cancellation and session cleanup reach active reviews here.
    pub(super) drivers: GraphDrivers,
    /// Graph snapshots, durable admission, and utility-run records.
    pub(super) repositories: AppRepositories,
    /// Env gates and graph capacity shared with every other graph driver.
    pub(super) resources: Arc<GraphResources>,
    /// Settles each node's recorded utility run around its child calls.
    pub(super) runs: Arc<RunWorker>,
    /// Graph wakeups published by ordinary Env releases.
    pub(super) wake: tokio::sync::watch::Sender<u64>,
}

/// One review generation's captured inputs and graph identity.
pub(super) struct ReviewGraphRequest {
    /// Captured review model, retained by every node snapshot.
    pub(super) agent: AgentSelection,
    /// Session history supplied as untrusted review context.
    pub(super) context: String,
    /// Shared review deadline, also bounding Env and dispatch waits.
    pub(super) deadline: Instant,
    /// Captured diff defining the review scope.
    pub(super) diff: String,
    /// Unique per generation; retries create a new graph.
    pub(super) graph_id: String,
    /// Provider, model, reasoning, and folder shared by every phase.
    pub(super) request: OneShotRequest,
    /// Session owning the graph and its Env.
    pub(super) session_id: String,
}

impl ReviewGraphHost {
    /// Reviews original diff text in bounded fragments. History and shared
    /// cross-file context may be summarized. All fragments, history repairs,
    /// the cross-file pass, and final reduction share one provider budget.
    /// Fragment nodes run in waves of at most three that merge in input order;
    /// a failure drains its running peers and stops new work, reporting
    /// incomplete coverage. At most three provider calls run at once across
    /// every node. Every node holds a background-reader lease, so reviews
    /// overlap ordinary session work but never graph writers. Child calls go
    /// through `client` without taking further Env leases. The deadline also
    /// bounds Env and dispatch waits, and lifecycle cancellation or session
    /// cleanup cancels running nodes and their calls.
    ///
    /// # Errors
    /// Returns render, persistence, and preparation failures, and discovery
    /// failures before any fragment review completed.
    pub(super) async fn submit(
        &self,
        client: Arc<dyn RunClient>,
        launch: ReviewGraphRequest,
        render: ReviewRender,
        progress: ReviewProgressSink,
    ) -> Result<String, OneShotError> {
        let client: Arc<dyn RunClient> = Arc::new(ReviewCallLimit {
            client,
            permits: Semaphore::new(REVIEW_CONCURRENCY),
        });
        let budget = ProviderCallBudget::new(MAX_REVIEW_PROVIDER_CALLS);
        let request = review_prompt::review_request(launch.request, &budget);
        let env_id = EnvId::SessionWorktree(launch.session_id.as_str().into());
        let context = {
            let shared_gate = timeout_at(
                launch.deadline,
                self.resources.background_readers(&env_id).read_owned(),
            )
            .await
            .map_err(|_| ReviewDeadlineClient::expired())?;
            let _lease = EnvironmentLease::background_reader(shared_gate, self.wake.clone());

            review_prompt::prepare_context(
                client.as_ref(),
                &request,
                &launch.diff,
                &launch.context,
                render.as_ref(),
                &budget,
                progress.as_ref(),
            )
            .await?
        };
        let leaves = review_prompt::partition(&launch.diff, &context, render.as_ref());
        let graph = review_graph(&launch.graph_id, &env_id, launch.agent, leaves.len());
        self.repositories
            .graphs()
            .save_graph(&graph, None)
            .await
            .map_err(|error| OneShotError::new(error.to_string()))?;
        let state = Arc::new(ReviewState::new(context, leaves.len(), progress.clone()));
        state.report();
        let runner = ReviewNodeRunner {
            budget,
            client,
            diff: launch.diff.as_str().into(),
            leaves: graph
                .nodes
                .iter()
                .map(|node| node.id.clone())
                .zip(leaves.iter().cloned())
                .collect(),
            progress,
            render,
            request: request.clone(),
            runs: self.runs.clone(),
            state: state.clone(),
        };
        let executor = Arc::new(
            GraphExecutor::new(
                self.repositories.graph_dispatch(),
                self.resources.clone(),
                Arc::new(runner),
            )
            .with_wave_limit(NonZeroUsize::new(REVIEW_CONCURRENCY).unwrap_or(NonZeroUsize::MIN)),
        );
        let registration =
            GraphDriverRegistration::register(&self.drivers, &graph, executor.clone());
        let driver_error = self
            .drive(
                &graph,
                &executor,
                &request.folder,
                &registration.cancellation,
                launch.deadline,
            )
            .await;
        drop(registration);
        let outcome = state.outcome(&leaves, driver_error);
        if !matches!(outcome.consolidation, Some(Ok(_))) {
            // Labels the abandoned generation terminal; the review text never
            // depends on it, and retries always start a new graph.
            let _ = self
                .repositories
                .graphs()
                .cancel(&GraphControlRequest {
                    graph_id: graph.id.clone(),
                    revision: graph.revision,
                })
                .await;
        }

        outcome.render(&launch.diff)
    }

    /// Dispatches until the persisted graph completes, a wave fails, the
    /// graph is canceled, or the deadline passes. Resource contention, such as
    /// an active graph writer, waits for releases. Cancellation reaches
    /// running nodes through their worker scope; each wave still settles its
    /// admitted work before dispatch stops.
    async fn drive(
        &self,
        graph: &ExecutionGraph,
        executor: &GraphExecutor,
        folder: &Path,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Option<OneShotError> {
        let mut wake = self.wake.subscribe();
        let mut releases = self.resources.subscribe_releases();
        let scope = RunScope {
            cancellation: Some(cancellation.clone()),
            ..RunScope::default()
        };
        loop {
            wake.borrow_and_update();
            releases.borrow_and_update();
            match ag_worker::in_scope(
                scope.clone(),
                executor.run_ready(&graph.id, graph.revision, folder, PermissionMode::ReadOnly),
            )
            .await
            {
                Err(error) => return Some(error),
                Ok(receipts) if !receipts.is_empty() => continue,
                Ok(_) => {}
            }
            // Only the persisted projection proves completion; an empty
            // dispatch can also mean contention or a closed graph.
            let status = self
                .repositories
                .graphs()
                .progress(&graph.id)
                .await
                .ok()
                .flatten()
                .map(|progress| progress.status);
            match status {
                Some(ExecutionGraphStatus::Completed) => return None,
                Some(ExecutionGraphStatus::Cancelled) | None => return Some(closed()),
                Some(_) => {}
            }
            // Both senders outlive this loop, so neither wait ends spuriously.
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Some(closed()),
                () = sleep_until(deadline) => return Some(ReviewDeadlineClient::expired()),
                _ = wake.changed() => {},
                _ = releases.changed() => {},
                () = executor.capacity_released() => {},
                () = executor.environment_released() => {},
            }
        }
    }
}

/// Reported when lifecycle control or cleanup closes a review graph.
fn closed() -> OneShotError {
    OneShotError::new("Focused review graph was canceled or is unavailable")
}

/// Bounds one review's simultaneous provider calls across its concurrent
/// nodes, including calls made while subdividing rejected fragments.
struct ReviewCallLimit {
    client: Arc<dyn RunClient>,
    permits: Semaphore,
}

#[async_trait]
impl RunClient for ReviewCallLimit {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        // The semaphore is never closed, so a permit is always granted.
        let _permit = self.permits.acquire().await.ok();

        self.client.submit(request).await
    }
}

/// Builds fragment readers, a cross-file join, and a consolidation join.
/// Fragment text stays with the host so every admission reloads a compact
/// snapshot; nodes carry no executor budget because review phases enforce
/// the shared review budget and report its exhaustion themselves.
fn review_graph(
    graph_id: &str,
    env_id: &EnvId,
    agent: AgentSelection,
    fragments: usize,
) -> ExecutionGraph {
    let EnvId::SessionWorktree(session_id) = env_id;
    let node = |id: String, objective: &str, request: String, producers: &[String]| GraphNode {
        agent,
        definition: AgentDefinition {
            description: "Read-only review of captured changes".into(),
            id: "focused-review".into(),
            name: "Focused review".into(),
            prompt: "Review the captured changes under the focused-review criteria.".into(),
            read_only: true,
        },
        dependencies: producers
            .iter()
            .map(|producer| GraphDependency {
                gate: GraphCompletionGate::ResultValidated {
                    output_name: OUTPUT.into(),
                },
                node_id: producer.clone(),
            })
            .collect(),
        env_id: env_id.clone(),
        execution: GraphExecutionProfile {
            context: GraphContext::Fresh,
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::FocusedReview,
            shared_budget_id: None,
            workspace_access: GraphWorkspaceAccess::BackgroundRead,
        },
        id,
        inputs: producers
            .iter()
            .map(|producer| GraphInputBinding {
                name: producer.clone(),
                output_name: OUTPUT.into(),
                producer_node_id: producer.clone(),
            })
            .collect(),
        objective: objective.into(),
        outputs: vec![GraphOutput {
            name: OUTPUT.into(),
            schema: ProtocolRequestProfile::FocusedReview,
        }],
        request: TurnPrompt::from_agent_data(request),
    };
    let mut nodes: Vec<_> = (0..fragments)
        .map(|index| {
            node(
                format!("batch-{index}"),
                "Review one original diff fragment",
                format!(
                    "Review captured diff fragment {} of {fragments}.",
                    index + 1
                ),
                &[],
            )
        })
        .collect();
    let batches: Vec<_> = nodes.iter().map(|node| node.id.clone()).collect();
    nodes.push(node(
        CROSS_FILE_NODE.into(),
        "Inspect interactions across the captured changes",
        "Review the complete captured diff with every fragment's findings.".into(),
        &batches,
    ));
    let mut all = batches;
    all.push(CROSS_FILE_NODE.into());
    nodes.push(node(
        CONSOLIDATION_NODE.into(),
        "Reconcile every candidate into the final review",
        "Consolidate discovery and cross-file findings.".into(),
        &all,
    ));

    ExecutionGraph {
        id: graph_id.into(),
        nodes,
        revision: NonZeroU64::MIN,
        session_id: session_id.clone(),
    }
}

/// Phase results shared by node runs and the final assembly.
struct ReviewState {
    progress: ReviewProgressSink,
    results: Mutex<ReviewResults>,
}

#[derive(Default)]
struct ReviewResults {
    completed: BTreeMap<Vec<usize>, FocusedReview>,
    consolidation: Option<Result<FocusedReview, OneShotError>>,
    context: String,
    cross_file: Option<Result<ReviewedBatches, OneShotError>>,
    failure: Option<OneShotError>,
    finished: usize,
    pending: BTreeMap<Vec<usize>, String>,
    /// Discovery nodes whose completed and unfinished fragments are recorded.
    recorded: BTreeSet<String>,
    total: usize,
}

impl ReviewState {
    fn new(context: String, total: usize, progress: ReviewProgressSink) -> Self {
        Self {
            progress,
            results: Mutex::new(ReviewResults {
                context,
                total,
                ..ReviewResults::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReviewResults> {
        self.results.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publishes finished and known fragments, outside the state lock.
    fn report(&self) {
        let (completed, total) = {
            let results = self.lock();

            (results.finished, results.total)
        };
        (self.progress)(ReviewProgress::Batches { completed, total });
    }

    fn batch_progress(&self, event: BatchProgress) {
        {
            let mut results = self.lock();
            match event {
                BatchProgress::Completed => results.finished += 1,
                BatchProgress::Added(count) => results.total += count,
            }
        }
        self.report();
    }

    /// Converts settled phases into review text inputs. Fragments whose node
    /// never ran or was canceled remain unfinished, reported with the reason
    /// dispatch stopped.
    fn outcome(&self, leaves: &[Fragment], driver_error: Option<OneShotError>) -> ReviewOutcome {
        let mut results = self.lock();
        let stopped = driver_error
            .unwrap_or_else(|| OneShotError::new("Focused review stopped before it finished"));
        for (index, fragment) in leaves.iter().enumerate() {
            if !results.recorded.contains(&format!("batch-{index}")) {
                results
                    .pending
                    .insert(fragment.0.clone(), fragment.1.clone());
            }
        }
        let failure = results
            .failure
            .take()
            .or_else(|| (!results.pending.is_empty()).then(|| stopped.clone()));

        ReviewOutcome {
            batches: BatchOutcome {
                completed: std::mem::take(&mut results.completed),
                failure,
                pending: std::mem::take(&mut results.pending)
                    .into_iter()
                    .collect::<VecDeque<_>>(),
            },
            consolidation: results.consolidation.take(),
            context: std::mem::take(&mut results.context),
            cross_file: results.cross_file.take(),
            stopped,
        }
    }
}

/// Runs review phases for this generation's graph nodes only.
struct ReviewNodeRunner {
    budget: ProviderCallBudget,
    client: Arc<dyn RunClient>,
    diff: Arc<str>,
    leaves: HashMap<String, Fragment>,
    progress: ReviewProgressSink,
    render: ReviewRender,
    request: OneShotRequest,
    runs: Arc<RunWorker>,
    state: Arc<ReviewState>,
}

#[async_trait]
impl GraphNodeRunner for ReviewNodeRunner {
    fn supports(&self, node: &GraphNode) -> bool {
        node.execution.context == GraphContext::Fresh
            && node.execution.workspace_access == GraphWorkspaceAccess::BackgroundRead
            && node.execution.response_profile == ProtocolRequestProfile::FocusedReview
    }

    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError> {
        let GraphExecutionRef::UtilityRun { run_id } = &request.dispatch.record.execution else {
            return Err(OneShotError::new("Review node requires a utility run"));
        };
        let EnvId::SessionWorktree(session_id) = &request.dispatch.node.env_id;
        let run = RunInfo {
            folder: request.folder.clone(),
            id: run_id.clone(),
            parent_id: Some(request.dispatch.record.graph_id.clone()),
            project_id: None,
            purpose: format!("graph:{}", request.dispatch.node.id),
            session_id: Some(session_id.to_string()),
        };
        let mut phase_request = self.request.clone();
        phase_request.folder = request.folder;
        phase_request.permission_mode = request.permission_mode;
        let phase = ReviewPhase {
            budget: self.budget.clone(),
            client: self.client.clone(),
            diff: self.diff.clone(),
            fragment: self.leaves.get(&request.dispatch.node.id).cloned(),
            dispatch: request.dispatch,
            progress: self.progress.clone(),
            render: self.render.clone(),
            request: phase_request,
            state: self.state.clone(),
        };

        self.runs.coordinate_recorded(run, phase.run()).await
    }
}

/// Owned inputs for one node's phase, run under its recorded utility.
struct ReviewPhase {
    budget: ProviderCallBudget,
    client: Arc<dyn RunClient>,
    diff: Arc<str>,
    dispatch: GraphDispatch,
    fragment: Option<Fragment>,
    progress: ReviewProgressSink,
    render: ReviewRender,
    request: OneShotRequest,
    state: Arc<ReviewState>,
}

impl ReviewPhase {
    async fn run(self) -> Result<String, OneShotError> {
        let review = match (&self.fragment, self.dispatch.node.id.as_str()) {
            (Some(fragment), _) => self.discover(fragment.clone()).await?,
            (None, CROSS_FILE_NODE) => self.cross_file().await?,
            (None, _) => self.consolidate().await?,
        };

        Ok(serde_json::json!(review).to_string())
    }

    /// Reviews one original fragment, subdividing it on provider size limits.
    async fn discover(&self, fragment: Fragment) -> Result<FocusedReview, OneShotError> {
        let mut context = self.state.lock().context.clone();
        let outcome = review_prompt::review_batches(
            self.client.as_ref(),
            &self.request,
            VecDeque::from([fragment]),
            &mut context,
            self.render.as_ref(),
            &self.budget,
            &|event| self.state.batch_progress(event),
        )
        .await;
        let mut results = self.state.lock();
        // A canceled node never reaches this point; its whole fragment then
        // remains unfinished.
        results.recorded.insert(self.dispatch.node.id.clone());
        if context.len() < results.context.len() {
            results.context = context;
        }
        let review = review_prompt::merged(outcome.completed.values().cloned());
        results.completed.extend(outcome.completed);
        if let Some(error) = outcome.failure {
            results.pending.extend(outcome.pending);
            results.failure.get_or_insert_with(|| error.clone());

            return Err(error);
        }

        Ok(review)
    }

    /// Checks interactions with shared context only when discovery spans
    /// several fragments. A partial pass fails the node, blocking
    /// consolidation while its retained findings remain available.
    async fn cross_file(&self) -> Result<FocusedReview, OneShotError> {
        let discovery = self.inputs()?;
        let (completed, mut context) = {
            let results = self.state.lock();

            (results.completed.len(), results.context.clone())
        };
        (self.progress)(ReviewProgress::CrossFile);
        let result = review_prompt::cross_file_review(
            self.client.as_ref(),
            &self.request,
            &self.diff,
            &mut context,
            (completed > 1).then_some(&discovery),
            self.render.as_ref(),
            &self.budget,
        )
        .await;
        let mut results = self.state.lock();
        results.context = context;
        results.cross_file = Some(result.clone());
        let result = result?;
        if !result.coverage.is_empty() {
            return Err(OneShotError::new(result.coverage));
        }

        Ok(result.review)
    }

    /// Replaces every discovery and cross-file candidate with the reconciled
    /// review.
    async fn consolidate(&self) -> Result<FocusedReview, OneShotError> {
        let review = self.inputs()?;
        let mut context = self.state.lock().context.clone();
        (self.progress)(ReviewProgress::Reducing);
        let result = review_prompt::reduce_review(
            self.client.as_ref(),
            &self.request,
            &review,
            &self.diff,
            &mut context,
            self.render.as_ref(),
            &self.budget,
        )
        .await
        .map(|result| review_prompt::merged([result]));
        let mut results = self.state.lock();
        results.context = context;
        results.consolidation = Some(result.clone());

        result
    }

    /// Merges the exact pinned producer outputs in declaration order.
    fn inputs(&self) -> Result<FocusedReview, OneShotError> {
        let reviews = self
            .dispatch
            .inputs
            .iter()
            .map(|input| {
                serde_json::from_str::<FocusedReview>(&input.body)
                    .map_err(|error| OneShotError::new(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(review_prompt::merged(reviews))
    }
}

#[cfg(test)]
#[path = "review_graph_test.rs"]
mod tests;

#[cfg(test)]
#[path = "review_graph_support_test.rs"]
pub(super) mod test_support;
