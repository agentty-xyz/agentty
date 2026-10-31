use std::collections::{BTreeMap, HashMap};
use std::future::poll_fn;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use ag_contracts::{ExecutionPolicy, OneShotError, OneShotRequest, OneShotSubmission};
use ag_telemetry::{FutureExt as _, Outcome, Span};
use async_trait::async_trait;
use tokio::sync::{Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;
use tracing::warn;

use crate::scope::RunContext;
use crate::{Clock, RunInfo, RunRepository, RunState};

/// Application-facing submission boundary for worker-owned isolated runs.
/// Runtime adapters are injected into the worker, never exposed to workflows.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait RunClient: Send + Sync {
    /// Submits an isolated prompt. Dropping the caller cancels its execution;
    /// the worker retains ownership until terminal bookkeeping completes.
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError>;
}

/// Concurrent utility execution with durable lifecycle and application
/// shutdown. A session's serial workflow may await these child runs without
/// holding a utility execution slot itself.
pub struct RunWorker {
    admission: Mutex<bool>,
    execution: Arc<Execution>,
    tasks: TaskTracker,
}

impl RunWorker {
    /// Submits an exact utility run already admitted by a durable coordinator.
    /// The repository claims its queued identity before runtime invocation;
    /// redelivery and terminal records are rejected without changing them.
    ///
    /// # Errors
    /// Rejects mismatched or unclaimable records, closed admission, and runtime
    /// or persistence failures.
    pub async fn submit_recorded(
        &self,
        request: OneShotRequest,
        run: RunInfo,
    ) -> Result<OneShotSubmission, OneShotError> {
        if request.folder != run.folder {
            return Err(OneShotError::new(
                "Recorded run folder does not match request",
            ));
        }

        self.submit_inner(request, Some(run)).await
    }

    /// Runs host coordination under an exact preadmitted utility identity.
    ///
    /// The record represents work that awaits several child utilities, such
    /// as one phase of a graph node. It holds no utility execution slot, so
    /// children submitted by `work` never queue behind their waiting parent;
    /// they inherit this run as their parent and its ownership. The record is
    /// claimed before `work` starts, receives heartbeats while it runs, and
    /// settles after it returns. Session cancellation, worker shutdown, or the
    /// caller dropping this future cancels `work`, which cancels its children.
    /// Settlement and the result always wait for every child's cleanup and
    /// terminal bookkeeping, so resources the caller holds for this run, such
    /// as Env leases, outlive the children it started.
    ///
    /// # Errors
    /// Rejects closed admission and unclaimable records, and returns `work`'s
    /// failure, cancellation, panic, or terminal persistence errors.
    pub async fn coordinate_recorded<T: Send + 'static>(
        &self,
        run: RunInfo,
        work: impl Future<Output = Result<T, OneShotError>> + Send + 'static,
    ) -> Result<T, OneShotError> {
        let (sender, receiver) = oneshot::channel();
        {
            let admission = self
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*admission {
                return Err(OneShotError::new("Run worker has shut down"));
            }
            let execution = Arc::clone(&self.execution);
            let mut context = RunContext::current();
            context.scope.parent_id = Some(run.id.clone());
            context.scope.project_id = run.project_id;
            context.scope.purpose = Some(run.purpose.clone());
            context.scope.session_id.clone_from(&run.session_id);
            let trace = context.trace.clone();
            let session = run
                .session_id
                .as_deref()
                .map(|id| execution.admit_session(id));
            let session_cancellation = session
                .as_ref()
                .map_or_else(CancellationToken::new, |(session, _)| {
                    session.cancellation.clone()
                });
            self.tasks.spawn(
                async move {
                    let session_id = run.session_id.clone();
                    tokio::select! {
                        biased;
                        () = execution.force_shutdown.cancelled() => {},
                        () = execution.coordinate(run, work, context, session_cancellation, sender) => {},
                    }
                    if let (Some(session_id), Some((session, task))) = (session_id, session) {
                        drop(task);
                        execution.release_session(&session_id, &session);
                    }
                }
                .with_context(trace),
            );
        }

        receiver
            .await
            .map_err(|_| OneShotError::new("Run worker task failed"))?
    }

    /// Creates provider runtimes inside the worker, retaining host storage and
    /// clock injection.
    pub fn new(
        config: &crate::RuntimeConfig,
        repository: Arc<dyn RunRepository>,
        clock: Arc<dyn Clock>,
        concurrency: NonZeroUsize,
    ) -> Self {
        Self::from_runtime(
            config.factory.utility(),
            repository,
            clock,
            concurrency,
            config.policies.clone(),
        )
    }

    /// Cancels this session's utilities and rejects late submissions from
    /// detached tasks. Other sessions continue independently.
    pub fn cancel_session(&self, session_id: &str) {
        self.execution.cancel_session(session_id);
    }

    /// Cancels a session and waits for its admitted utilities to release
    /// runtime resources and persist terminal state before the host deletes
    /// the session. Durable admission closure lets settled session state be
    /// reclaimed without allowing late submissions. Hosts must finish canceled
    /// sessions even when they retain the session record.
    pub async fn cancel_session_and_wait(&self, session_id: &str) {
        let session = self.execution.cancel_session(session_id);
        match self.execution.repository.close_session(session_id).await {
            Ok(()) => session.closed.store(true, Ordering::Release),
            Err(error) => {
                warn!(session_id, %error, "Retaining session cancellation after admission persistence failed");
            }
        }
        session.tasks.close();
        session.tasks.wait().await;
        self.execution.release_session(session_id, &session);
    }

    /// Closes admission, cancels queued and running work, and waits for cleanup
    /// and terminal persistence. Call before shutting down the host runtime.
    pub async fn shutdown(&self) {
        self.close_admission();
        self.tasks.wait().await;
        self.execution.runtime.close().await;
    }

    /// Stops waiting for graceful cleanup after the host deadline. Drops
    /// worker futures and escalates detached runtime tasks without waiting
    /// for persistence; startup recovery settles any unfinished run records.
    pub fn force_shutdown(&self) {
        self.close_admission();
        self.execution.force_shutdown.cancel();
        self.execution.runtime.force_shutdown();
    }

    /// Composes the runtime and storage boundaries with bounded concurrency.
    fn from_runtime(
        runtime: ag_runtime::UtilityRuntime,
        repository: Arc<dyn RunRepository>,
        clock: Arc<dyn Clock>,
        concurrency: NonZeroUsize,
        policies: BTreeMap<String, ExecutionPolicy>,
    ) -> Self {
        Self {
            admission: Mutex::new(true),
            execution: Arc::new(Execution {
                sessions: Mutex::new(HashMap::new()),
                capacity: Semaphore::new(concurrency.get()),
                policies,
                clock,
                force_shutdown: CancellationToken::new(),
                repository,
                runtime,
                shutdown: CancellationToken::new(),
            }),
            tasks: TaskTracker::new(),
        }
    }

    fn close_admission(&self) {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *admission = false;
        self.execution.shutdown.cancel();
        self.tasks.close();
    }
}

