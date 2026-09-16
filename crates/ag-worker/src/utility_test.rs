use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_protocol::AgentResponse;
use ag_runtime::{
    AgentRequestKind, OneShotClient, OneShotError, OneShotRequest, OneShotSubmission,
    PermissionMode, ReasoningLevel, SessionStats, SpeedMode,
};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    HeartbeatClock, RunClient, RunInfo, RunRepository, RunScope, RunState, RunWorker, in_scope,
    scoped_client,
};

#[derive(Default)]
struct Repository {
    closed_sessions: Mutex<HashSet<String>>,
    failure: AtomicU8,
    runs: Mutex<Vec<RunInfo>>,
    states: Mutex<Vec<RunState>>,
    heartbeats: AtomicU8,
}

#[async_trait]
impl RunRepository for Repository {
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError> {
        if self.failure.load(Ordering::SeqCst) == 6 {
            std::panic::resume_unwind(Box::new("repository panicked before insertion"));
        }
        if run.session_id.as_ref().is_some_and(|id| {
            self.closed_sessions
                .lock()
                .expect("closed sessions")
                .contains(id)
        }) {
            return Err(OneShotError::new("session closed"));
        }
        self.runs.lock().expect("runs").push(run.clone());
        if self.failure.load(Ordering::SeqCst) == 1 {
            return Err(OneShotError::new("create failed"));
        }
        Ok(())
    }

    async fn close_session(&self, session_id: &str) -> Result<(), OneShotError> {
        if self.failure.load(Ordering::SeqCst) == 5 {
            return Err(OneShotError::new("close failed"));
        }
        self.closed_sessions
            .lock()
            .expect("closed sessions")
            .insert(session_id.into());
        Ok(())
    }

    async fn transition(
        &self,
        _: &str,
        state: RunState,
        _: Option<&str>,
    ) -> Result<(), OneShotError> {
        self.states.lock().expect("states").push(state);
        let failure = self.failure.load(Ordering::SeqCst);
        if (failure == 2 && state == RunState::Running)
            || (failure == 3 && state != RunState::Running)
        {
            return Err(OneShotError::new("transition failed"));
        }
        Ok(())
    }

    async fn heartbeat(&self, _: &str) -> Result<(), OneShotError> {
        self.heartbeats.fetch_add(1, Ordering::SeqCst);
        if self.failure.load(Ordering::SeqCst) == 4 {
            return Err(OneShotError::new("heartbeat failed"));
        }
        Ok(())
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        Ok(())
    }
}

struct Call {
    request: OneShotRequest,
    result: oneshot::Sender<Result<OneShotSubmission, OneShotError>>,
    released: CancellationToken,
}

struct Runtime(mpsc::UnboundedSender<Call>);

#[async_trait]
impl OneShotClient for Runtime {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let (result, receiver) = oneshot::channel();
        let released = CancellationToken::new();
        let _guard = released.clone().drop_guard();
        self.0
            .send(Call {
                request,
                result,
                released,
            })
            .expect("observe call");
        receiver.await.expect("resolve runtime")
    }
}

