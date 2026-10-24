use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::num::{NonZeroU64, NonZeroUsize};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};
use ag_session::{EnvId, ExecutionGraph, GraphNode, GraphWorkspaceAccess};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore, oneshot};
use tokio::task::JoinSet;

use super::{
    GraphAdmission, GraphDispatchRepository, GraphExecutionRecord, GraphNodeRunner, GraphRunRequest,
};
use crate::scope::{RunContext, in_context};

/// Shared across graph coordinators and other host work using the same Envs.
pub struct GraphResources {
    budgets: HashMap<String, ProviderCallBudget>,
    capacity: Arc<Semaphore>,
    environments: Mutex<HashMap<EnvId, Arc<RwLock<()>>>>,
}

impl GraphResources {
    /// Captures host-owned budgets and an additional graph concurrency ceiling.
    /// Existing worker capacity and execution policy remain independent
    /// ceilings.
    pub fn new(capacity: NonZeroUsize, budgets: HashMap<String, ProviderCallBudget>) -> Self {
        Self {
            budgets,
            capacity: Arc::new(Semaphore::new(capacity.get())),
            environments: Mutex::new(HashMap::new()),
        }
    }

    /// Host work sharing an Env uses this same read/write gate. Hold writers
    /// through ordered effects and resource cleanup, not just provider return.
    pub fn environment(&self, id: &EnvId) -> Arc<RwLock<()>> {
        self.environments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.clone())
            .or_default()
            .clone()
    }
}

/// Runs arbitrary static graphs in stable declaration-order waves.
///
/// No automatic retry, acceptance, task completion, or restart replay occurs.
/// A call drains currently reachable nodes; blocked gates remain persisted.
pub struct GraphExecutor {
    repository: Arc<dyn GraphDispatchRepository>,
    resources: Arc<GraphResources>,
    runner: Arc<dyn GraphNodeRunner>,
}

impl GraphExecutor {
    /// Composes durable admission, shared resources, and a trusted worker
    /// adapter.
    pub fn new(
        repository: Arc<dyn GraphDispatchRepository>,
        resources: Arc<GraphResources>,
        runner: Arc<dyn GraphNodeRunner>,
    ) -> Self {
        Self {
            repository,
            resources,
            runner,
        }
    }