impl Drop for RunWorker {
    fn drop(&mut self) {
        self.execution.shutdown.cancel();
    }
}

#[async_trait]
impl RunClient for RunWorker {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        self.submit_inner(request, None).await
    }
}

impl RunWorker {
    async fn submit_inner(
        &self,
        request: OneShotRequest,
        recorded: Option<RunInfo>,
    ) -> Result<OneShotSubmission, OneShotError> {
        let (sender, receiver) = oneshot::channel();
        {
            let admission = self
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*admission {
                return Err(OneShotError::new("Run worker has shut down"));
            }
            let execution = Arc::clone(&self.execution);
            let mut scope = RunContext::current();
            if let Some(run) = &recorded {
                scope.scope.session_id.clone_from(&run.session_id);
                scope.scope.parent_id.clone_from(&run.parent_id);
                scope.scope.project_id = run.project_id;
                scope.scope.purpose = Some(run.purpose.clone());
            }
            let trace = scope.trace.clone();
            let session = scope
                .scope
                .session_id
                .as_deref()
                .map(|id| execution.admit_session(id));
            let cancellation = session
                .as_ref()
                .map_or_else(CancellationToken::new, |(session, _)| {
                    session.cancellation.clone()
                });
            let id = uuid::Uuid::new_v4().to_string();
            self.tasks.spawn(
                async move {
                    let session_id = scope.scope.session_id.clone();
                    tokio::select! {
                        biased;
                        () = execution.force_shutdown.cancelled() => {},
                        () = execution.execute(request, scope, &id, recorded, cancellation, sender) => {},
                    }
                    if let (Some(session_id), Some((session, task))) = (session_id, session) {
                        drop(task);
                        execution.release_session(&session_id, &session);
                    }
                }
                .with_context(trace),
            );
        }

        receiver
            .await
            .map_err(|_| OneShotError::new("Run worker task failed"))?
    }
}