fn fixture(
    concurrency: usize,
) -> (
    Arc<RunWorker>,
    Arc<Repository>,
    mpsc::UnboundedReceiver<Call>,
) {
    let (calls, receiver) = mpsc::unbounded_channel();
    let repository = Arc::new(Repository::default());
    let worker = Arc::new(RunWorker::new(
        Arc::new(Runtime(calls)),
        repository.clone(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::new(concurrency).expect("positive concurrency"),
    ));
    (worker, repository, receiver)
}

fn request() -> OneShotRequest {
    OneShotRequest {
        child_pid: None,
        folder: "repository".into(),
        harness: "test".into(),
        model: "model".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "summarize".into(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::UtilityPrompt,
        speed_mode: SpeedMode::default(),
    }
}

fn result() -> OneShotSubmission {
    OneShotSubmission {
        response: AgentResponse::plain("summary"),
        stats: SessionStats::default(),
    }
}

fn submit(
    worker: Arc<RunWorker>,
) -> tokio::task::JoinHandle<Result<OneShotSubmission, OneShotError>> {
    tokio::spawn(async move { worker.submit(request()).await })
}

#[tokio::test]
async fn independent_runs_execute_concurrently_with_preserved_requests() {
    // Arrange
    let (worker, repository, mut calls) = fixture(2);
    // Act
    let first = submit(worker.clone());
    let second = submit(worker.clone());
    let first_call = calls.recv().await.expect("first runtime");
    let second_call = calls.recv().await.expect("concurrent runtime");
    // Assert
    assert_eq!(first_call.request.prompt, "summarize");
    assert_eq!(first_call.request.permission_mode, PermissionMode::ReadOnly);
    first_call.result.send(Ok(result())).expect("first result");
    second_call
        .result
        .send(Err(OneShotError::new("provider failed")))
        .expect("failure");
    let outcomes = [first.await.expect("first"), second.await.expect("second")];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    worker.shutdown().await;
    let states = repository.states.lock().expect("states");
    assert!(states.contains(&RunState::Completed));
    assert!(states.contains(&RunState::Failed));
    let runs = repository.runs.lock().expect("runs");
    assert_ne!(runs[0].id, runs[1].id);
    assert_eq!(runs[0].purpose, "UtilityPrompt");
}

#[tokio::test]
async fn shutdown_cancels_active_and_queued_work_and_closes_admission() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    let first = submit(worker.clone());
    let call = calls.recv().await.expect("active runtime");
    let second = submit(worker.clone());
    while repository.runs.lock().expect("runs").len() < 2 {
        tokio::task::yield_now().await;
    }
    // Act
    worker.shutdown().await;
    // Assert
    assert!(call.released.is_cancelled());
    assert!(first.await.expect("first").is_err());
    assert!(second.await.expect("queued").is_err());
    assert!(calls.try_recv().is_err());
    assert!(worker.submit(request()).await.is_err());
    assert_eq!(
        repository
            .states
            .lock()
            .expect("states")
            .iter()
            .filter(|state| **state == RunState::Canceled)
            .count(),
        2
    );
}

#[tokio::test]
async fn dropping_caller_releases_runtime_and_records_cancellation() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    let caller = submit(worker.clone());
    let call = calls.recv().await.expect("active runtime");
    // Act
    caller.abort();
    let _ = caller.await;
    call.released.cancelled().await;
    worker.shutdown().await;
    // Assert
    assert!(
        repository
            .states
            .lock()
            .expect("states")
            .contains(&RunState::Canceled)
    );
}

#[tokio::test]
async fn session_cancellation_stops_only_its_owned_runs() {
    // Arrange
    let (worker, repository, mut calls) = fixture(2);
    let client = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("canceled-session".into()),
            ..RunScope::default()
        },
    );
    let owned = tokio::spawn(async move { client.submit(request()).await });
    let owned_call = calls.recv().await.expect("owned runtime");
    let independent = submit(worker.clone());
    let independent_call = calls.recv().await.expect("independent runtime");
    // Act
    worker.cancel_session("canceled-session");
    // Assert
    assert!(owned.await.expect("owned task").is_err());
    assert!(owned_call.released.is_cancelled());
    assert!(!independent_call.released.is_cancelled());
    independent_call
        .result
        .send(Ok(result()))
        .expect("independent result");
    assert!(independent.await.expect("independent task").is_ok());
    let late = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("canceled-session".into()),
            ..RunScope::default()
        },
    );
    assert!(late.submit(request()).await.is_err());
    assert!(calls.try_recv().is_err());
    worker.shutdown().await;
    assert!(
        repository
            .states
            .lock()
            .expect("states")
            .contains(&RunState::Canceled)
    );
}

#[tokio::test]
async fn precanceled_parent_never_invokes_runtime() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let client = scoped_client(
        worker.clone(),
        RunScope {
            cancellation: Some(cancellation),
            ..RunScope::default()
        },
    );
    // Act
    let result = client.submit(request()).await;
    worker.shutdown().await;
    // Assert
    assert!(result.is_err());
    assert!(calls.try_recv().is_err());
    assert_eq!(
        *repository.states.lock().expect("states"),
        [RunState::Canceled]
    );
}

