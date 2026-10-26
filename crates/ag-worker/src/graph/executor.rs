use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::num::{NonZeroU64, NonZeroUsize};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;

use ag_contracts::{OneShotError, PermissionMode, ProviderCallBudget};
use ag_session::{EnvId, ExecutionGraph, GraphNode, GraphWorkspaceAccess};
use tokio::sync::{Notify, OwnedSemaphorePermit, RwLock, Semaphore, oneshot, watch};
use tokio::task::JoinSet;

use super::{
    GraphAdmission, GraphDispatchRepository, GraphExecutionRecord, GraphNodeRunner, GraphRunRequest,
};
use crate::scope::{RunContext, in_context};

/// Shared across graph coordinators and other host work using the same Envs.
pub struct GraphResources {
    background_readers: Arc<Mutex<HashMap<EnvId, Arc<RwLock<()>>>>>,
    budgets: HashMap<String, ProviderCallBudget>,
    capacity: Arc<Semaphore>,
    capacity_waiters: Arc<Mutex<Vec<Weak<Notify>>>>,
    environment_waiters: EnvironmentWaiters,
    environments: Arc<Mutex<HashMap<EnvId, Arc<RwLock<()>>>>>,
    releases: watch::Sender<u64>,
}

impl GraphResources {
    /// Captures host-owned budgets and an additional graph concurrency ceiling.
    /// Existing worker capacity and execution policy remain independent
    /// ceilings.
    pub fn new(capacity: NonZeroUsize, budgets: HashMap<String, ProviderCallBudget>) -> Self {
        Self {
            background_readers: Arc::default(),
            budgets,
            capacity: Arc::new(Semaphore::new(capacity.get())),
            capacity_waiters: Arc::default(),
            environment_waiters: Arc::default(),
            environments: Arc::default(),
            releases: watch::channel(0).0,
        }
    }

    /// Captures graph-local budgets while sharing the application's Env gates
    /// and graph capacity. Budget identities are local to this graph driver.
    #[must_use]
    pub fn with_budgets(&self, budgets: HashMap<String, ProviderCallBudget>) -> Self {
        Self {
            background_readers: Arc::clone(&self.background_readers),
            budgets,
            capacity: Arc::clone(&self.capacity),
            capacity_waiters: Arc::clone(&self.capacity_waiters),
            environment_waiters: Arc::clone(&self.environment_waiters),
            environments: Arc::clone(&self.environments),
            releases: self.releases.clone(),
        }
    }

    /// Observes each admitted node's durable settlement and subsequent release
    /// of capacity and Env leases, including failures. Shared across drivers
    /// created with `with_budgets`; unclaimed scans do not signal releases.
    pub fn subscribe_releases(&self) -> watch::Receiver<u64> {
        self.releases.subscribe()
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

    /// Legacy background readers may overlap ordinary host mutations. They
    /// hold this gate for reading instead of `environment`. Graph writers,
    /// including host-managed nodes, must acquire this gate for writing before
    /// acquiring the ordinary Env writer and retain both through settlement.
    pub fn background_readers(&self, id: &EnvId) -> Arc<RwLock<()>> {
        self.background_readers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.clone())
            .or_default()
            .clone()
    }

    /// Register contention under the same lock as permit return, preventing a
    /// release between a failed acquire and registration from losing a wakeup.
    fn try_capacity(&self, wake: &Arc<Notify>) -> Option<GraphCapacityLease> {
        let mut waiters = self
            .capacity_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(permit) = self.capacity.clone().try_acquire_owned() {
            return Some(GraphCapacityLease {
                permit: Some(permit),
                waiters: self.capacity_waiters.clone(),
            });
        }
        waiters.retain(|waiter| waiter.strong_count() > 0);
        let waiter = Arc::downgrade(wake);
        if !waiters.iter().any(|existing| existing.ptr_eq(&waiter)) {
            waiters.push(waiter);
        }

        None
    }
}

/// Runs arbitrary static graphs in stable declaration-order waves.
///
/// No automatic retry, acceptance, task completion, or restart replay occurs.
/// A call drains currently reachable nodes; blocked gates remain persisted.
pub struct GraphExecutor {
    capacity_wake: Arc<Notify>,
    environment_wake: Arc<Notify>,
    repository: Arc<dyn GraphDispatchRepository>,
    resources: Arc<GraphResources>,
    runner: Arc<dyn GraphNodeRunner>,
}