struct Execution {
    capacity: Semaphore,
    clock: Arc<dyn Clock>,
    force_shutdown: CancellationToken,
    policies: BTreeMap<String, ExecutionPolicy>,
    repository: Arc<dyn RunRepository>,
    runtime: ag_runtime::UtilityRuntime,
    sessions: Mutex<HashMap<String, Arc<SessionExecution>>>,
    shutdown: CancellationToken,
}

#[derive(Default)]
struct SessionExecution {
    cancellation: CancellationToken,
    closed: AtomicBool,
    tasks: TaskTracker,
}

impl Execution {
    fn cancel_session(&self, session_id: &str) -> Arc<SessionExecution> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = sessions.entry(session_id.to_string()).or_default();
        session.cancellation.cancel();

        session.clone()
    }

    fn admit_session(&self, session_id: &str) -> (Arc<SessionExecution>, TaskTrackerToken) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = sessions.entry(session_id.to_string()).or_default();

        (session.clone(), session.tasks.token())
    }

    fn release_session(&self, session_id: &str, session: &Arc<SessionExecution>) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if session.tasks.is_empty()
            && (!session.cancellation.is_cancelled() || session.closed.load(Ordering::Acquire))
            && sessions
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            sessions.remove(session_id);
        }
    }

    async fn execute(
        &self,
        request: OneShotRequest,
        context: RunContext,
        id: &str,
        recorded: Option<RunInfo>,
        session_cancellation: CancellationToken,
        mut sender: oneshot::Sender<Result<OneShotSubmission, OneShotError>>,
    ) {
        let scope = context.scope.clone();
        let run = recorded.clone().unwrap_or_else(|| RunInfo {
            folder: request.folder.clone(),
            id: id.to_string(),
            parent_id: scope.parent_id,
            project_id: scope.project_id,
            purpose: scope
                .purpose
                .unwrap_or_else(|| format!("{:?}", request.request_kind)),
            session_id: scope.session_id,
        });
        let span = Span::child("utility.run");
        if let Some(phase) = &context.phase_trace {
            span.link(phase);
        }
        span.attribute("agentty.harness", request.harness.clone());
        span.attribute("gen_ai.request.model", request.model.clone());
        if let Some(session_id) = &run.session_id {
            span.attribute("agentty.session.id", session_id.clone());
        }
        if let Some(parent_id) = &run.parent_id {
            span.attribute("agentty.operation.id", parent_id.clone());
        }
        span.attribute("agentty.run.id", run.id.clone());
        span.attribute("agentty.purpose", run.purpose.clone());
        let trace = span.context();
        let admitted = if recorded.is_some() {
            self.repository
                .claim_queued(&run)
                .await
                .and_then(|claimed| {
                    if claimed {
                        Ok(())
                    } else {
                        Err(OneShotError::new(
                            "Recorded run is not queued or does not match",
                        ))
                    }
                })
        } else {
            self.repository.create(&run).await
        };
        if let Err(error) = admitted {
            span.finish(Outcome::Failed);
            let _ = sender.send(Err(error));
            return;
        }
        let cancellation = CancellationToken::new();
        let work = self
            .run(&run.id, request, cancellation.clone())
            .with_context(trace);
        tokio::pin!(work);
        // Observe unwinding while polling the runtime, including cancellation
        // cleanup, so a provider panic cannot bypass terminal persistence.
        let work = poll_fn(|context| {
            catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(context)))
                .unwrap_or_else(|_| Poll::Ready(Err(OneShotError::new("Run worker task failed"))))
        });
        tokio::pin!(work);
        let canceled = || {
            (
                RunState::Canceled,
                Err(OneShotError::new("[Stopped] Agent run canceled")),
            )
        };
        let (state, result) = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => canceled(),
            () = context.cancelled() => canceled(),
            () = session_cancellation.cancelled() => canceled(),
            () = sender.closed() => canceled(),
            result = &mut work => {
                (if result.is_ok() { RunState::Completed } else { RunState::Failed }, result)
            }
        };
        if state == RunState::Canceled {
            cancellation.cancel();
            let _ = work.await;
        }
        let error = result.as_ref().err().map(ToString::to_string);
        let recorded = self
            .repository
            .transition(&run.id, state, error.as_deref())
            .await;
        let result = match recorded {
            Ok(()) => result,
            Err(error) => {
                warn!(run_id = %run.id, %error, "Failed to record terminal agent run state");
                Err(error)
            }
        };
        span.finish(match state {
            RunState::Canceled => Outcome::Canceled,
            _ if result.is_ok() => Outcome::Completed,
            _ => Outcome::Failed,
        });
        let _ = sender.send(result);
    }

    /// Mirrors `execute` for host coordination: the claim precedes `work`,
    /// cancellation drops it, children drain their cleanup, and terminal state
    /// persists before the caller receives the result.
    async fn coordinate<T>(
        &self,
        run: RunInfo,
        work: impl Future<Output = Result<T, OneShotError>>,
        context: RunContext,
        session_cancellation: CancellationToken,
        mut sender: oneshot::Sender<Result<T, OneShotError>>,
    ) {
        let span = Span::child("utility.coordinate");
        span.attribute("agentty.run.id", run.id.clone());
        span.attribute("agentty.purpose", run.purpose.clone());
        let claimed = self
            .repository
            .claim_queued(&run)
            .await
            .and_then(|claimed| {
                if claimed {
                    Ok(())
                } else {
                    Err(OneShotError::new(
                        "Recorded run is not queued or does not match",
                    ))
                }
            });
        if let Err(error) = claimed {
            span.finish(Outcome::Failed);
            let _ = sender.send(Err(error));
            return;
        }
        // Every child retains a token until its own cancellation cleanup and
        // terminal bookkeeping finish, including children whose submission
        // was dropped by cancellation or a caller-side timeout.
        let children = TaskTracker::new();
        let mut child_context = context.clone();
        child_context.trace = span.context();
        child_context.retain(Arc::new(children.token()));
        let (state, result) = {
            let work = crate::scope::in_context(child_context, work);
            tokio::pin!(work);
            let work = poll_fn(|poll_context| {
                catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(poll_context))).unwrap_or_else(
                    |_| Poll::Ready(Err(OneShotError::new("Run worker task failed"))),
                )
            });
            tokio::pin!(work);
            let canceled = || {
                (
                    RunState::Canceled,
                    Err(OneShotError::new("[Stopped] Agent run canceled")),
                )
            };
            loop {
                tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => break canceled(),
                    () = context.cancelled() => break canceled(),
                    () = session_cancellation.cancelled() => break canceled(),
                    () = sender.closed() => break canceled(),
                    result = &mut work => {
                        break (if result.is_ok() { RunState::Completed } else { RunState::Failed }, result);
                    }
                    () = self.clock.wait() => {
                        if let Err(error) = self.repository.heartbeat(&run.id).await {
                            warn!(run_id = %run.id, %error, "Agent run heartbeat failed");
                        }
                    }
                }
            }
        };
        // Dropping the work above cancels its children; settle only after they
        // release resources the caller holds for this run, such as Env leases.
        children.close();
        children.wait().await;
        let error = result.as_ref().err().map(ToString::to_string);
        let result = match self
            .repository
            .transition(&run.id, state, error.as_deref())
            .await
        {
            Ok(()) => result,
            Err(error) => {
                warn!(run_id = %run.id, %error, "Failed to record terminal agent run state");
                Err(error)
            }
        };
        span.finish(match state {
            RunState::Canceled => Outcome::Canceled,
            _ if result.is_ok() => Outcome::Completed,
            _ => Outcome::Failed,
        });
        let _ = sender.send(result);
    }

    async fn run(
        &self,
        id: &str,
        mut request: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        request.execution_policy = request
            .harness
            .parse::<ag_session::AgentKind>()
            .ok()
            .and_then(|kind| self.policies.get(&kind.to_string()))
            .cloned()
            .unwrap_or_default();
        let admission = Span::child("admission.wait");
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(OneShotError::new("[Stopped] Agent run canceled")),
            permit = self.capacity.acquire() => permit,
        };
        let _permit = permit.map_err(|error| OneShotError::new(error.to_string()))?;
        admission.finish(Outcome::Completed);
        self.repository
            .transition(id, RunState::Running, None)
            .await?;
        let turn = self.runtime.submit_cancellable(request, cancellation);
        tokio::pin!(turn);
        loop {
            tokio::select! {
                result = &mut turn => return result,
                () = self.clock.wait() => {
                    tokio::select! {
                        result = &mut turn => return result,
                        heartbeat = self.repository.heartbeat(id) => {
                            if let Err(error) = heartbeat {
                                warn!(run_id = id, %error, "Agent run heartbeat failed");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "utility_test.rs"]
mod tests;

#[cfg(test)]
#[path = "utility_coordinate_test.rs"]
mod coordinate_tests;

#[cfg(any(test, feature = "test-utils"))]
#[path = "utility_support_test.rs"]
mod support;