#[tokio::test]
async fn nested_scope_crosses_spawn_and_preserves_parent_cancellation() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    let cancellation = CancellationToken::new();
    let client = in_scope(
        RunScope {
            parent_id: Some("operation".into()),
            project_id: Some(7),
            cancellation: Some(cancellation.clone()),
            ..RunScope::default()
        },
        async {
            scoped_client(
                worker.clone(),
                RunScope {
                    session_id: Some("session".into()),
                    purpose: Some("title".into()),
                    ..RunScope::default()
                },
            )
        },
    )
    .await;
    // Act
    let task = tokio::spawn(async move { client.submit(request()).await });
    let call = calls.recv().await.expect("child runtime");
    cancellation.cancel();
    assert!(task.await.expect("child").is_err());
    worker.shutdown().await;
    // Assert
    assert!(call.released.is_cancelled());
    let runs = repository.runs.lock().expect("runs");
    assert_eq!(runs[0].parent_id.as_deref(), Some("operation"));
    assert_eq!(runs[0].session_id.as_deref(), Some("session"));
    assert_eq!(runs[0].project_id, Some(7));
    assert_eq!(runs[0].purpose, "title");
}

#[tokio::test(start_paused = true)]
async fn heartbeats_track_live_runs_without_abandoning_them_on_store_failure() {
    // Arrange
    for failure in [0, 4] {
        let (worker, repository, mut calls) = fixture(1);
        repository.failure.store(failure, Ordering::SeqCst);
        let task = submit(worker.clone());
        let call = calls.recv().await.expect("runtime");
        // Act
        tokio::time::sleep(Duration::from_secs(31)).await;
        // Assert
        assert!(repository.heartbeats.load(Ordering::SeqCst) > 0);
        assert!(!call.released.is_cancelled());
        call.result.send(Ok(result())).expect("result");
        assert!(task.await.expect("task").is_ok());
        worker.shutdown().await;
    }
}

#[tokio::test]
async fn closed_execution_capacity_fails_before_invoking_runtime() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    worker.execution.capacity.close();
    // Act
    let result = worker.submit(request()).await;
    worker.shutdown().await;
    // Assert
    assert!(result.is_err());
    assert!(calls.try_recv().is_err());
    assert_eq!(
        *repository.states.lock().expect("states"),
        [RunState::Failed]
    );
}

#[tokio::test]
async fn persistence_failures_are_reported_and_prevent_untracked_execution() {
    // Arrange
    for failure in [1, 2, 3] {
        let (worker, repository, mut calls) = fixture(1);
        repository.failure.store(failure, Ordering::SeqCst);
        // Act
        let task = submit(worker.clone());
        if failure == 3 {
            calls
                .recv()
                .await
                .expect("runtime")
                .result
                .send(Ok(result()))
                .expect("result");
        }
        // Assert
        assert!(task.await.expect("worker").is_err());
        assert!(calls.try_recv().is_err());
        worker.shutdown().await;
    }
}

#[tokio::test]
async fn nested_independent_tokens_preserve_every_cancellation_source() {
    for canceled_index in 0..3 {
        // Arrange
        let (worker, repository, mut calls) = fixture(1);
        let tokens = [
            CancellationToken::new(),
            CancellationToken::new(),
            CancellationToken::new(),
        ];
        let base = scoped_client(worker.clone(), RunScope::default());
        let client = in_scope(
            RunScope {
                cancellation: Some(tokens[0].clone()),
                ..RunScope::default()
            },
            in_scope(
                RunScope {
                    cancellation: Some(tokens[1].clone()),
                    ..RunScope::default()
                },
                async {
                    scoped_client(
                        base,
                        RunScope {
                            cancellation: Some(tokens[2].clone()),
                            ..RunScope::default()
                        },
                    )
                },
            ),
        )
        .await;
        // Act
        let task = tokio::spawn(async move { client.submit(request()).await });
        let call = calls.recv().await.expect("nested runtime");
        tokens[canceled_index].cancel();
        task.await.expect("task").expect_err("canceled run");
        worker.shutdown().await;
        // Assert
        assert!(call.released.is_cancelled());
        assert_eq!(
            repository.states.lock().expect("states").last(),
            Some(&RunState::Canceled)
        );
        for (index, token) in tokens.iter().enumerate() {
            assert_eq!(token.is_cancelled(), index == canceled_index);
        }
    }
}

