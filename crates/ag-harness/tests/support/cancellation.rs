//! Public cancellation contract, also run as a source coverage suite.

use std::future::{Future, pending};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use ag_harness::{
    AcquiredTurn, FileSystem, Harness, HostRequest, HostTurnAcquisition, HostTurnRecord,
    LoadedSession, LocalFileSystem, MemoryStore, Model, ModelCompletion, ModelError, ModelMessage,
    ModelMetadata, ModelRequest, ModelResponse, NewSession, OutputSchema, SessionError,
    SessionStore, SqliteStore, StoreIdentity, Tool, ToolCall, ToolPolicy, TurnControl, TurnError,
    TurnErrorType, TurnLimits, TurnOptions, TurnOutcome, TurnOwner, WriteRecord, WriteStatus,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncRead;
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};

use crate::repository_fixture::repository_with_host_git;
use crate::store_conformance_test::ExternalStore;

fn schema() -> OutputSchema {
    OutputSchema::new(json!({"type":"object"})).expect("schema")
}

fn options() -> TurnOptions {
    TurnOptions::new(schema(), ToolPolicy::default(), TurnLimits::default())
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), future)
        .await
        .expect("bounded operation")
}

async fn unsettled(control: &TurnControl) {
    assert!(
        timeout(Duration::from_millis(20), control.settled())
            .await
            .is_err()
    );
}

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    dropped: Notify,
    entered: Notify,
}

struct DropNotice(Arc<Probe>);

impl Drop for DropNotice {
    fn drop(&mut self) {
        self.0.dropped.notify_one();
    }
}

struct TestModel(Arc<Probe>);

#[async_trait]
impl Model for TestModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        let _notice = DropNotice(Arc::clone(&self.0));
        if matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        ) {
            return Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({}),
            )));
        }
        match request.prompt() {
            "wait" => pending().await,
            "panic" => std::panic::resume_unwind(Box::new("injected model panic")),
            "write" => Ok(ModelCompletion::from_response(ModelResponse::ToolCall(
                ToolCall::from_json(
                    "write".into(),
                    "write",
                    &json!({
                        "path":"file.txt",
                        "patch":"--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1 @@\n+new\n"
                    })
                    .to_string(),
                    None,
                )
                .expect("write call"),
            ))),
            "fail" => Err(ModelError::InvalidResponse),
            _ => Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({}),
            ))),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    None,
    Acquire,
    AcquireAck,
    AcquirePanic,
    Complete,
    CompleteAck,
    Fail,
    Cleanup,
    Renew,
    Intent,
    Outcome,
}

struct Gate {
    entered: Notify,
    fail_cleanup: AtomicBool,
    fail_outcome: AtomicBool,
    lookup: Notify,
    phase: Phase,
    release: Notify,
    store: Arc<dyn SessionStore>,
}

impl Gate {
    fn new(store: Arc<dyn SessionStore>, phase: Phase) -> Self {
        Self {
            entered: Notify::new(),
            fail_cleanup: AtomicBool::new(false),
            fail_outcome: AtomicBool::new(false),
            lookup: Notify::new(),
            phase,
            release: Notify::new(),
            store,
        }
    }

    async fn pause(&self, phase: Phase) {
        if self.phase == phase {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl SessionStore for Gate {
    fn identity(&self) -> &StoreIdentity {
        self.store.identity()
    }

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        budget: usize,
    ) -> Result<(), SessionError> {
        self.store.create_session(config, metadata, budget).await
    }

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        self.store.load_session(id).await
    }

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        prompt: &str,
        options: &TurnOptions,
    ) -> Result<AcquiredTurn, SessionError> {
        self.pause(Phase::Acquire).await;
        let turn = self.store.begin_turn(store, id, prompt, options).await?;
        self.pause(Phase::AcquireAck).await;
        Ok(turn)
    }

    async fn begin_request(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        prompt: &str,
        options: &TurnOptions,
        request: &HostRequest,
    ) -> Result<HostTurnAcquisition, SessionError> {
        if self.phase == Phase::AcquirePanic {
            std::panic::resume_unwind(Box::new("injected host acquisition panic"));
        }
        self.pause(Phase::Acquire).await;
        let turn = self
            .store
            .begin_request(store, id, prompt, options, request)
            .await?;
        self.pause(Phase::AcquireAck).await;

        Ok(turn)
    }

