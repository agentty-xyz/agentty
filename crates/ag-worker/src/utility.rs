use std::collections::HashMap;
use std::future::poll_fn;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use ag_runtime::{OneShotClient, OneShotError, OneShotRequest, OneShotSubmission};
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
    /// Composes the runtime and storage boundaries with bounded concurrency.
    pub fn new(
        runtime: Arc<dyn OneShotClient>,
        repository: Arc<dyn RunRepository>,
        clock: Arc<dyn Clock>,
        concurrency: NonZeroUsize,
    ) -> Self {
        Self {
            admission: Mutex::new(true),
            execution: Arc::new(Execution {
                sessions: Mutex::new(HashMap::new()),
                capacity: Semaphore::new(concurrency.get()),
                clock,
                force_shutdown: CancellationToken::new(),
                repository,
                runtime,
                shutdown: CancellationToken::new(),
            }),
            tasks: TaskTracker::new(),
        }
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
    }

    /// Stops waiting for graceful cleanup after the host deadline. Drops
    /// worker futures and escalates detached runtime tasks without waiting
    /// for persistence; startup recovery settles any unfinished run records.
    pub fn force_shutdown(&self) {
        self.close_admission();
        self.execution.force_shutdown.cancel();
        self.execution.runtime.force_shutdown();
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
            let scope = RunContext::current();
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
            self.tasks.spawn(async move {
                let session_id = scope.scope.session_id.clone();
                tokio::select! {
                    biased;
                    () = execution.force_shutdown.cancelled() => {},
                    () = execution.execute(request, scope, &id, cancellation, sender) => {},
                }
                if let (Some(session_id), Some((session, task))) = (session_id, session) {
                    drop(task);
                    execution.release_session(&session_id, &session);
                }
            });
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
    repository: Arc<dyn RunRepository>,
    runtime: Arc<dyn OneShotClient>,
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
        session_cancellation: CancellationToken,
        mut sender: oneshot::Sender<Result<OneShotSubmission, OneShotError>>,
    ) {
        let scope = context.scope.clone();
        let run = RunInfo {
            folder: request.folder.clone(),
            id: id.to_string(),
            parent_id: scope.parent_id,
            project_id: scope.project_id,
            purpose: scope
                .purpose
                .unwrap_or_else(|| format!("{:?}", request.request_kind)),
            session_id: scope.session_id,
        };
        if let Err(error) = self.repository.create(&run).await {
            let _ = sender.send(Err(error));
            return;
        }
        let cancellation = CancellationToken::new();
        let work = self.run(&run.id, request, cancellation.clone());
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
        let _ = sender.send(result);
    }

    async fn run(
        &self,
        id: &str,
        request: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(OneShotError::new("[Stopped] Agent run canceled")),
            permit = self.capacity.acquire() => permit,
        };
        let _permit = permit.map_err(|error| OneShotError::new(error.to_string()))?;
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