struct CleanupRuntime {
    started: CancellationToken,
    cleaning: CancellationToken,
    finish: CancellationToken,
}

#[async_trait]
impl OneShotClient for CleanupRuntime {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        std::future::pending().await
    }

    async fn submit_cancellable(
        &self,
        _: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        self.started.cancel();
        cancellation.cancelled().await;
        self.cleaning.cancel();
        self.finish.cancelled().await;
        Err(OneShotError::new("stopped"))
    }
}

#[tokio::test]
async fn session_cancellation_waits_for_runtime_cleanup_before_terminal_state() {
    // Arrange
    let runtime = Arc::new(CleanupRuntime {
        started: CancellationToken::new(),
        cleaning: CancellationToken::new(),
        finish: CancellationToken::new(),
    });
    let repository = Arc::new(Repository::default());
    let worker = Arc::new(RunWorker::new(
        runtime.clone(),
        repository.clone(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let client = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("session".into()),
            ..RunScope::default()
        },
    );
    let task = tokio::spawn(async move { client.submit(request()).await });
    runtime.started.cancelled().await;
    // Act
    let stopping = {
        let worker = worker.clone();
        tokio::spawn(async move { worker.cancel_session_and_wait("session").await })
    };
    runtime.cleaning.cancelled().await;
    // Assert
    assert!(!stopping.is_finished());
    assert!(!task.is_finished());
    assert_eq!(
        repository.states.lock().expect("states").last(),
        Some(&RunState::Running)
    );
    runtime.finish.cancel();
    stopping.await.expect("session stopped");
    task.await.expect("run task").expect_err("canceled");
    assert_eq!(
        repository.states.lock().expect("states").last(),
        Some(&RunState::Canceled)
    );
    worker.shutdown().await;
}

#[tokio::test]
async fn worker_task_failure_releases_session_tracking_and_reaches_the_caller() {
    // Arrange
    let mut runtime = ag_runtime::MockOneShotClient::new();
    runtime
        .expect_submit_cancellable()
        .once()
        .returning(|_, _| std::panic::resume_unwind(Box::new("runtime task failed")));
    let repository = Arc::new(Repository::default());
    let worker = Arc::new(RunWorker::new(
        Arc::new(runtime),
        repository.clone(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let client = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("failed".into()),
            ..RunScope::default()
        },
    );
    // Act
    let error = client
        .submit(request())
        .await
        .expect_err("failed worker task");
    worker.cancel_session_and_wait("failed").await;
    worker.shutdown().await;
    // Assert
    assert_eq!(error.to_string(), "Run worker task failed");
    assert_eq!(
        *repository.states.lock().expect("states"),
        [RunState::Running, RunState::Failed]
    );
    assert!(
        worker
            .execution
            .sessions
            .lock()
            .expect("sessions")
            .is_empty()
    );
}

#[tokio::test]
async fn completed_session_tracking_is_reclaimed_after_the_last_concurrent_run() {
    // Arrange
    let (worker, _, mut calls) = fixture(2);
    let client = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("active".into()),
            ..RunScope::default()
        },
    );
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.submit(request()).await }
    });
    let first_call = calls.recv().await.expect("first call");
    let second = tokio::spawn(async move { client.submit(request()).await });
    let second_call = calls.recv().await.expect("second call");
    // Act
    first_call.result.send(Ok(result())).expect("first result");
    first.await.expect("first task").expect("first submission");
    // Assert
    assert_eq!(worker.execution.sessions.lock().expect("sessions").len(), 1);
    second_call
        .result
        .send(Ok(result()))
        .expect("second result");
    second
        .await
        .expect("second task")
        .expect("second submission");
    worker.shutdown().await;
    assert!(
        worker
            .execution
            .sessions
            .lock()
            .expect("sessions")
            .is_empty()
    );
}