    async fn load_request(
        &self,
        id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        let result = self.store.load_request(id, host_id).await;
        self.lookup.notify_one();

        result
    }

    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        self.pause(Phase::Complete).await;
        self.store
            .complete_request(owner, messages, continuation, outcome)
            .await?;
        self.pause(Phase::CompleteAck).await;

        Ok(())
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.pause(Phase::Renew).await;
        self.store.renew(owner).await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
    ) -> Result<(), SessionError> {
        self.pause(Phase::Complete).await;
        self.store
            .complete_turn(owner, messages, continuation)
            .await?;
        self.pause(Phase::CompleteAck).await;
        Ok(())
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        self.pause(Phase::Fail).await;
        self.store.fail_turn(owner, error).await
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        self.pause(Phase::Cleanup).await;
        if self.fail_cleanup.load(Ordering::SeqCst) {
            return Err(SessionError::Store {
                operation: "cleanup",
                source: Box::new(std::io::Error::other("injected cleanup failure")),
            });
        }
        self.store.interrupt(owner).await
    }

    async fn load_writes(&self, id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        self.store.load_writes(id).await
    }

    async fn write_intent(
        &self,
        owner: &TurnOwner,
        call: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        let intent = self
            .store
            .write_intent(owner, call, root, path, expected, resulting)
            .await?;
        self.pause(Phase::Intent).await;
        Ok(intent)
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        self.pause(Phase::Outcome).await;
        if self.fail_outcome.load(Ordering::SeqCst) {
            return Err(SessionError::Store {
                operation: "record write outcome",
                source: Box::new(io::Error::other("injected outcome failure")),
            });
        }
        self.store.finish_write(owner, id, applied).await
    }
}

async fn stores() -> Vec<Arc<dyn SessionStore>> {
    vec![
        Arc::new(MemoryStore::new()),
        Arc::new(ExternalStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("sqlite"),
        ),
    ]
}

