use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, OneShotClient, OneShotError, OneShotRequest, OneShotSubmission,
    PermissionMode, ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::AgentResponse;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{Clock, RunClient, RunInfo, RunRepository, RunScope, RunState, RunWorker, in_scope};

/// Records lifecycle writes and supports preadmitted claims.
#[derive(Default)]
struct Repository {
    claim: Mutex<Option<Result<bool, OneShotError>>>,
    created: Mutex<Vec<RunInfo>>,
    fail_heartbeat: AtomicBool,
    fail_terminal: AtomicBool,
    heartbeats: AtomicUsize,
    transitions: Mutex<Vec<(String, RunState, Option<String>)>>,
}

impl Repository {
    fn transitions(&self) -> Vec<(String, RunState, Option<String>)> {
        self.transitions.lock().expect("transitions").clone()
    }
}

#[async_trait]
impl RunRepository for Repository {
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError> {
        self.created.lock().expect("created").push(run.clone());

        Ok(())
    }

    async fn claim_queued(&self, _: &RunInfo) -> Result<bool, OneShotError> {
        self.claim.lock().expect("claim").take().unwrap_or(Ok(true))
    }

    async fn close_session(&self, _: &str) -> Result<(), OneShotError> {
        Ok(())
    }

    async fn transition(
        &self,
        id: &str,
        state: RunState,
        error: Option<&str>,
    ) -> Result<(), OneShotError> {
        self.transitions.lock().expect("transitions").push((
            id.into(),
            state,
            error.map(str::to_string),
        ));
        if self.fail_terminal.load(Ordering::SeqCst) && state != RunState::Running {
            return Err(OneShotError::new("terminal write failed"));
        }

        Ok(())
    }

    async fn heartbeat(&self, _: &str) -> Result<(), OneShotError> {
        self.heartbeats.fetch_add(1, Ordering::SeqCst);
        if self.fail_heartbeat.load(Ordering::SeqCst) {
            return Err(OneShotError::new("heartbeat failed"));
        }

        Ok(())
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        Ok(())
    }
}

/// Completes every child immediately.
struct Runtime;

#[async_trait]
impl OneShotClient for Runtime {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        Ok(OneShotSubmission {
            response: AgentResponse::plain("child"),
            stats: SessionStats::default(),
        })
    }
}

/// Fires one heartbeat, then waits forever.
#[derive(Default)]
struct OneTick(AtomicBool);

#[async_trait]
impl Clock for OneTick {
    async fn wait(&self) {
        if self.0.swap(true, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
    }
}

fn worker(repository: &Arc<Repository>, clock: Arc<dyn Clock>) -> Arc<RunWorker> {
    Arc::new(RunWorker::with_client(
        Arc::new(Runtime),
        repository.clone(),
        clock,
        NonZeroUsize::MIN,
    ))
}

fn run() -> RunInfo {
    RunInfo {
        folder: "repository".into(),
        id: "node".into(),
        parent_id: Some("graph".into()),
        project_id: Some(7),
        purpose: "graph:node".into(),
        session_id: Some("session".into()),
    }
}

fn request() -> OneShotRequest {
    OneShotRequest {
        activity_tx: None,
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        child_pid: None,
        folder: "repository".into(),
        harness: "test".into(),
        model: "model".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "child".into(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::UtilityPrompt,
        speed_mode: SpeedMode::default(),
    }
}

#[tokio::test]
async fn coordination_settles_its_record_after_children_inherit_it_as_parent() {
    for session_id in [Some("session"), None] {
        // Arrange: session-owned runs take session admission; project-only
        // runs coordinate without it.
        let repository = Arc::new(Repository::default());
        let worker = worker(&repository, Arc::new(OneTick::default()));
        let child_worker = worker.clone();
        let mut run = run();
        run.session_id = session_id.map(str::to_string);

        // Act
        let result = worker
            .coordinate_recorded(run, async move {
                tokio::task::yield_now().await;
                let first = child_worker.submit(request()).await?;
                let second = child_worker.submit(request()).await?;

                Ok(format!(
                    "{} {}",
                    first.response.answer, second.response.answer
                ))
            })
            .await;
        worker.shutdown().await;

        // Assert: the parent holds no slot, so both children ran at capacity
        // one.
        assert_eq!(result.expect("coordination"), "child child");
        let children = repository.created.lock().expect("created").clone();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|child| {
            child.parent_id.as_deref() == Some("node")
                && child.session_id.as_deref() == session_id
                && child.project_id == Some(7)
        }));
        assert!(
            repository
                .transitions()
                .contains(&("node".into(), RunState::Completed, None))
        );
        assert!(repository.heartbeats.load(Ordering::SeqCst) > 0);
    }
}