#[tokio::test]
async fn retired_sessions_are_evicted_and_late_submissions_remain_closed() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    // Act
    for id in 0..128 {
        worker.cancel_session_and_wait(&id.to_string()).await;
    }
    // Assert
    assert!(
        worker
            .execution
            .sessions
            .lock()
            .expect("sessions")
            .is_empty()
    );
    assert_eq!(
        repository
            .closed_sessions
            .lock()
            .expect("closed sessions")
            .len(),
        128
    );
    let late = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("0".into()),
            ..RunScope::default()
        },
    );
    assert!(late.submit(request()).await.is_err());
    worker.shutdown().await;
    assert!(calls.try_recv().is_err());
    assert!(
        worker
            .execution
            .sessions
            .lock()
            .expect("sessions")
            .is_empty()
    );
}

#[tokio::test]
async fn failed_admission_closure_retains_cancellation_until_persistence_recovers() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    repository.failure.store(5, Ordering::SeqCst);
    let client = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("closed".into()),
            ..RunScope::default()
        },
    );
    // Act
    worker.cancel_session_and_wait("closed").await;
    // Assert
    assert_eq!(worker.execution.sessions.lock().expect("sessions").len(), 1);
    assert!(client.submit(request()).await.is_err());
    assert!(calls.try_recv().is_err());
    repository.failure.store(0, Ordering::SeqCst);
    worker.cancel_session_and_wait("closed").await;
    assert!(
        worker
            .execution
            .sessions
            .lock()
            .expect("sessions")
            .is_empty()
    );
    worker.shutdown().await;
}

#[tokio::test]
async fn repository_panic_before_acceptance_reaches_the_caller() {
    // Arrange
    let (worker, repository, mut calls) = fixture(1);
    repository.failure.store(6, Ordering::SeqCst);
    // Act
    let error = worker
        .submit(request())
        .await
        .expect_err("repository panic");
    worker.shutdown().await;
    // Assert
    assert_eq!(error.to_string(), "Run worker task failed");
    assert!(repository.runs.lock().expect("runs").is_empty());
    assert!(calls.try_recv().is_err());
}

#[tokio::test]
async fn forced_shutdown_releases_uncooperative_runs_without_waiting_for_persistence() {
    // Arrange
    let started = CancellationToken::new();
    let cleaning = CancellationToken::new();
    let repository = Arc::new(Repository::default());
    let worker = Arc::new(RunWorker::new(
        Arc::new(CleanupRuntime {
            started: started.clone(),
            cleaning: cleaning.clone(),
            finish: CancellationToken::new(),
        }),
        repository.clone(),
        Arc::new(HeartbeatClock),
        NonZeroUsize::MIN,
    ));
    let client = scoped_client(
        worker.clone(),
        RunScope {
            session_id: Some("stuck".into()),
            ..RunScope::default()
        },
    );
    let task = tokio::spawn(async move { client.submit(request()).await });
    started.cancelled().await;
    let shutdown = tokio::spawn({
        let worker = worker.clone();
        async move { worker.shutdown().await }
    });
    cleaning.cancelled().await;
    // Act
    assert!(!shutdown.is_finished());
    worker.force_shutdown();
    tokio::time::timeout(Duration::from_secs(1), shutdown)
        .await
        .expect("forced stop")
        .expect("shutdown task");
    // Assert
    task.await.expect("caller").expect_err("forced stop");
    assert!(worker.submit(request()).await.is_err());
    assert!(
        worker
            .execution
            .sessions
            .lock()
            .expect("sessions")
            .is_empty()
    );
    assert_eq!(
        repository.states.lock().expect("states").last(),
        Some(&RunState::Running),
        "startup recovery owns unfinished records after forced exit"
    );
}