#[tokio::test]
async fn cancellation_before_poll_and_unstarted_drop_do_not_execute() {
    // Arrange
    let probe = Arc::new(Probe::default());
    let harness = Harness::new(TestModel(Arc::clone(&probe)));
    let turn = harness.run_once_controlled("wait", options());
    let control = turn.control();

    // Act
    control.cancel();
    control.cancel();
    let error = turn.await.expect_err("cancelled");
    bounded(control.settled()).await.expect("settled");
    bounded(control.effects_settled())
        .await
        .expect("effects settled");
    control.retry_settlement().await.expect("no cleanup needed");
    let unstarted = harness.run_once_controlled("wait", options());
    let unstarted_control = unstarted.control();
    drop(unstarted);
    bounded(unstarted_control.effects_settled())
        .await
        .expect("unstarted effects");
    bounded(unstarted_control.settled())
        .await
        .expect("unstarted settled");

    // Assert
    assert!(matches!(error, TurnError::Cancelled));
    assert_eq!(error.error_type(), TurnErrorType::Cancelled);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn one_shot_cancellation_drop_success_and_panic_settle_without_storage() {
    // Arrange
    for prompt in ["wait", "ok", "panic"] {
        let probe = Arc::new(Probe::default());
        let harness = Harness::new(TestModel(Arc::clone(&probe)));
        let mut turn = Box::pin(harness.run_once_controlled(prompt, options()));
        let control = turn.control();

        // Act
        if prompt == "wait" {
            tokio::select! {
                result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
                () = probe.entered.notified() => {},
            }
            drop(turn);
            bounded(probe.dropped.notified()).await;
        } else {
            let result = bounded(turn).await;
            assert_eq!(result.is_ok(), prompt == "ok");
        }
        bounded(control.settled()).await.expect("settled");
        control.cancel();

        // Assert
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn abandoned_acquisition_keeps_admission_and_never_executes() {
    // Arrange
    for store in stores().await {
        for phase in [Phase::Acquire, Phase::AcquireAck] {
            let gate = Arc::new(Gate::new(Arc::clone(&store), phase));
            let probe = Arc::new(Probe::default());
            let harness = Harness::new(TestModel(Arc::clone(&probe))).store(gate.clone());
            let id = if phase == Phase::Acquire {
                "before"
            } else {
                "after"
            };
            let mut session = harness
                .session(id, schema())
                .create()
                .await
                .expect("session");
            let mut successor = harness.resume(id).await.expect("successor");
            let mut turn = Box::pin(session.send_controlled("ok", options()));
            let control = turn.control();
            tokio::select! {
                result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
                () = gate.entered.notified() => {},
            }

            // Act
            control.cancel();
            assert!(matches!(
                bounded(turn).await,
                Err(SessionError::Turn(TurnError::Cancelled))
            ));
            unsettled(&control).await;
            assert!(matches!(
                successor.send("ok").await,
                Err(SessionError::Busy { .. })
            ));
            gate.release.notify_one();
            bounded(control.settled()).await.expect("cleanup settled");

            // Assert
            assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                store.load_session(id).await.expect("history").turns.len(),
                0
            );
            gate.release.notify_one();
            successor.send("ok").await.expect("successor completes");
        }
    }
}

#[tokio::test]
async fn terminal_acknowledgment_survives_cancellation_and_caller_drop() {
    // Arrange
    for store in stores().await {
        for (index, phase) in [Phase::Complete, Phase::CompleteAck, Phase::Fail]
            .into_iter()
            .enumerate()
        {
            let gate = Arc::new(Gate::new(Arc::clone(&store), phase));
            let probe = Arc::new(Probe::default());
            let harness = Harness::new(TestModel(probe)).store(gate.clone());
            let id = index.to_string();
            let mut session = harness
                .session(&id, schema())
                .create()
                .await
                .expect("session");
            let mut successor = harness.resume(&id).await.expect("successor");
            let prompt = if phase == Phase::Fail { "fail" } else { "ok" };
            let mut turn = Box::pin(session.send_controlled(prompt, options()));
            let control = turn.control();
            tokio::select! {
                result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
                () = gate.entered.notified() => {},
            }

            // Act
            if phase == Phase::CompleteAck {
                drop(turn);
            } else {
                control.cancel();
                assert!(matches!(
                    bounded(turn).await,
                    Err(SessionError::Turn(TurnError::Cancelled))
                ));
            }
            unsettled(&control).await;
            assert!(matches!(
                successor.send("ok").await,
                Err(SessionError::Busy { .. })
            ));
            gate.release.notify_one();
            bounded(control.settled())
                .await
                .expect("terminal persistence settled");
            control.cancel();
            control
                .retry_settlement()
                .await
                .expect("stale retry is inert");

            // Assert
            assert_eq!(
                store.load_session(&id).await.expect("history").turns.len(),
                usize::from(phase != Phase::Fail)
            );
            gate.release.notify_one();
            successor.send("ok").await.expect("successor unaffected");
        }
    }
}

#[tokio::test]
async fn cleanup_failure_is_observable_and_retry_is_owner_scoped() {
    // Arrange
    for store in stores().await {
        let gate = Arc::new(Gate::new(store, Phase::None));
        gate.fail_cleanup.store(true, Ordering::SeqCst);
        let probe = Arc::new(Probe::default());
        let harness = Harness::new(TestModel(Arc::clone(&probe))).store(gate.clone());
        let mut session = harness
            .session("session", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(session.send_controlled("wait", options()));
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = probe.entered.notified() => {},
        }

        // Act
        control.cancel();
        assert!(matches!(
            bounded(turn).await,
            Err(SessionError::Turn(TurnError::Cancelled))
        ));
        let error = bounded(control.settled())
            .await
            .expect_err("cleanup failed");
        assert!(bounded(control.retry_settlement()).await.is_err());
        assert!(session.send("ok").await.is_err());
        gate.fail_cleanup.store(false, Ordering::SeqCst);
        bounded(control.retry_settlement())
            .await
            .expect("cleanup recovered");
        bounded(control.settled()).await.expect("settled");
        let next = session.send_controlled("ok", options());
        let next_control = next.control();
        control.cancel();
        control.retry_settlement().await.expect("old retry inert");
        bounded(next).await.expect("next turn completes");
        bounded(next_control.settled()).await.expect("next settled");

        // Assert
        assert!(error.to_string().contains("injected cleanup failure"));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn provider_drop_is_prompt_while_cleanup_stalls() {
    // Arrange
    for store in stores().await {
        let gate = Arc::new(Gate::new(store, Phase::Cleanup));
        let probe = Arc::new(Probe::default());
        let harness = Harness::new(TestModel(Arc::clone(&probe))).store(gate.clone());
        let mut session = harness
            .session("session", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(session.send_controlled("wait", options()));
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = probe.entered.notified() => {},
        }

        // Act
        drop(turn);
        bounded(probe.dropped.notified()).await;
        bounded(gate.entered.notified()).await;
        unsettled(&control).await;
        gate.release.notify_one();

        // Assert
        bounded(control.settled())
            .await
            .expect("cleanup acknowledged");
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    }
}

struct DeferredReplacement {
    entered: Arc<Notify>,
    finished: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl FileSystem for DeferredReplacement {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        LocalFileSystem.canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        LocalFileSystem.open_beneath(root, path).await
    }

    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        let release = Arc::clone(&self.release);
        let finished = Arc::clone(&self.finished);
        let task = tokio::spawn(async move {
            release.notified().await;
            let result = LocalFileSystem
                .replace_beneath(&root, &path, expected, content)
                .await;
            finished.notify_one();

            result
        });
        self.entered.notify_one();

        task.await.expect("replacement worker")
    }
}

fn write_options() -> TurnOptions {
    TurnOptions::new(
        schema(),
        ToolPolicy::default().allow(Tool::Write),
        TurnLimits::default(),
    )
}

async fn effects_pending(control: &TurnControl) {
    assert!(
        timeout(Duration::from_millis(20), control.effects_settled())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn persistence_settlement_does_not_prove_filesystem_effect_completion() {
    // Arrange
    for store in stores().await {
        let root = tempfile::tempdir().expect("repository");
        let entered = Arc::new(Notify::new());
        let finished = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let harness = Harness::new(TestModel(Arc::new(Probe::default())))
            .store(store)
            .repository(repository_with_host_git(root.path()))
            .file_system(DeferredReplacement {
                entered: Arc::clone(&entered),
                finished: Arc::clone(&finished),
                release: Arc::clone(&release),
            });
        let mut session = harness
            .session("write", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(session.send_controlled("write", write_options()));
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = entered.notified() => {},
        }

        // Act
        control.cancel();
        assert!(matches!(
            bounded(turn).await,
            Err(SessionError::Turn(TurnError::Cancelled))
        ));
        bounded(control.settled())
            .await
            .expect("persistence settled");
        effects_pending(&control).await;

        // Assert
        assert!(!root.path().join("file.txt").exists());
        assert_eq!(
            session.writes().await.expect("journal")[0].status,
            WriteStatus::Pending
        );
        assert!(matches!(
            session.send("ok").await,
            Err(SessionError::Busy { .. })
        ));
        control.retry_settlement().await.expect("no cleanup needed");
        assert!(matches!(
            session.send("ok").await,
            Err(SessionError::Busy { .. })
        ));
        release.notify_one();
        bounded(finished.notified()).await;
        bounded(control.effects_settled())
            .await
            .expect("effects acknowledged");
        assert_eq!(
            tokio::fs::read(root.path().join("file.txt"))
                .await
                .expect("late replacement"),
            b"new\n"
        );
        assert_eq!(
            session.writes().await.expect("journal")[0].status,
            WriteStatus::Applied
        );
        let mut successor = Box::pin(session.send_controlled("wait", options()));
        let successor_control = successor.control();
        tokio::select! {
            result = &mut successor => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = tokio::time::sleep(Duration::from_millis(20)) => {},
        }
        control.cancel();
        control.retry_settlement().await.expect("stale retry");
        bounded(control.effects_settled())
            .await
            .expect("stale effects");
        unsettled(&successor_control).await;
        drop(successor);
        bounded(successor_control.settled())
            .await
            .expect("successor cleanup");
    }
}

#[tokio::test]
async fn host_request_cancellation_retains_effect_and_outcome_admission() {
    // Arrange
    for store in stores().await {
        let root = tempfile::tempdir().expect("repository");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let gate = Arc::new(Gate::new(store, Phase::Outcome));
        let probe = Arc::new(Probe::default());
        let harness = Harness::new(TestModel(Arc::clone(&probe)))
            .store(gate.clone())
            .execution_identity(ag_harness::ExecutionIdentity::new("model", "1").expect("identity"))
            .repository(repository_with_host_git(root.path()))
            .file_system(DeferredReplacement {
                entered: entered.clone(),
                finished: Arc::new(Notify::new()),
                release: release.clone(),
            });
        let mut session = harness
            .session("host-write", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(
            session
                .submit_controlled("write-id", "write", write_options())
                .expect("turn"),
        );
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = bounded(entered.notified()) => {},
        }

        // Act
        control.cancel();
        assert!(matches!(
            bounded(turn).await,
            Err(SessionError::Turn(TurnError::Cancelled))
        ));
        bounded(control.settled())
            .await
            .expect("persistence settled");
        effects_pending(&control).await;

        // Assert
        assert!(matches!(
            session.submit("write-id", "write", write_options()).await,
            Err(SessionError::HostTurnStopped(_))
        ));
        assert!(matches!(
            session.submit("successor", "ok", options()).await,
            Err(SessionError::Busy { .. })
        ));
        release.notify_one();
        bounded(gate.entered.notified()).await;
        effects_pending(&control).await;
        assert!(matches!(
            session.submit("successor", "ok", options()).await,
            Err(SessionError::Busy { .. })
        ));
        gate.release.notify_one();
        bounded(control.effects_settled())
            .await
            .expect("effects settled");
        let record = session
            .recover("write-id")
            .await
            .expect("lookup")
            .expect("record");
        assert_eq!(record.writes[0].status, WriteStatus::Applied);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        session
            .submit("successor", "ok", options())
            .await
            .expect("successor after effect settlement");
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn cancelled_intent_does_not_start_replacement() {
    // Arrange
    for store in stores().await {
        let root = tempfile::tempdir().expect("repository");
        let gate = Arc::new(Gate::new(store, Phase::Intent));
        let harness = Harness::new(TestModel(Arc::new(Probe::default())))
            .store(gate.clone())
            .repository(repository_with_host_git(root.path()));
        let mut session = harness
            .session("intent", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(session.send_controlled("write", write_options()));
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = gate.entered.notified() => {},
        }

        // Act
        drop(turn);
        bounded(control.settled()).await.expect("persistence");
        bounded(control.effects_settled())
            .await
            .expect("no effects");
        gate.release.notify_one();

        // Assert
        assert!(!root.path().join("file.txt").exists());
        assert_eq!(
            session.writes().await.expect("journal")[0].status,
            WriteStatus::Pending
        );
        session.send("ok").await.expect("successor");
    }
}

#[tokio::test]
async fn dropped_ordinary_turn_retains_replacement_and_admission() {
    // Arrange
    for store in stores().await {
        let root = tempfile::tempdir().expect("repository");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let gate = Arc::new(Gate::new(store, Phase::Outcome));
        let harness = Harness::new(TestModel(Arc::new(Probe::default())))
            .store(gate.clone())
            .repository(repository_with_host_git(root.path()))
            .file_system(DeferredReplacement {
                entered: entered.clone(),
                finished: Arc::new(Notify::new()),
                release: release.clone(),
            });
        let mut session = harness
            .session("ordinary", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(session.send_with_options("write", write_options()));
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = entered.notified() => {},
        }

        // Act
        drop(turn);
        assert!(matches!(
            session.send("ok").await,
            Err(SessionError::Busy { .. })
        ));
        release.notify_one();
        bounded(gate.entered.notified()).await;

        // Assert
        assert!(root.path().join("file.txt").exists());
        assert!(matches!(
            session.send("ok").await,
            Err(SessionError::Busy { .. })
        ));
        gate.release.notify_one();
        bounded(async {
            loop {
                match session.send("ok").await {
                    Ok(_) => break,
                    Err(SessionError::Busy { .. }) => tokio::task::yield_now().await,
                    Err(error) => std::panic::resume_unwind(Box::new(error)),
                }
            }
        })
        .await;
        assert_eq!(
            session.writes().await.expect("journal")[0].status,
            WriteStatus::Applied
        );
    }
}

#[tokio::test]
async fn cancelled_outcome_failure_is_observable_after_persistence_settlement() {
    // Arrange
    for store in stores().await {
        let root = tempfile::tempdir().expect("repository");
        let gate = Arc::new(Gate::new(store, Phase::Outcome));
        gate.fail_outcome.store(true, Ordering::SeqCst);
        let harness = Harness::new(TestModel(Arc::new(Probe::default())))
            .store(gate.clone())
            .repository(repository_with_host_git(root.path()));
        let mut session = harness
            .session("outcome", schema())
            .create()
            .await
            .expect("session");
        let mut turn = Box::pin(session.send_controlled("write", write_options()));
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = gate.entered.notified() => {},
        }

        // Act
        drop(turn);
        bounded(control.settled()).await.expect("persistence");
        effects_pending(&control).await;
        assert!(matches!(
            session.send("ok").await,
            Err(SessionError::Busy { .. })
        ));
        gate.release.notify_one();
        let error = bounded(control.effects_settled())
            .await
            .expect_err("outcome failure");

        // Assert
        assert!(!error.is_unresolved());
        assert!(error.to_string().contains("injected outcome failure"));
        assert!(root.path().join("file.txt").exists());
        assert_eq!(
            session.writes().await.expect("journal")[0].status,
            WriteStatus::Pending
        );
        session
            .send("ok")
            .await
            .expect("acknowledged effect permits successor");
    }
}

#[tokio::test]
async fn one_shot_drop_retains_effect_completion() {
    // Arrange
    let root = tempfile::tempdir().expect("repository");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = Harness::new(TestModel(Arc::new(Probe::default())))
        .repository(repository_with_host_git(root.path()))
        .file_system(DeferredReplacement {
            entered: entered.clone(),
            finished: Arc::new(Notify::new()),
            release: release.clone(),
        });
    let mut turn = Box::pin(harness.run_once_controlled("write", write_options()));
    let control = turn.control();
    tokio::select! {
        result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
        () = entered.notified() => {},
    }

    // Act
    drop(turn);
    bounded(control.settled()).await.expect("execution settled");
    effects_pending(&control).await;
    release.notify_one();
    bounded(control.effects_settled())
        .await
        .expect("effect completion");

    // Assert
    assert_eq!(
        tokio::fs::read(root.path().join("file.txt"))
            .await
            .expect("replacement"),
        b"new\n"
    );
}

#[tokio::test(start_paused = true)]
async fn stalled_renewal_and_terminal_persistence_keep_hard_deadlines() {
    // Arrange
    for phase in [Phase::Renew, Phase::Complete] {
        let store = Arc::new(ExternalStore::with_leases(
            Duration::from_secs(2),
            Duration::from_secs(2),
        ));
        let gate = Arc::new(Gate::new(store, phase));
        let harness = Harness::new(TestModel(Arc::new(Probe::default()))).store(gate.clone());
        let mut session = harness
            .session("deadline", schema())
            .create()
            .await
            .expect("session");
        let prompt = if phase == Phase::Renew { "wait" } else { "ok" };
        let mut turn = Box::pin(session.send_controlled(prompt, options()));
        let control = turn.control();
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = gate.entered.notified() => {},
        }

        // Act
        let error = bounded(turn).await.expect_err("hard deadline");
        bounded(control.settled()).await.expect("cleanup settled");

        // Assert
        assert!(matches!(error, SessionError::OwnershipLost { .. }));
    }
}

#[tokio::test]
async fn controlled_turns_reopen_sqlite_and_preserve_regular_results() {
    // Arrange
    let root = tempfile::tempdir().expect("database directory");
    let path = root.path().join("session.db");
    let probe = Arc::new(Probe::default());
    let harness = Harness::new(TestModel(Arc::clone(&probe))).database(&path);
    let mut session = harness
        .session("session", schema())
        .create()
        .await
        .expect("session");

    // Act
    let cancelled = session.send_controlled("wait", options());
    let control = cancelled.control();
    control.cancel();
    assert!(matches!(
        cancelled.await,
        Err(SessionError::Turn(TurnError::Cancelled))
    ));
    bounded(control.settled()).await.expect("settled");
    let success = session.send_controlled("ok", options());
    let success_control = success.control();
    bounded(success).await.expect("success");
    bounded(success_control.settled())
        .await
        .expect("success settled");
    let failure = session.send_controlled("fail", options());
    let failure_control = failure.control();
    assert!(matches!(
        bounded(failure).await,
        Err(SessionError::Turn(TurnError::Model(_)))
    ));
    bounded(failure_control.settled())
        .await
        .expect("failure settled");
    drop(session);
    drop(harness);
    let store = SqliteStore::open(&path).await.expect("reopen");

    // Assert
    assert_eq!(
        store
            .load_session("session")
            .await
            .expect("history")
            .turns
            .len(),
        1
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
}

struct BrokenReplacement {
    panic: bool,
}

#[async_trait]
impl FileSystem for BrokenReplacement {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        LocalFileSystem.canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        LocalFileSystem.open_beneath(root, path).await
    }

    async fn replace_beneath(
        &self,
        _root: &Path,
        _path: &Path,
        _expected: Option<Vec<u8>>,
        _content: Vec<u8>,
    ) -> io::Result<()> {
        if self.panic {
            std::panic::resume_unwind(Box::new("injected replacement panic"));
        }
        Err(io::Error::other("injected replacement failure"))
    }
}

#[tokio::test]
async fn write_failures_preserve_combined_diagnostics_and_unresolved_admission() {
    // Arrange
    for store in stores().await {
        for panic in [false, true] {
            let root = tempfile::tempdir().expect("repository");
            let gate = Arc::new(Gate::new(store.clone(), Phase::None));
            gate.fail_outcome.store(true, Ordering::SeqCst);
            let harness = Harness::new(TestModel(Arc::new(Probe::default())))
                .store(gate)
                .repository(repository_with_host_git(root.path()))
                .file_system(BrokenReplacement { panic });
            let id = if panic { "panic-write" } else { "failed-write" };
            let mut session = harness
                .session(id, schema())
                .create()
                .await
                .expect("session");
            let turn = session.send_controlled("write", write_options());
            let control = turn.control();

            // Act
            let error = bounded(turn).await.expect_err("write failed");
            bounded(control.settled())
                .await
                .expect("persistence settled");
            let effects = bounded(control.effects_settled())
                .await
                .expect_err("effect failure");
            control
                .retry_settlement()
                .await
                .expect("persistence retry does not replay");

            // Assert
            assert_eq!(effects.is_unresolved(), panic);
            assert_eq!(
                session.writes().await.expect("journal")[0].status,
                WriteStatus::Pending
            );
            if panic {
                drop(control);
                assert!(matches!(
                    session.send("ok").await,
                    Err(SessionError::Busy { .. })
                ));
            } else {
                assert!(error.to_string().contains("injected replacement failure"));
                assert!(error.to_string().contains("injected outcome failure"));
                assert!(effects.to_string().contains("injected replacement failure"));
                assert!(effects.to_string().contains("injected outcome failure"));
                session
                    .send("ok")
                    .await
                    .expect("acknowledged failure releases admission");
            }
        }
    }
}

#[tokio::test]
async fn lease_loss_settles_persistence_before_retained_replacement() {
    // Arrange
    let store = Arc::new(ExternalStore::with_leases(
        Duration::from_secs(2),
        Duration::from_secs(2),
    ));
    let gate = Arc::new(Gate::new(store, Phase::Renew));
    let root = tempfile::tempdir().expect("repository");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = Harness::new(TestModel(Arc::new(Probe::default())))
        .store(gate)
        .repository(repository_with_host_git(root.path()))
        .file_system(DeferredReplacement {
            entered: entered.clone(),
            finished: Arc::new(Notify::new()),
            release: release.clone(),
        });
    let mut session = harness
        .session("lease-write", schema())
        .create()
        .await
        .expect("session");
    let mut turn = Box::pin(session.send_controlled("write", write_options()));
    let control = turn.control();
    tokio::select! {
        result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
        () = entered.notified() => {},
    }

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::time::resume();
    assert!(matches!(
        bounded(turn).await,
        Err(SessionError::OwnershipLost { .. })
    ));
    bounded(control.settled()).await.expect("persistence");
    effects_pending(&control).await;
    assert!(matches!(
        session.send("ok").await,
        Err(SessionError::Busy { .. })
    ));
    release.notify_one();
    bounded(control.effects_settled())
        .await
        .expect("late outcome");

    // Assert
    assert_eq!(
        session.writes().await.expect("journal")[0].status,
        WriteStatus::Applied
    );
    session.send("ok").await.expect("successor");
}

#[tokio::test]
async fn successful_controlled_writes_acknowledge_both_boundaries() {
    // Arrange
    for store in stores().await {
        let root = tempfile::tempdir().expect("repository");
        let harness = Harness::new(TestModel(Arc::new(Probe::default())))
            .store(store)
            .repository(repository_with_host_git(root.path()));
        let mut session = harness
            .session("success-write", schema())
            .create()
            .await
            .expect("session");
        let turn = session.send_controlled("write", write_options());
        let control = turn.control();

        // Act
        bounded(turn).await.expect("completed write");
        bounded(control.settled()).await.expect("persistence");
        bounded(control.effects_settled()).await.expect("effects");

        // Assert
        assert_eq!(
            session.writes().await.expect("journal")[0].status,
            WriteStatus::Applied
        );
        session.send("ok").await.expect("successor");
    }
}

#[tokio::test]
async fn host_recovery_survives_cancelled_acquisition_and_terminal_acknowledgments() {
    // Arrange
    for store in crate::store_conformance_test::stores().await {
        for (index, phase) in [
            Phase::Acquire,
            Phase::AcquireAck,
            Phase::Complete,
            Phase::CompleteAck,
        ]
        .into_iter()
        .enumerate()
        {
            let gate = Arc::new(Gate::new(Arc::clone(&store), phase));
            let probe = Arc::new(Probe::default());
            let harness = Harness::new(TestModel(Arc::clone(&probe)))
                .store(gate.clone())
                .execution_identity(
                    ag_harness::ExecutionIdentity::new("model", "1").expect("identity"),
                );
            let mut session = harness
                .session(format!("host-ack-{index}"), schema())
                .create()
                .await
                .expect("session");
            let mut retry = harness.resume(session.id()).await.expect("retry");
            let mut turn = Box::pin(
                session
                    .submit_controlled("host-id", "ok", options())
                    .expect("controlled"),
            );
            let control = turn.control();

            // Act
            tokio::select! {
                result = &mut turn => std::panic::resume_unwind(Box::new(format!("completed before barrier: {result:?}"))),
                () = bounded(gate.entered.notified()) => {},
            }
            if phase != Phase::Acquire {
                let duplicate = retry.submit("host-id", "ok", options()).await;
                if phase == Phase::CompleteAck {
                    assert!(duplicate.is_ok());
                } else {
                    assert!(matches!(
                        duplicate,
                        Err(SessionError::HostTurnInProgress(_))
                    ));
                }
            }
            control.cancel();
            assert!(matches!(
                bounded(turn).await,
                Err(SessionError::Turn(TurnError::Cancelled))
            ));
            unsettled(&control).await;
            gate.release.notify_one();
            bounded(control.settled()).await.expect("settled");
            let record = retry
                .recover("host-id")
                .await
                .expect("recover")
                .expect("record");
            let repeated = retry.submit("host-id", "ok", options()).await;

            // Assert
            if matches!(phase, Phase::Complete | Phase::CompleteAck) {
                assert!(matches!(
                    record.status,
                    ag_harness::HostTurnStatus::Completed(_)
                ));
                assert!(repeated.is_ok());
                assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
            } else {
                assert!(matches!(
                    record.status,
                    ag_harness::HostTurnStatus::Interrupted { .. }
                ));
                assert!(matches!(repeated, Err(SessionError::HostTurnStopped(_))));
                assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[tokio::test]
async fn host_retry_waiting_before_reservation_gets_recorded_classification() {
    // Arrange
    for store in crate::store_conformance_test::stores().await {
        let gate = Arc::new(Gate::new(store, Phase::Acquire));
        let probe = Arc::new(Probe::default());
        let harness = Harness::new(TestModel(Arc::clone(&probe)))
            .store(gate.clone())
            .execution_identity(
                ag_harness::ExecutionIdentity::new("model", "1").expect("identity"),
            );
        let mut first = harness
            .session("queued-host", schema())
            .create()
            .await
            .expect("session");
        let mut retry = harness.resume(first.id()).await.expect("retry");
        let mut turn = Box::pin(
            first
                .submit_controlled("id", "wait", options())
                .expect("turn"),
        );
        let control = turn.control();

        // Act
        tokio::select! {
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = bounded(gate.entered.notified()) => {},
        }
        bounded(gate.lookup.notified()).await;
        let mut duplicate = Box::pin(
            retry
                .submit_controlled("id", "wait", options())
                .expect("retry"),
        );
        tokio::select! {
            result = &mut duplicate => std::panic::resume_unwind(Box::new(format!("unexpected result {result:?}"))),
            () = bounded(gate.lookup.notified()) => {},
        }
        gate.release.notify_one();
        let result = bounded(duplicate).await;
        bounded(probe.entered.notified()).await;
        control.cancel();
        let original = bounded(turn).await;
        bounded(control.settled()).await.expect("settled");

        // Assert
        assert!(matches!(result, Err(SessionError::HostTurnInProgress(_))));
        assert!(matches!(
            original,
            Err(SessionError::Turn(TurnError::Cancelled))
        ));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn host_acquisition_panic_releases_admission_without_execution() {
    // Arrange
    for store in crate::store_conformance_test::stores().await {
        let gate = Arc::new(Gate::new(store, Phase::AcquirePanic));
        let probe = Arc::new(Probe::default());
        let harness = Harness::new(TestModel(Arc::clone(&probe)))
            .store(gate)
            .execution_identity(
                ag_harness::ExecutionIdentity::new("model", "1").expect("identity"),
            );
        let mut session = harness
            .session("panic-host", schema())
            .create()
            .await
            .expect("session");
        let turn = session
            .submit_controlled("id", "ok", options())
            .expect("turn");
        let control = turn.control();

        // Act
        let result = bounded(turn).await;
        bounded(control.settled()).await.expect("settled");
        let recovered = session.recover("id").await.expect("lookup");

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::Store {
                operation: "acquire host request",
                ..
            })
        ));
        assert!(recovered.is_none());
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    }
}