#[tokio::test]
async fn rejected_claims_and_closed_admission_never_start_coordination() {
    for claim in [Ok(false), Err(OneShotError::new("claim failed"))] {
        // Arrange
        let repository = Arc::new(Repository::default());
        *repository.claim.lock().expect("claim") = Some(claim.clone());
        let worker = worker(&repository, Arc::new(OneTick::default()));
        let started = Arc::new(AtomicBool::new(false));
        let observed = started.clone();

        // Act
        let result = worker
            .coordinate_recorded(run(), async move {
                observed.store(true, Ordering::SeqCst);

                Ok(())
            })
            .await;
        worker.shutdown().await;
        let closed = worker.coordinate_recorded(run(), async { Ok(()) }).await;

        // Assert
        let expected = claim.map_or_else(
            |error| error.to_string(),
            |_| "Recorded run is not queued or does not match".into(),
        );
        assert_eq!(result.expect_err("rejected claim").to_string(), expected);
        assert!(!started.load(Ordering::SeqCst));
        assert_eq!(repository.transitions(), []);
        assert_eq!(
            closed.expect_err("closed admission").to_string(),
            "Run worker has shut down"
        );
    }
}

#[tokio::test]
async fn failures_panics_and_terminal_write_errors_reach_the_caller() {
    for scenario in ["failure", "panic", "terminal"] {
        // Arrange
        let repository = Arc::new(Repository::default());
        repository
            .fail_terminal
            .store(scenario == "terminal", Ordering::SeqCst);
        repository.fail_heartbeat.store(true, Ordering::SeqCst);
        let worker = worker(&repository, Arc::new(OneTick::default()));

        // Act
        let result = worker
            .coordinate_recorded(run(), async move {
                tokio::task::yield_now().await;
                match scenario {
                    "failure" => Err(OneShotError::new("phase failed")),
                    "panic" => std::panic::resume_unwind(Box::new("phase panicked")),
                    _ => Ok(()),
                }
            })
            .await;
        worker.shutdown().await;

        // Assert
        let (error, state) = match scenario {
            "failure" => ("phase failed", RunState::Failed),
            "panic" => ("Run worker task failed", RunState::Failed),
            _ => ("terminal write failed", RunState::Completed),
        };
        assert_eq!(result.expect_err("reported error").to_string(), error);
        assert_eq!(repository.transitions()[0].1, state);
    }
}

#[tokio::test]
async fn every_cancellation_source_drops_coordination_and_records_cancellation() {
    for source in ["session", "scope", "caller", "shutdown"] {
        // Arrange
        let repository = Arc::new(Repository::default());
        let worker = worker(&repository, Arc::new(OneTick::default()));
        let started = CancellationToken::new();
        let dropped = CancellationToken::new();
        let scope = CancellationToken::new();
        let work = {
            let (started, dropped) = (started.clone(), dropped.clone());
            async move {
                let _dropped = dropped.drop_guard();
                started.cancel();
                std::future::pending::<Result<(), OneShotError>>().await
            }
        };
        let caller = tokio::spawn({
            let worker = worker.clone();
            let scope = scope.clone();
            async move {
                in_scope(
                    RunScope {
                        cancellation: Some(scope),
                        ..RunScope::default()
                    },
                    worker.coordinate_recorded(run(), work),
                )
                .await
            }
        });
        started.cancelled().await;

        // Act
        match source {
            "session" => worker.cancel_session("session"),
            "scope" => scope.cancel(),
            "caller" => caller.abort(),
            _ => worker.shutdown().await,
        }
        dropped.cancelled().await;
        let result = caller.await;
        worker.shutdown().await;

        // Assert
        if source != "caller" {
            assert_eq!(
                result
                    .expect("caller task")
                    .expect_err("canceled")
                    .to_string(),
                "[Stopped] Agent run canceled"
            );
        }
        assert_eq!(
            repository.transitions(),
            [(
                "node".into(),
                RunState::Canceled,
                Some("[Stopped] Agent run canceled".into())
            )]
        );
    }
}