    /// Dispatches ready branches concurrently, then reevaluates joins. Returns
    /// only newly admitted receipts, never an inferred graph-complete status.
    /// Nodes with exhausted shared budgets remain unclaimed for a later call.
    ///
    /// # Errors
    /// Rejects stale graphs, unsupported profiles, unknown budgets,
    /// storage failures, and worker failures. Already admitted peers settle
    /// before a wave's failure is returned. Hosts invoke again after gate or
    /// resource changes; previously admitted nodes are never replayed.
    pub async fn run_ready(
        &self,
        graph_id: &str,
        revision: NonZeroU64,
        folder: &Path,
        session_permissions: PermissionMode,
    ) -> Result<Vec<GraphExecutionRecord>, OneShotError> {
        let graph = self.repository.graph(graph_id, revision).await?;
        self.check_supported(&graph)?;

        let mut receipts = Vec::new();
        loop {
            let mut work = GraphWave(JoinSet::new());
            let initial_receipt_count = receipts.len();
            for node in &graph.nodes {
                let Ok(capacity) = self.resources.capacity.clone().try_acquire_owned() else {
                    break;
                };
                let environment = self.resources.environment(&node.env_id);
                let lease = match node.execution.workspace_access {
                    GraphWorkspaceAccess::SharedRead => {
                        let Ok(lease) = environment.try_read_owned() else {
                            continue;
                        };
                        EnvironmentLease::Read { _lease: lease }
                    }
                    GraphWorkspaceAccess::ExclusiveWrite => {
                        let Ok(lease) = environment.try_write_owned() else {
                            continue;
                        };
                        EnvironmentLease::Write { _lease: lease }
                    }
                };
                let (admitted, admission) = oneshot::channel();
                let node_work = self.admit_and_run(
                    &graph,
                    node,
                    folder,
                    session_permissions,
                    admitted,
                    GraphNodeLease {
                        _capacity: capacity,
                        _environment: lease,
                    },
                );
                let context = RunContext::current();
                work.0.spawn(in_context(context, node_work));
                // Acknowledge claims in declaration order. The owned task
                // keeps admission and settlement alive if this receiver drops.
                match admission.await {
                    Ok(Some(record)) => receipts.push(record),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            let mut failure = None;
            while let Some(result) = work.0.join_next().await {
                let result = result
                    .map_err(|error| OneShotError::new(error.to_string()))
                    .and_then(|result| result);
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
            }
            if let Some(error) = failure {
                return Err(error);
            }
            if receipts.len() == initial_receipt_count {
                return Ok(receipts);
            }
        }
    }

    /// Captures owned admission inputs before spawning. The same task owns
    /// commit, execution and settlement, independently of driver cancellation.
    fn admit_and_run(
        &self,
        graph: &ExecutionGraph,
        node: &GraphNode,
        folder: &Path,
        session_permissions: PermissionMode,
        admitted: oneshot::Sender<Option<GraphExecutionRecord>>,
        lease: GraphNodeLease,
    ) -> impl Future<Output = Result<(), OneShotError>> + Send + 'static {
        let graph_id = graph.id.clone();
        let revision = graph.revision;
        let node_id = node.id.clone();
        let folder = folder.to_path_buf();
        let permission_mode = node
            .execution
            .permission_mode(&node.definition, session_permissions);
        let provider_call_budget = node
            .execution
            .shared_budget_id
            .as_ref()
            .and_then(|id| self.resources.budgets.get(id))
            .cloned();
        let repository = self.repository.clone();
        let runner = self.runner.clone();

        async move {
            let resource_lease = lease;
            let provider_call_budget = match provider_call_budget {
                Some(budget) => {
                    let Ok(reserved) = budget.reserve() else {
                        drop(resource_lease);
                        let _ = admitted.send(None);

                        return Ok(());
                    };

                    Some(reserved)
                }
                None => None,
            };
            let dispatch = match repository
                .claim(
                    &graph_id,
                    revision,
                    &node_id,
                    &uuid::Uuid::new_v4().to_string(),
                    &folder,
                )
                .await?
            {
                GraphAdmission::Claimed(dispatch) => *dispatch,
                GraphAdmission::Blocked | GraphAdmission::Existing(_) => {
                    drop(provider_call_budget);
                    drop(resource_lease);
                    let _ = admitted.send(None);

                    return Ok(());
                }
            };
            let record = dispatch.record.clone();
            let _ = admitted.send(Some(record.clone()));
            let request = GraphRunRequest {
                dispatch: dispatch.clone(),
                folder,
                permission_mode,
                provider_call_budget,
            };
            let result = {
                // Include future construction and every poll in the unwind
                // boundary. Drop a panicked runner before settling its claim.
                let work = async { runner.run(request).await };
                tokio::pin!(work);

                poll_fn(|context| {
                    catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(context))).unwrap_or_else(
                        |_| Poll::Ready(Err(OneShotError::new("Graph node runner panicked"))),
                    )
                })
                .await
            };
            match result {
                Ok(body) => repository.finish(&dispatch, &body).await,
                Err(error) => {
                    repository.fail(&record, &error.to_string()).await?;

                    Err(error)
                }
            }
        }
    }

    fn check_supported(&self, graph: &ExecutionGraph) -> Result<(), OneShotError> {
        for node in &graph.nodes {
            if !self.runner.supports(node) {
                return Err(OneShotError::new(format!(
                    "Unsupported graph execution profile: {}",
                    node.id
                )));
            }
            if let Some(id) = &node.execution.shared_budget_id {
                self.resources.budgets.get(id).ok_or_else(|| {
                    OneShotError::new(format!("Unknown graph provider budget: {id}"))
                })?;
            }
        }

        Ok(())
    }
}

/// Dropping a driver leaves admitted work owned until settlement. Inherited
/// cancellation still reaches workers; leases outlive their runtime cleanup.
struct GraphWave(JoinSet<Result<(), OneShotError>>);

impl Drop for GraphWave {
    fn drop(&mut self) {
        self.0.detach_all();
    }
}

/// One task retains both guards from before admission through settlement.
struct GraphNodeLease {
    _capacity: OwnedSemaphorePermit,
    _environment: EnvironmentLease,
}

/// Both lease kinds live through worker settlement and result persistence.
enum EnvironmentLease {
    Read {
        _lease: tokio::sync::OwnedRwLockReadGuard<()>,
    },
    Write {
        _lease: tokio::sync::OwnedRwLockWriteGuard<()>,
    },
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;