impl GraphExecutor {
    /// Reads a named live budget without replenishing it or reserving calls.
    pub fn remaining_provider_calls(&self, id: &str) -> Option<usize> {
        self.resources
            .budgets
            .get(id)
            .map(ProviderCallBudget::remaining)
    }

    /// Composes durable admission, shared resources, and a trusted worker
    /// adapter.
    pub fn new(
        repository: Arc<dyn GraphDispatchRepository>,
        resources: Arc<GraphResources>,
        runner: Arc<dyn GraphNodeRunner>,
    ) -> Self {
        Self {
            capacity_wake: Arc::default(),
            environment_wake: Arc::default(),
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
    /// Hosts wait on `GraphResources::subscribe_releases`,
    /// `Self::capacity_released` and `Self::environment_released` to observe
    /// settlement and unclaimed returns.
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
            let mut reusable_capacity = None;
            for node in &graph.nodes {
                let Some(capacity) = reusable_capacity
                    .take()
                    .or_else(|| self.resources.try_capacity(&self.capacity_wake))
                else {
                    break;
                };
                let Ok(lease) = self.try_environment(node) else {
                    reusable_capacity = Some(capacity);
                    continue;
                };
                let (admitted, admission) = oneshot::channel();
                let node_work = self.admit_and_run(
                    &graph,
                    node,
                    folder,
                    session_permissions,
                    admitted,
                    GraphNodeLease {
                        capacity: Some(capacity),
                        claimed: false,
                        environment: lease,
                        releases: self.resources.releases.clone(),
                    },
                );
                let context = RunContext::current();
                work.0.spawn(in_context(context, node_work));
                // Acknowledge claims in declaration order. The owned task
                // keeps admission and settlement alive if this receiver drops.
                match admission.await {
                    Ok(GraphNodeAdmission::Claimed(record)) => receipts.push(record),
                    Ok(GraphNodeAdmission::Unclaimed(mut lease)) => {
                        reusable_capacity = lease.capacity.take();
                    }
                    Err(_) => break,
                }
            }
            drop(reusable_capacity);
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

    /// Waits for a permit return after this executor encountered full capacity.
    /// The wakeup survives a release racing the host's wait. Only contending
    /// executors are notified, including releases from unclaimed checks;
    /// idle scans do not broadcast wakeups to one another.
    pub async fn capacity_released(&self) {
        self.capacity_wake.notified().await;
    }

    /// Waits for an Env release after this executor encountered a conflicting
    /// lease. Registration and release are synchronized, and the wakeup is
    /// retained until observed. Unclaimed checks wake only Env contenders.
    pub async fn environment_released(&self) {
        self.environment_wake.notified().await;
    }

    fn try_environment(
        &self,
        node: &GraphNode,
    ) -> Result<Option<EnvironmentLease>, tokio::sync::TryLockError> {
        if matches!(
            node.execution.context,
            ag_session::GraphContext::TaskConversation { .. }
        ) && self.runner.manages_environment(node)
        {
            return Ok(None);
        }
        let mut waiters = self
            .resources
            .environment_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let guard = match EnvironmentGuard::try_acquire(&self.resources, node) {
            Ok(guard) => guard,
            Err(contention) => {
                let waiting = waiters.entry(node.env_id.clone()).or_default();
                waiting.retain(|waiter| waiter.wake.strong_count() > 0);
                let waiter = Arc::downgrade(&self.environment_wake);
                if !waiting.iter().any(|existing| {
                    existing.wake.ptr_eq(&waiter)
                        && Arc::ptr_eq(&existing.gate, &contention.gate)
                        && existing.writer == contention.writer
                }) {
                    waiting.push(EnvironmentWaiter {
                        gate: contention.gate,
                        wake: waiter,
                        writer: contention.writer,
                    });
                }

                return Err(contention.error);
            }
        };
        let lease = EnvironmentLease {
            guard: Some(guard),
            id: node.env_id.clone(),
            waiters: self.resources.environment_waiters.clone(),
        };

        Ok(Some(lease))
    }

    /// Captures owned admission inputs before spawning. The same task owns
    /// commit, execution and settlement, independently of driver cancellation.
    fn admit_and_run(
        &self,
        graph: &ExecutionGraph,
        node: &GraphNode,
        folder: &Path,
        session_permissions: PermissionMode,
        admitted: oneshot::Sender<GraphNodeAdmission>,
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
            let mut resource_lease = lease;
            let provider_call_budget = match provider_call_budget {
                Some(budget) => {
                    let Ok(reserved) = budget.reserve() else {
                        let _ = admitted.send(GraphNodeAdmission::Unclaimed(resource_lease));

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
                    let _ = admitted.send(GraphNodeAdmission::Unclaimed(resource_lease));

                    return Ok(());
                }
            };
            resource_lease.claimed = true;
            let record = dispatch.record.clone();
            let _ = admitted.send(GraphNodeAdmission::Claimed(record.clone()));
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

/// Unclaimed checks lend their permit to the rest of this scan. Returning it
/// between idle prefixes could make contending drivers restart one another.
enum GraphNodeAdmission {
    Claimed(GraphExecutionRecord),
    Unclaimed(GraphNodeLease),
}

/// One task retains both guards from before admission through settlement.
struct GraphNodeLease {
    capacity: Option<GraphCapacityLease>,
    claimed: bool,
    environment: Option<EnvironmentLease>,
    releases: watch::Sender<u64>,
}

impl Drop for GraphNodeLease {
    fn drop(&mut self) {
        drop(self.environment.take());
        drop(self.capacity.take());
        if self.claimed {
            self.releases
                .send_modify(|version| *version = version.wrapping_add(1));
        }
    }
}

/// Every permit return wakes only coordinators whose scans lacked capacity.
/// Weak registrations do not retain canceled drivers or grow without bound.
struct GraphCapacityLease {
    permit: Option<OwnedSemaphorePermit>,
    waiters: Arc<Mutex<Vec<Weak<Notify>>>>,
}

impl Drop for GraphCapacityLease {
    fn drop(&mut self) {
        let mut waiters = self
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(self.permit.take());
        for waiter in waiters.drain(..).filter_map(|waiter| waiter.upgrade()) {
            waiter.notify_one();
        }
    }
}

type EnvironmentWaiters = Arc<Mutex<HashMap<EnvId, Vec<EnvironmentWaiter>>>>;

/// Remember the exact conflicting gate and access, so releasing one reader
/// cannot restart writer scans while other readers still exclude them.
struct EnvironmentWaiter {
    gate: Arc<RwLock<()>>,
    wake: Weak<Notify>,
    writer: bool,
}

impl EnvironmentWaiter {
    fn retain_until_available(&self) -> bool {
        let Some(wake) = self.wake.upgrade() else {
            return false;
        };
        let available = if self.writer {
            self.gate.try_write().is_ok()
        } else {
            self.gate.try_read().is_ok()
        };
        if available {
            wake.notify_one();
        }

        !available
    }
}

/// Registration and guard release share one lock so no release can race past
/// a coordinator that has just observed contention.
struct EnvironmentLease {
    guard: Option<EnvironmentGuard>,
    id: EnvId,
    waiters: EnvironmentWaiters,
}

impl Drop for EnvironmentLease {
    fn drop(&mut self) {
        let mut waiters = self
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(self.guard.take());
        if let Some(waiting) = waiters.get_mut(&self.id) {
            waiting.retain(EnvironmentWaiter::retain_until_available);
            if waiting.is_empty() {
                waiters.remove(&self.id);
            }
        }
    }
}

/// Both lease kinds live through worker settlement and result persistence.
enum EnvironmentGuard {
    Read {
        _lease: tokio::sync::OwnedRwLockReadGuard<()>,
    },
    Write {
        _background: tokio::sync::OwnedRwLockWriteGuard<()>,
        _lease: tokio::sync::OwnedRwLockWriteGuard<()>,
    },
}

impl EnvironmentGuard {
    fn try_acquire(
        resources: &GraphResources,
        node: &GraphNode,
    ) -> Result<Self, EnvironmentContention> {
        let environment = resources.environment(&node.env_id);

        match node.execution.workspace_access {
            GraphWorkspaceAccess::SharedRead => Ok(Self::Read {
                _lease: environment.clone().try_read_owned().map_err(|error| {
                    EnvironmentContention {
                        error,
                        gate: environment,
                        writer: false,
                    }
                })?,
            }),
            GraphWorkspaceAccess::ExclusiveWrite => Ok(Self::Write {
                _background: EnvironmentContention::acquire_writer(
                    resources.background_readers(&node.env_id),
                )?,
                _lease: EnvironmentContention::acquire_writer(environment)?,
            }),
        }
    }
}

struct EnvironmentContention {
    error: tokio::sync::TryLockError,
    gate: Arc<RwLock<()>>,
    writer: bool,
}

impl EnvironmentContention {
    fn acquire_writer(
        gate: Arc<RwLock<()>>,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, Self> {
        gate.clone().try_write_owned().map_err(|error| Self {
            error,
            gate,
            writer: true,
        })
    }
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;