/// Signals each child start, then cleans up asynchronously when canceled.
struct CleanupRuntime {
    cleaning: CancellationToken,
    finish: CancellationToken,
    started: CancellationToken,
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

        Err(OneShotError::new("[Stopped] Agent run canceled"))
    }
}

#[tokio::test]
async fn coordination_settles_only_after_dropped_children_finish_cleanup() {
    for trigger in ["cancellation", "timeout"] {
        // Arrange
        let runtime = Arc::new(CleanupRuntime {
            cleaning: CancellationToken::new(),
            finish: CancellationToken::new(),
            started: CancellationToken::new(),
        });
        let repository = Arc::new(Repository::default());
        let worker = Arc::new(RunWorker::with_client(
            runtime.clone(),
            repository.clone(),
            Arc::new(OneTick::default()),
            NonZeroUsize::MIN,
        ));
        let scope = CancellationToken::new();
        let caller = tokio::spawn({
            let (worker, scope) = (worker.clone(), scope.clone());
            async move {
                let child_worker = worker.clone();
                let work = async move {
                    // A caller-side deadline drops the child submission.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(20),
                        child_worker.submit(request()),
                    )
                    .await;

                    Ok(())
                };
                in_scope(
                    RunScope {
                        cancellation: Some(scope),
                        ..RunScope::default()
                    },
                    worker.coordinate_recorded(run(), work),
                )
                .await
            }
        });
        runtime.started.cancelled().await;

        // Act
        if trigger == "cancellation" {
            scope.cancel();
        }
        runtime.cleaning.cancelled().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let settled_early = caller.is_finished()
            || repository
                .transitions()
                .iter()
                .any(|(id, _, _)| id == "node");
        runtime.finish.cancel();
        let result = caller.await.expect("caller task");
        worker.shutdown().await;

        // Assert
        assert!(
            !settled_early,
            "{trigger}: node settled during child cleanup"
        );
        assert_eq!(result.is_ok(), trigger == "timeout", "{trigger}");
        let transitions = repository.transitions();
        let child = transitions
            .iter()
            .position(|(id, state, _)| id != "node" && *state == RunState::Canceled)
            .expect("child cancellation");
        let node = transitions
            .iter()
            .position(|(id, _, _)| id == "node")
            .expect("node settlement");
        assert!(child < node, "{trigger}: {transitions:?}");
    }
}

#[tokio::test]
async fn forced_shutdown_abandons_coordination_without_terminal_persistence() {
    // Arrange
    let repository = Arc::new(Repository::default());
    let worker = worker(&repository, Arc::new(OneTick::default()));
    let started = CancellationToken::new();
    let caller = tokio::spawn({
        let worker = worker.clone();
        let started = started.clone();
        async move {
            worker
                .coordinate_recorded(run(), async move {
                    started.cancel();
                    std::future::pending::<Result<(), OneShotError>>().await
                })
                .await
        }
    });
    started.cancelled().await;

    // Act
    worker.force_shutdown();

    // Assert
    assert_eq!(
        caller
            .await
            .expect("caller task")
            .expect_err("forced stop")
            .to_string(),
        "Run worker task failed"
    );
    assert_eq!(repository.transitions(), []);
}
